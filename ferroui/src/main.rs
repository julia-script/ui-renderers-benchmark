//! FerroUI benchmark on the shared simulated chat session (1000 messages + scroll + stream).
//! Uses `ferroui_base::bench_prof` counters inside ferroui-base to separate:
//!   - view / control tree build & reconciliation
//!   - style application (Fluent theme / StyledElement::try_apply_styling)
//!   - text shaping (HarfBuzz via TextShaper::shape_text)
//!   - pure layout (LayoutManager::execute_layout_pass minus nested style & shaping)
//!   - paint prep (CompositingRenderer::update_core draw list recording)
//!   - Skia CPU rasterization (ServerCompositionTarget::render)

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use chat_common::{
    Block, HEIGHT, INITIAL_MESSAGES, INTER_BOLD, INTER_ITALIC, INTER_REGULAR, MONO, Mark, Message,
    PICTURES, SCROLL_FRAMES, SCROLL_STEP, STREAM_FRAMES, Span, WIDTH, apply_stream_step,
    colors as c, make_stream_message, parse_corpus,
};
use ferroui_base::bench_prof;
use ferroui_base::layout::{HorizontalAlignment, Orientation, VerticalAlignment};
use ferroui_base::media::fonts::EmbeddedFontCollection;
use ferroui_base::media::imaging::Bitmap;
use ferroui_base::media::{Brush, FontFamily, FontStyle, FontWeight, IBrush, IImage, TextWrapping};
use ferroui_base::platform::register_assets;
use ferroui_base::threading::Dispatcher;
use ferroui_base::utilities::UriRef;
use ferroui_base::{Ref, Vector};
use ferroui_controls::documents::Run;
use ferroui_controls::{AppBuilder, Application, ScrollViewer, Window};
use ferroui_headless::{FerroHeadlessPlatform, FerroHeadlessPlatformExtensions, FerroHeadlessPlatformOptions};
use ferroui_skia::SkiaApplicationExtensions;
use ferroui_themes_fluent::FluentTheme;
use ferroui_view::prelude::*;

#[cfg(target_os = "macos")]
mod windowed;

const SANS: &str = "fonts:XBench#Inter";
const MONO_FAMILY: &str = "fonts:XBench#JetBrains Mono";

thread_local! {
    static MESSAGES: RefCell<Vec<Message>> = const { RefCell::new(Vec::new()) };
    static PICTURE_BITMAPS: RefCell<Vec<Rc<dyn IImage>>> = const { RefCell::new(Vec::new()) };
    static BRUSHES: RefCell<HashMap<u32, Rc<dyn IBrush>>> = RefCell::new(HashMap::new());
}

fn brush(v: u32) -> Rc<dyn IBrush> {
    BRUSHES.with(|b| {
        b.borrow_mut()
            .entry(v)
            .or_insert_with(|| Brush::parse(&format!("#{v:06x}")).expect("color"))
            .clone()
    })
}

fn hex(v: u32) -> Option<Rc<dyn IBrush>> {
    Some(brush(v))
}

fn family(name: &str) -> FontFamily {
    FontFamily::parse(name).expect("family")
}

fn inline(spans: &[Span], size: f64, line: f64, color: u32, italic: bool) -> Element {
    // The runs go through the `inlines` property (not a mount-only `.with` hook), so a re-rendered
    // message (streamed words) updates the TextBlock it patches.
    let inlines = ferroui_controls::documents::InlineCollection::new();
    for s in spans {
        let run = Run::with_text(Some(&s.text));
        match s.mark {
            Mark::None => {}
            Mark::Bold => run.set_font_weight(FontWeight::Bold),
            Mark::Italic => run.set_font_style(FontStyle::Italic),
            Mark::Code => {
                run.set_font_family(family(MONO_FAMILY));
                run.set_font_size(12.5);
                run.set_background(Some(brush(c::TAG_BG)));
            }
            Mark::Link => run.set_foreground(Some(brush(c::LINK))),
        }
        inlines.add(run);
    }
    text_block()
        .text_wrapping(TextWrapping::Wrap)
        .font_size(size)
        .line_height(line)
        .foreground(hex(color))
        .font_style(if italic { FontStyle::Italic } else { FontStyle::Normal })
        .inlines(Some(inlines))
        .into()
}

