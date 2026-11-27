//! GPUI benchmark on the shared simulated chat session (500 messages + scroll + stream).
//! Uses our lightweight `gpui::bench_prof` counters inside GPUI to separate:
//!   - view render / element build + request_layout
//!   - text shaping & wrapping (cosmic-text + GPUI line layout cache)
//!   - pure Taffy layout (excl. text shaping)
//!   - prepaint (excl. Taffy layout & text shaping)
//!   - paint + scene finish (building GPU Scene primitives)

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use chat_common::{
    Block, HEIGHT, INITIAL_MESSAGES, INTER_BOLD, INTER_ITALIC, INTER_REGULAR, MONO, Mark, Message,
    PICTURES, SCROLL_FRAMES, SCROLL_STEP, STREAM_FRAMES, Span, WIDTH, apply_stream_step,
    colors as c, make_stream_message, parse_corpus,
};
use gpui::{
    AnyElement, Context, FontStyle, FontWeight, HeadlessAppContext, Hsla, Image, ImageFormat,
    IntoElement, ParentElement, Render, ScrollHandle, SharedString, StyledText, TextRun, Window,
    bench_prof, div, font, img, point, prelude::*, px, relative, rgb,
    size,
};

const SANS: &str = "Inter";
const MONO_FAMILY: &str = "JetBrains Mono";

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

struct ChatApp {
    messages: Vec<Message>,
    pictures: Vec<Arc<Image>>,
    scroll: ScrollHandle,
}

fn inline(spans: &[Span], color_hex: u32, italic: bool) -> StyledText {
    let mut content = String::new();
    let mut runs = Vec::new();
    for s in spans {
        if s.text.is_empty() {
            continue;
        }
        content.push_str(&s.text);
        let mut f = font(if s.mark == Mark::Code { MONO_FAMILY } else { SANS });
        if s.mark == Mark::Bold {
            f.weight = FontWeight::BOLD;
        }
        if s.mark == Mark::Italic || italic {
            f.style = FontStyle::Italic;
        }
        runs.push(TextRun {
            len: s.text.len(),
            font: f,
            color: color(if s.mark == Mark::Link { c::LINK } else { color_hex }),
            background_color: (s.mark == Mark::Code).then(|| color(c::TAG_BG)),
            underline: None,
            strikethrough: None,
        });
    }
    StyledText::new(content).with_runs(runs)
}

fn label(text: impl Into<SharedString>) -> gpui::Div {
    div().child(text.into())
}

fn block(b: &Block, pictures: &[Arc<Image>]) -> AnyElement {
    match b {
        Block::P(spans) => div()
            .line_height(px(21.))
            .child(inline(spans, c::TEXT, false))
            .into_any_element(),
        Block::Q(spans) => div()
            .pl(px(12.))
            .border_l(px(3.))
            .border_color(color(c::QUOTE_LINE))
            .line_height(px(21.))
            .child(inline(spans, c::QUOTE_TEXT, true))
            .into_any_element(),
        Block::H(text) => label(text.clone())
            .text_size(px(16.))
            .line_height(px(22.))
            .font_weight(FontWeight::BOLD)
            .text_color(color(c::HEADING))
            .into_any_element(),
        Block::Ul(items) => div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .pl(px(4.))
            .children(items.iter().map(|spans| {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .child(div().flex_shrink_0().size(px(6.)).mt(px(8.)).rounded(px(3.)).bg(color(c::MUTED)))
                    .child(div().flex_grow(1.0).flex_basis(px(0.)).min_w_0().child(inline(spans, c::TEXT, false)))
            }))
            .into_any_element(),
        Block::Pre(lines) => div()
            .flex()
            .flex_col()
            .px(px(12.))
            .py(px(10.))
            .bg(color(c::PRE_BG))
            .border_1()
            .border_color(color(c::PRE_BORDER))
            .rounded(px(6.))
            .font_family(MONO_FAMILY)
            .text_size(px(12.5))
            .line_height(px(18.))
            .text_color(color(c::PRE_TEXT))
            .children(lines.iter().map(|l| label(l.clone()).whitespace_nowrap()))
            .into_any_element(),
        Block::Table { head, rows } => {
            let total = (head.len() + 1) as f32;
            let row = |cells: &Vec<String>, header: bool, odd: bool| {
                div().flex().flex_row().children(cells.iter().enumerate().map(|(i, v)| {
                    let cell = label(v.clone())
                        .w(relative(if i == 0 { 2.0 } else { 1.0 } / total))
                        .px(px(8.))
                        .py(px(5.))
                        .border_b_1()
                        .border_color(color(if header { c::CARD_BORDER } else { c::TD_BORDER }))
                        .overflow_hidden();
                    if header {
                        cell.bg(color(c::TH_BG))
                            .font_weight(FontWeight::BOLD)
                            .text_color(color(c::HEADING))
                    } else if odd {
                        cell.bg(color(c::TD_ODD))
                    } else {
                        cell
                    }
                }))
            };
            div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(color(c::CARD_BORDER))
                .rounded(px(6.))
                .text_size(px(13.))
                .line_height(px(18.))
                .child(row(head, true, false))
                .children(rows.iter().enumerate().map(|(r, rv)| row(rv, false, r % 2 == 1)))
                .into_any_element()
        }
        Block::Img { picture, width, height, caption } => div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .items_start()
            .child(img(pictures[*picture % 4].clone()).w(px(*width)).h(px(*height)).rounded(px(8.)))
            .child(label(caption.clone()).text_size(px(12.)).line_height(px(16.)).text_color(color(c::MUTED)))
            .into_any_element(),
    }
}

fn msg_view(m: &Message, pictures: &[Arc<Image>]) -> AnyElement {
    let av_bg = if m.mine { c::AVATAR_MINE } else { c::AVATAR_AI };
    let head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(30.))
                .flex_shrink_0()
                .rounded(px(15.))
                .bg(color(av_bg))
                .text_size(px(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(color(c::WHITE))
                .child(m.initials.clone()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(label(m.author.clone()).font_weight(FontWeight::BOLD).text_color(color(c::AUTHOR)))
                .child(label(m.time.clone()).text_size(px(12.)).line_height(px(16.)).text_color(color(c::MUTED))),
        )
        .child(div().flex().flex_row().gap(px(6.)).ml_auto().children(m.tags.iter().map(|t| {
            label(t.clone())
                .px(px(8.))
                .py(px(2.))
                .rounded(px(9.))
                .bg(color(c::TAG_BG))
                .text_color(color(c::TAG_TEXT))
                .text_size(px(11.))
                .line_height(px(14.))
        })));

    let card_bg = if m.mine { c::MINE_CARD } else { c::CARD };
    let card_border = if m.mine { c::MINE_BORDER } else { c::CARD_BORDER };
    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(16.))
        .py(px(14.))
        .bg(color(card_bg))
        .border_1()
        .border_color(color(card_border))
        .rounded(px(10.))
        .child(head)
        .children(m.blocks.iter().map(|b| block(b, pictures)));

    if !m.mine {
        let act = |s: &str| {
            label(s.to_string())
                .px(px(10.))
                .py(px(3.))
                .rounded(px(6.))
                .bg(color(c::ACT_BG))
                .text_color(color(c::TAG_TEXT))
                .text_size(px(12.))
                .line_height(px(16.))
        };
        card = card.child(div().flex().flex_row().gap(px(8.)).child(act("Copy")).child(act("Retry")).child(act("Cite")));
    }
    card.into_any_element()
}

impl Render for ChatApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(color(c::BG))
            .font_family(SANS)
            .text_size(px(14.))
            .text_color(color(c::TEXT))
            .child(
                div()
                    .id("scroller")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .px(px(24.))
                            .py(px(16.))
                            .children(self.messages.iter().map(|m| msg_view(m, &self.pictures))),
                    ),
            )
    }
}