fn label(text: &str, size: f64, line: f64, weight: FontWeight, color: u32) -> Element {
    text_block()
        .text(text.to_string())
        .font_size(size)
        .line_height(line)
        .font_weight(weight)
        .foreground(hex(color))
        .into()
}

fn block_view(b: &Block) -> Element {
    match b {
        Block::P(spans) => inline(spans, 14.0, 21.0, c::TEXT, false),
        Block::Q(spans) => border()
            .border_brush(hex(c::QUOTE_LINE))
            .border_thickness((3.0, 0.0, 0.0, 0.0))
            .padding((12.0, 0.0, 0.0, 0.0))
            .child(inline(spans, 14.0, 21.0, c::QUOTE_TEXT, true))
            .into(),
        Block::H(text) => text_block()
            .text(text.clone())
            .font_size(16.0)
            .line_height(22.0)
            .font_weight(FontWeight::Bold)
            .text_wrapping(TextWrapping::Wrap)
            .foreground(hex(c::HEADING))
            .into(),
        Block::Ul(items) => stack_panel()
            .spacing(4.0)
            .margin((4.0, 0.0, 0.0, 0.0))
            .children(
                items
                    .iter()
                    .map(|spans| -> Element {
                        grid()
                            .column_definitions("Auto,*")
                            .child(
                                border()
                                    .width(6.0)
                                    .height(6.0)
                                    .corner_radius(3.0)
                                    .background(hex(c::MUTED))
                                    .margin((0.0, 8.0, 8.0, 0.0))
                                    .vertical_alignment(VerticalAlignment::Top),
                            )
                            .child(border().grid_column(1).child(inline(spans, 14.0, 20.0, c::TEXT, false)))
                            .into()
                    })
                    .collect::<Vec<_>>(),
            )
            .into(),
        Block::Pre(lines) => border()
            .background(hex(c::PRE_BG))
            .border_brush(hex(c::PRE_BORDER))
            .border_thickness(1.0)
            .corner_radius(6.0)
            .padding((12.0, 10.0))
            .child(
                stack_panel().children(
                    lines
                        .iter()
                        .map(|l| -> Element {
                            text_block()
                                .text(l.clone())
                                .font_family(family(MONO_FAMILY))
                                .font_size(12.5)
                                .line_height(18.0)
                                .foreground(hex(c::PRE_TEXT))
                                .into()
                        })
                        .collect::<Vec<_>>(),
                ),
            )
            .into(),
        Block::Table { head, rows } => {
            let columns = if head.len() == 3 { "2*,*,*" } else { "2*,*,*,*" };
            let rows_def = vec!["Auto"; rows.len() + 1].join(",");
            let mut cells: Vec<Element> = Vec::new();
            for (r, row) in std::iter::once(head).chain(rows.iter()).enumerate() {
                let header = r == 0;
                let odd = r > 0 && ((r - 1) % 2 == 1);
                for (i, v) in row.iter().enumerate() {
                    let mut cell = border()
                        .grid_row(r as i32)
                        .grid_column(i as i32)
                        .padding((8.0, 5.0))
                        .border_brush(hex(if header { c::CARD_BORDER } else { c::TD_BORDER }))
                        .border_thickness((0.0, 0.0, 0.0, 1.0));
                    if header {
                        cell = cell.background(hex(c::TH_BG));
                    } else if odd {
                        cell = cell.background(hex(c::TD_ODD));
                    }
                    cells.push(
                        cell.child(
                            text_block()
                                .text(v.clone())
                                .font_size(13.0)
                                .line_height(18.0)
                                .font_weight(if header { FontWeight::Bold } else { FontWeight::Normal })
                                .foreground(hex(if header { c::HEADING } else { c::TEXT })),
                        )
                        .into(),
                    );
                }
            }
            border()
                .border_brush(hex(c::CARD_BORDER))
                .border_thickness(1.0)
                .corner_radius(6.0)
                .child(grid().column_definitions(columns).row_definitions(rows_def.as_str()).children(cells))
                .into()
        }
        Block::Img { picture, width, height, caption } => {
            let bitmap = PICTURE_BITMAPS.with(|p| p.borrow()[*picture % 4].clone());
            stack_panel()
                .spacing(6.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .child(
                    border()
                        .width(*width as f64)
                        .height(*height as f64)
                        .corner_radius(8.0)
                        .clip_to_bounds(true)
                        .child(image().source(Some(bitmap)).width(*width as f64).height(*height as f64)),
                )
                .child(
                    text_block()
                        .text(caption.clone())
                        .font_size(12.0)
                        .line_height(16.0)
                        .text_wrapping(TextWrapping::Wrap)
                        .foreground(hex(c::MUTED)),
                )
                .into()
        }
    }
}

fn msg_body(m: &Message) -> Element {
    let av_bg = if m.mine { c::AVATAR_MINE } else { c::AVATAR_AI };
    let head = grid()
        .column_definitions("Auto,*,Auto")
        .child(
            border()
                .width(30.0)
                .height(30.0)
                .corner_radius(15.0)
                .background(hex(av_bg))
                .vertical_alignment(VerticalAlignment::Center)
                .child(
                    text_block()
                        .text(m.initials.clone())
                        .font_size(12.0)
                        .font_weight(FontWeight::Bold)
                        .foreground(hex(c::WHITE))
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center),
                ),
        )
        .child(
            stack_panel()
                .grid_column(1)
                .margin((10.0, 0.0, 10.0, 0.0))
                .vertical_alignment(VerticalAlignment::Center)
                .child(label(&m.author, 14.0, 18.0, FontWeight::Bold, c::AUTHOR))
                .child(label(&m.time, 12.0, 16.0, FontWeight::Normal, c::MUTED)),
        )
        .child(
            stack_panel()
                .grid_column(2)
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children(
                    m.tags
                        .iter()
                        .map(|t| -> Element {
                            border()
                                .background(hex(c::TAG_BG))
                                .corner_radius(9.0)
                                .padding((8.0, 2.0))
                                .child(label(t, 11.0, 14.0, FontWeight::Normal, c::TAG_TEXT))
                                .into()
                        })
                        .collect::<Vec<_>>(),
                ),
        );

    let mut children = Vec::with_capacity(m.blocks.len() + 2);
    children.push(head.into());
    for b in &m.blocks {
        children.push(block_view(b));
    }
    if !m.mine {
        let act = |s: &str| -> Element {
            border()
                .background(hex(c::ACT_BG))
                .corner_radius(6.0)
                .padding((10.0, 3.0))
                .child(label(s, 12.0, 16.0, FontWeight::Normal, c::TAG_TEXT))
                .into()
        };
        children.push(
            stack_panel()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .child(act("Copy"))
                .child(act("Retry"))
                .child(act("Cite"))
                .into(),
        );
    }

    let card_bg = if m.mine { c::MINE_CARD } else { c::CARD };
    let card_border = if m.mine { c::MINE_BORDER } else { c::CARD_BORDER };
    border()
        .background(hex(card_bg))
        .border_brush(hex(card_border))
        .border_thickness(1.0)
        .corner_radius(10.0)
        .padding((16.0, 14.0))
        .child(stack_panel().spacing(10.0).children(children))
        .into()
}

// `rerender`: memoized on (at, id, version), so only a message whose version changed runs again.
#[component(rerender)]
fn MsgView(at: usize, id: usize, version: u64) -> Element {
    let _ = (id, version);
    MESSAGES.with(|ms| msg_body(&ms.borrow()[at]))
}

#[component(rerender)]
fn Page(tick: Signal<u64>, scroller: NodeRef<ScrollViewer>) -> Element {
    let _ = tick.get();
    let keys: Vec<(usize, u64)> = MESSAGES.with(|ms| ms.borrow().iter().map(|m| (m.id, m.version)).collect());
    let content = stack_panel().spacing(12.0).margin((24.0, 16.0)).children(
        keys.into_iter()
            .enumerate()
            .map(|(at, (id, ver))| msg_view().key(id).at(at).id(id).version(ver).into_element())
            .collect::<Vec<_>>(),
    );
    border()
        .background(hex(c::BG))
        .child(
            scroll_viewer()
                .node_ref(scroller)
                .horizontal_scroll_bar_visibility(ferroui_controls::primitives::ScrollBarVisibility::Disabled)
                .child(content),
        )
        .into()
}

fn ns_to_ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    if std::env::args().any(|a| a == "--windowed") {
        #[cfg(target_os = "macos")]
        windowed::run();
        #[cfg(not(target_os = "macos"))]
        panic!("--windowed is implemented for macOS only");
        #[allow(unreachable_code)]
        return;
    }
    run_headless();
}