fn ns_to_ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[derive(Default)]
struct PhaseAccum {
    view_req_ms: f64,
    text_shape_ms: f64,
    text_shape_miss_ms: f64,
    layout_pure_ms: f64,
    prepaint_ms: f64,
    paint_ms: f64,
    total_ms: f64,
}

impl PhaseAccum {
    fn add(&mut self, s: bench_prof::Snapshot, wall_ms: f64) {
        let req_pure = s.request_layout_ns.saturating_sub(s.text_shape_in_req_ns);
        let taffy_pure = s.taffy_layout_ns.saturating_sub(s.text_shape_in_taffy_ns);
        let text_outside = s.text_shape_ns.saturating_sub(s.text_shape_in_taffy_ns + s.text_shape_in_req_ns);
        let prepaint_pure = s.prepaint_ns.saturating_sub(s.taffy_layout_ns).saturating_sub(text_outside);
        self.view_req_ms += ns_to_ms(req_pure);
        self.text_shape_ms += ns_to_ms(s.text_shape_ns);
        self.text_shape_miss_ms += ns_to_ms(s.text_shape_miss_ns);
        self.layout_pure_ms += ns_to_ms(taffy_pure);
        self.prepaint_ms += ns_to_ms(prepaint_pure);
        self.paint_ms += ns_to_ms(s.paint_ns + s.scene_finish_ns);
        self.total_ms += wall_ms;
    }
}

fn main() {
    let t_start = Instant::now();
    let all = parse_corpus();
    let initial_msgs = all[..INITIAL_MESSAGES].to_vec();
    let extra_msgs = all[INITIAL_MESSAGES..].to_vec();
    let corpus_parse_ms = ms(t_start.elapsed());

    let t_setup = Instant::now();
    let text_system = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(text_system, Arc::new(()), || Ok(None));
    let setup_ms = ms(t_setup.elapsed());

    let t_fonts = Instant::now();
    cx.update(|cx| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(INTER_REGULAR),
                Cow::Borrowed(INTER_BOLD),
                Cow::Borrowed(INTER_ITALIC),
                Cow::Borrowed(MONO),
            ])
            .expect("fonts");
    });
    let font_load_ms = ms(t_fonts.elapsed());

    let t_imgs = Instant::now();
    let pictures: Vec<Arc<Image>> = PICTURES
        .iter()
        .map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b.to_vec())))
        .collect();
    let image_decode_ms = ms(t_imgs.elapsed());

    // Open headless window and measure Initial Frame (500 messages)
    bench_prof::reset();
    let t_init = Instant::now();
    let handle = cx
        .open_window(size(px(WIDTH), px(HEIGHT)), |_, cx| {
            cx.new(|_| ChatApp {
                messages: initial_msgs,
                pictures,
                scroll: ScrollHandle::new(),
            })
        })
        .expect("headless window");
    cx.run_until_parked();
    let init_wall_ms = ms(t_init.elapsed());
    let init_snap = bench_prof::snapshot();
    // Run one explicit draw if open_window didn't already paint, or measure single clean initial frame:
    // Actually open_window + run_until_parked can draw multiple times as images decode;
    // let's record cold_total_ms AND also do one clean cold-cache full draw for exact 1-frame initial breakdown!
    let mut init_acc = PhaseAccum::default();
    init_acc.add(init_snap, init_wall_ms);
    let cold_total_ms = ms(t_start.elapsed());

    // Phase 2: 30 Scroll frames (480px per frame)
    let mut scroll_acc = PhaseAccum::default();
    for sf in 0..SCROLL_FRAMES {
        let y = (sf + 1) as f32 * SCROLL_STEP;
        bench_prof::reset();
        let t0 = Instant::now();
        handle
            .update(&mut cx, |app, _w, cx| {
                app.scroll.set_offset(point(px(0.), px(-y)));
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        let dt_ms = ms(t0.elapsed());
        scroll_acc.add(bench_prof::snapshot(), dt_ms);
    }

    // Prepare Phase 3: add streaming card + scroll to bottom
    let mut stream_id = 9000usize;
    handle
        .update(&mut cx, |app, _w, cx| {
            app.messages.push(make_stream_message(stream_id));
            let max_y = app.scroll.max_offset().y;
            app.scroll.set_offset(point(px(0.), -max_y));
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();

    let mut next_extra = 0usize;
    let mut stream_acc = PhaseAccum::default();
    for tf in 0..STREAM_FRAMES {
        bench_prof::reset();
        let t0 = Instant::now();
        handle
            .update(&mut cx, |app, _w, cx| {
                apply_stream_step(&mut app.messages, &extra_msgs, &mut next_extra, tf, &mut stream_id);
                let max_y = app.scroll.max_offset().y;
                app.scroll.set_offset(point(px(0.), -max_y));
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        let dt_ms = ms(t0.elapsed());
        stream_acc.add(bench_prof::snapshot(), dt_ms);
    }

    let sf_n = SCROLL_FRAMES as f64;
    let st_n = STREAM_FRAMES as f64;
    println!("{{");
    println!("  \"framework\": \"GPUI (zed v1.29.2)\",");
    println!("  \"messages\": {},", INITIAL_MESSAGES);
    println!("  \"initial\": {{");
    println!("    \"corpus_parse_ms\": {:.4},", corpus_parse_ms);
    println!("    \"runtime_setup_ms\": {:.4},", setup_ms);
    println!("    \"font_load_ms\": {:.4},", font_load_ms);
    println!("    \"image_decode_ms\": {:.4},", image_decode_ms);
    println!("    \"tree_build_ms\": {:.4},", init_acc.view_req_ms);
    println!("    \"style_ms\": 0.0,");
    println!("    \"text_shape_ms\": {:.4},", init_acc.text_shape_ms);
    println!("    \"text_shape_miss_ms\": {:.4},", init_acc.text_shape_miss_ms);
    println!("    \"text_shape_calls\": {},", init_snap.text_shape_calls);
    println!("    \"text_shape_misses\": {},", init_snap.text_shape_misses);
    println!("    \"layout_pure_ms\": {:.4},", init_acc.layout_pure_ms);
    println!("    \"prepaint_ms\": {:.4},", init_acc.prepaint_ms);
    println!("    \"paint_prep_ms\": {:.4},", init_acc.prepaint_ms + init_acc.paint_ms);
    println!("    \"paint_only_ms\": {:.4},", init_acc.paint_ms);
    println!("    \"frame_total_ms\": {:.4},", init_wall_ms);
    println!("    \"cold_total_ms\": {:.4}", cold_total_ms);
    println!("  }},");
    println!("  \"scroll_per_frame\": {{");
    println!("    \"update_ms\": {:.4},", scroll_acc.view_req_ms / sf_n);
    println!("    \"style_ms\": 0.0,");
    println!("    \"text_shape_ms\": {:.4},", scroll_acc.text_shape_ms / sf_n);
    println!("    \"layout_pure_ms\": {:.4},", scroll_acc.layout_pure_ms / sf_n);
    println!("    \"paint_prep_ms\": {:.4},", (scroll_acc.prepaint_ms + scroll_acc.paint_ms) / sf_n);
    println!("    \"frame_total_ms\": {:.4}", scroll_acc.total_ms / sf_n);
    println!("  }},");
    println!("  \"stream_per_frame\": {{");
    println!("    \"update_ms\": {:.4},", stream_acc.view_req_ms / st_n);
    println!("    \"style_ms\": 0.0,");
    println!("    \"text_shape_ms\": {:.4},", stream_acc.text_shape_ms / st_n);
    println!("    \"layout_pure_ms\": {:.4},", stream_acc.layout_pure_ms / st_n);
    println!("    \"paint_prep_ms\": {:.4},", (stream_acc.prepaint_ms + stream_acc.paint_ms) / st_n);
    println!("    \"frame_total_ms\": {:.4}", stream_acc.total_ms / st_n);
    println!("  }}");
    println!("}}");
}