fn run_headless() {
    let t_start = Instant::now();
    let all = parse_corpus();
    let initial_msgs = all[..INITIAL_MESSAGES].to_vec();
    let extra_msgs = all[INITIAL_MESSAGES..].to_vec();
    MESSAGES.with(|ms| *ms.borrow_mut() = initial_msgs);
    let corpus_parse_ms = ms(t_start.elapsed());

    let t_setup = Instant::now();
    register_assets(
        "XBench",
        &[
            ("/Assets/Inter-Regular.ttf", INTER_REGULAR),
            ("/Assets/Inter-Bold.ttf", INTER_BOLD),
            ("/Assets/Inter-Italic.ttf", INTER_ITALIC),
            ("/Assets/JetBrainsMono-Regular.ttf", MONO),
        ],
    );
    AppBuilder::configure::<Application>()
        .use_skia()
        .use_headless(FerroHeadlessPlatformOptions {
            use_headless_drawing: false,
            ..FerroHeadlessPlatformOptions::default()
        })
        .configure_fonts(|fm| {
            fm.add_font_collection(Rc::new(EmbeddedFontCollection::new(
                UriRef::parse_absolute("fonts:XBench").expect("key"),
                UriRef::parse_absolute("ferres://XBench/Assets").expect("source"),
            )));
        })
        .setup_without_starting();
    let setup_ms = ms(t_setup.elapsed());

    let t_theme = Instant::now();
    Application::current().expect("app").styles().add(FluentTheme::new().as_style());
    let theme_ms = ms(t_theme.elapsed());

    let t_imgs = Instant::now();
    PICTURE_BITMAPS.with(|p| {
        *p.borrow_mut() = PICTURES
            .iter()
            .map(|bytes| Rc::new(Bitmap::from_stream(&mut &bytes[..]).expect("png")) as Rc<dyn IImage>)
            .collect();
    });
    let image_decode_ms = ms(t_imgs.elapsed());

    let tick = signal(0u64);
    let scroller = NodeRef::<ScrollViewer>::new();

    bench_prof::reset();
    let t_build = Instant::now();
    let win: Ref<Window> = mount_window(|| {
        window()
            .title("chat-bench")
            .width(WIDTH as f64)
            .height(HEIGHT as f64)
            .can_resize(false)
            .font_family(family(SANS))
            .child(page().tick(tick).scroller(scroller))
            .into()
    });
    let tree_build_ms = ms(t_build.elapsed());
    let build_snap = bench_prof::snapshot();

    // Show window, run initial layout + style + text shaping + render + Skia raster
    bench_prof::reset();
    let t_init = Instant::now();
    win.show();
    Dispatcher::ui_thread().run_jobs(None);
    FerroHeadlessPlatform::force_render_timer_tick(1);
    Dispatcher::ui_thread().run_jobs(None);
    let init_frame_ms = ms(t_init.elapsed());
    let init_snap = bench_prof::snapshot();
    let cold_total_ms = ms(t_start.elapsed());

    let init_style_ns = build_snap.style_ns + init_snap.style_ns;
    let init_shape_ns = build_snap.text_shape_ns + init_snap.text_shape_ns;
    let init_layout_pure_ns = init_snap
        .layout_total_ns
        .saturating_sub(init_snap.text_shape_in_layout_ns)
        .saturating_sub(init_snap.style_in_layout_ns);
    let init_paint_prep_ns = init_snap.paint_prep_ns.saturating_sub(init_snap.text_shape_in_paint_ns);
    let init_raster_ns = init_snap.raster_ns;

    // Phase 2: 30 Scroll frames (480px per frame)
    let mut scroll_update_ms = 0.0f64;
    let mut scroll_total_ms = 0.0f64;
    bench_prof::reset();
    for sf in 0..SCROLL_FRAMES {
        let y = ((sf + 1) as f64) * (SCROLL_STEP as f64);
        let t0 = Instant::now();
        if let Some(s) = scroller.get() {
            let max = (s.extent().height - s.viewport().height).max(0.0);
            s.set_offset(Vector::new(0.0, y.min(max)));
        }
        let t1 = Instant::now();
        Dispatcher::ui_thread().run_jobs(None);
        FerroHeadlessPlatform::force_render_timer_tick(1);
        Dispatcher::ui_thread().run_jobs(None);
        let t2 = Instant::now();
        scroll_update_ms += ms(t1 - t0);
        scroll_total_ms += ms(t2 - t0);
    }
    let scroll_snap = bench_prof::snapshot();

    // Prepare Phase 3: add streaming card + scroll to bottom
    let mut tick_val = 0u64;
    let mut stream_id = 9000usize;
    MESSAGES.with(|ms| ms.borrow_mut().push(make_stream_message(stream_id)));
    tick_val += 1;
    tick.set(tick_val);
    Dispatcher::ui_thread().run_jobs(None);
    if let Some(s) = scroller.get() {
        let max = (s.extent().height - s.viewport().height).max(0.0);
        s.set_offset(Vector::new(0.0, max));
    }
    Dispatcher::ui_thread().run_jobs(None);
    FerroHeadlessPlatform::force_render_timer_tick(1);
    Dispatcher::ui_thread().run_jobs(None);

    let mut next_extra = 0usize;
    let mut stream_total_ms = 0.0f64;
    let mut stream_mut_ms = 0.0f64;
    bench_prof::reset();
    for tf in 0..STREAM_FRAMES {
        let t0 = Instant::now();
        MESSAGES.with(|ms| {
            apply_stream_step(&mut ms.borrow_mut(), &extra_msgs, &mut next_extra, tf, &mut stream_id);
        });
        tick_val += 1;
        tick.set(tick_val);
        let t1 = Instant::now();
        Dispatcher::ui_thread().run_jobs(None);
        if let Some(s) = scroller.get() {
            let max = (s.extent().height - s.viewport().height).max(0.0);
            s.set_offset(Vector::new(0.0, max));
        }
        Dispatcher::ui_thread().run_jobs(None);
        FerroHeadlessPlatform::force_render_timer_tick(1);
        Dispatcher::ui_thread().run_jobs(None);
        let t2 = Instant::now();
        stream_mut_ms += ms(t1 - t0);
        stream_total_ms += ms(t2 - t0);
    }
    let stream_snap = bench_prof::snapshot();

    let sf_n = SCROLL_FRAMES as f64;
    let st_n = STREAM_FRAMES as f64;

    let scroll_layout_pure = scroll_snap
        .layout_total_ns
        .saturating_sub(scroll_snap.text_shape_in_layout_ns)
        .saturating_sub(scroll_snap.style_in_layout_ns);
    let scroll_paint_prep = scroll_snap.paint_prep_ns.saturating_sub(scroll_snap.text_shape_in_paint_ns);

    let stream_layout_pure = stream_snap
        .layout_total_ns
        .saturating_sub(stream_snap.text_shape_in_layout_ns)
        .saturating_sub(stream_snap.style_in_layout_ns);
    let stream_paint_prep = stream_snap.paint_prep_ns.saturating_sub(stream_snap.text_shape_in_paint_ns);
    let stream_reconcile_ms = (stream_total_ms
        - ns_to_ms(stream_snap.style_ns + stream_snap.text_shape_ns + stream_layout_pure + stream_paint_prep + stream_snap.raster_ns))
    .max(stream_mut_ms);

    println!("{{");
    println!("  \"framework\": \"FerroUI (0.5.0 + Skia)\",");
    println!("  \"messages\": {},", INITIAL_MESSAGES);
    println!("  \"initial\": {{");
    println!("    \"corpus_parse_ms\": {:.4},", corpus_parse_ms);
    println!("    \"runtime_setup_ms\": {:.4},", setup_ms);
    println!("    \"theme_setup_ms\": {:.4},", theme_ms);
    println!("    \"image_decode_ms\": {:.4},", image_decode_ms);
    println!("    \"tree_build_ms\": {:.4},", (tree_build_ms - ns_to_ms(build_snap.style_ns)).max(0.0));
    println!("    \"style_ms\": {:.4},", ns_to_ms(init_style_ns));
    println!("    \"text_shape_ms\": {:.4},", ns_to_ms(init_shape_ns));
    println!("    \"text_shape_calls\": {},", build_snap.text_shape_calls + init_snap.text_shape_calls);
    println!("    \"layout_total_ms\": {:.4},", ns_to_ms(init_snap.layout_total_ns));
    println!("    \"layout_pure_ms\": {:.4},", ns_to_ms(init_layout_pure_ns));
    println!("    \"paint_prep_ms\": {:.4},", ns_to_ms(init_paint_prep_ns));
    println!("    \"raster_ms\": {:.4},", ns_to_ms(init_raster_ns));
    println!("    \"frame_total_ms\": {:.4},", init_frame_ms);
    println!("    \"cold_total_ms\": {:.4}", cold_total_ms);
    println!("  }},");
    println!("  \"scroll_per_frame\": {{");
    println!("    \"update_ms\": {:.4},", scroll_update_ms / sf_n);
    println!("    \"style_ms\": {:.4},", ns_to_ms(scroll_snap.style_ns) / sf_n);
    println!("    \"text_shape_ms\": {:.4},", ns_to_ms(scroll_snap.text_shape_ns) / sf_n);
    println!("    \"layout_pure_ms\": {:.4},", ns_to_ms(scroll_layout_pure) / sf_n);
    println!("    \"paint_prep_ms\": {:.4},", ns_to_ms(scroll_paint_prep) / sf_n);
    println!("    \"raster_ms\": {:.4},", ns_to_ms(scroll_snap.raster_ns) / sf_n);
    println!("    \"frame_total_ms\": {:.4}", scroll_total_ms / sf_n);
    println!("  }},");
    println!("  \"stream_per_frame\": {{");
    println!("    \"update_ms\": {:.4},", stream_reconcile_ms / st_n);
    println!("    \"style_ms\": {:.4},", ns_to_ms(stream_snap.style_ns) / st_n);
    println!("    \"text_shape_ms\": {:.4},", ns_to_ms(stream_snap.text_shape_ns) / st_n);
    println!("    \"layout_pure_ms\": {:.4},", ns_to_ms(stream_layout_pure) / st_n);
    println!("    \"paint_prep_ms\": {:.4},", ns_to_ms(stream_paint_prep) / st_n);
    println!("    \"raster_ms\": {:.4},", ns_to_ms(stream_snap.raster_ns) / st_n);
    println!("    \"frame_total_ms\": {:.4}", stream_total_ms / st_n);
    println!("  }}");
    println!("}}");
    std::process::exit(0);
}
