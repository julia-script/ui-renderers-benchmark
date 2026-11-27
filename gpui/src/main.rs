//! GPUI benchmark on the shared simulated chat session (1000 messages + scroll + stream).
//! Uses our lightweight `gpui::bench_prof` counters inside GPUI to separate:
//!   - view render / element build + request_layout
//!   - text shaping & wrapping (cosmic-text + GPUI line layout cache)
//!   - pure Taffy layout (excl. text shaping)
//!   - prepaint (excl. Taffy layout & text shaping)
//!   - paint + scene finish (building GPU Scene primitives)
//!
//! Default: `HeadlessAppContext` (full Scene, no GPU). `--windowed`: real macOS `Application`,
//! visible window, production Metal renderer drawing + presenting every frame (see `windowed`).

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use chat_common::{
    Block, HEIGHT, INITIAL_MESSAGES, INTER_BOLD, INTER_ITALIC, INTER_REGULAR, MONO, Mark, Message,
    PICTURES, SCROLL_FRAMES, SCROLL_STEP, STREAM_FRAMES, Span, WIDTH, apply_stream_step,
    colors as c, make_stream_message, parse_corpus,
};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FontStyle, FontWeight, HeadlessAppContext, Hsla,
    Image, ImageFormat, IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    StyledText, TextRun, Window, WindowBounds, WindowOptions, bench_prof, div, font, img, point,
    prelude::*, px, relative, rgb, size,
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

fn run_headless() {
    let t_start = Instant::now();
    let all = parse_corpus();
    let initial_msgs = all[..INITIAL_MESSAGES].to_vec();
    let extra_msgs = all[INITIAL_MESSAGES..].to_vec();
    let corpus_parse_ms = ms(t_start.elapsed());

    let t_setup = Instant::now();
    // Real CoreText/font-kit text system (gpui_platform `font-kit` feature). Without it the
    // Mac platform falls back to NoopTextSystem, which never shapes text.
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

    // Open headless window and measure Initial Frame (1000 messages)
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

fn main() {
    if std::env::args().any(|a| a == "--windowed") {
        windowed::run();
    } else {
        run_headless();
    }
}

/// `--windowed`: real `Application` on the macOS platform, a visible 1040x720 pt window at the
/// display's backing scale, and the production Metal renderer drawing + presenting through
/// GPUI's `CAMetalDisplayLink` frame source.
///
/// Frames are driven from `Window::on_next_frame`: GPUI runs next-frame callbacks at the start
/// of a display-link update, *before* deciding whether to draw, so a callback that mutates the
/// model and notifies is drawn and presented in that same update. The driver only starts the
/// next frame once the previous one's command buffer completed (Metal completed handler) and its
/// drawable reported presentation (presented handler), and one further update passed without
/// another draw. Per-frame wall time is taken from Metal's host-clock timestamps, so the extra
/// polling ticks never inflate the numbers.
mod windowed {
    use super::*;

    const MAX_WAIT_TICKS: u32 = 600;

    #[derive(Default)]
    struct GpuAccum {
        phases: PhaseAccum,
        frames: u32,
        draws: u64,
        dropped: u64,
        gpu_encode_ms: f64,
        drawable_wait_ms: f64,
        gpu_exec_ms: f64,
        present_ms: f64,
        present_n: u32,
        frame_to_present_ms: f64,
        cpu_frame_ms: f64,
        first_frame_ms: f64,
        snap: bench_prof::Snapshot,
    }

    #[derive(Clone, Copy)]
    enum Phase {
        Initial,
        Scroll(usize),
        StreamPrep,
        Stream(usize),
    }

    struct Pending {
        host_t0: f64,
        committed0: u64,
        dropped0: u64,
        last_committed: u64,
        ticks: u32,
    }

    struct Driver {
        app: Entity<ChatApp>,
        extra_msgs: Vec<Message>,
        next_extra: usize,
        stream_id: usize,
        phase: Phase,
        pending: Pending,
        host_start: f64,
        corpus_parse_ms: f64,
        setup_ms: f64,
        font_load_ms: f64,
        image_decode_ms: f64,
        init: GpuAccum,
        scroll: GpuAccum,
        stream: GpuAccum,
        cold_total_ms: f64,
    }

    impl Pending {
        fn start(host_t0: f64) -> Self {
            bench_prof::reset();
            let s = bench_prof::snapshot();
            let c = s.frames_committed;
            Self { host_t0, committed0: c, dropped0: s.frames_dropped, last_committed: c, ticks: 0 }
        }

        /// True once at least one frame was committed since `start`, every committed command
        /// buffer completed, every drawable reported presentation, and a whole display-link
        /// update passed without a new commit.
        fn settled(&mut self, s: &bench_prof::Snapshot) -> bool {
            self.ticks += 1;
            if self.ticks > MAX_WAIT_TICKS {
                panic!(
                    "frame did not settle: committed {} -> {}, completed {}, presented {}",
                    self.committed0, s.frames_committed, s.frames_completed, s.frames_presented
                );
            }
            let quiet = s.frames_committed == self.last_committed;
            self.last_committed = s.frames_committed;
            s.frames_committed > self.committed0
                && s.frames_completed >= s.frames_committed
                && s.frames_presented >= s.frames_committed
                && quiet
        }
    }

    impl GpuAccum {
        fn add(&mut self, p: &Pending, s: bench_prof::Snapshot) {
            let frame_total_ms = (s.last_gpu_end_s - p.host_t0) * 1000.0;
            self.phases.add(s, frame_total_ms);
            self.frames += 1;
            self.draws += s.frames_committed - p.committed0;
            self.dropped += s.frames_dropped - p.dropped0;
            self.gpu_encode_ms += ns_to_ms(s.gpu_encode_ns);
            self.drawable_wait_ms += ns_to_ms(s.drawable_wait_ns);
            self.gpu_exec_ms += ns_to_ms(s.gpu_exec_ns);
            self.cpu_frame_ms += (s.last_commit_s - p.host_t0) * 1000.0;
            self.first_frame_ms += (s.first_gpu_end_s - p.host_t0) * 1000.0;
            if s.last_presented_s > 0.0 && s.last_presented_s >= p.host_t0 {
                self.present_ms += (s.last_presented_s - s.last_gpu_end_s).max(0.0) * 1000.0;
                self.frame_to_present_ms += (s.last_presented_s - p.host_t0) * 1000.0;
                self.present_n += 1;
            }
            self.snap = s;
        }

        fn mean(&self, v: f64) -> f64 {
            v / self.frames.max(1) as f64
        }

        fn present_mean(&self, v: f64) -> f64 {
            v / self.present_n.max(1) as f64
        }
    }

    fn tick(driver: Rc<RefCell<Driver>>, window: &mut Window, cx: &mut App) {
        let snap = bench_prof::snapshot();
        let done = {
            let mut d = driver.borrow_mut();
            if !d.pending.settled(&snap) {
                false
            } else {
                let d = &mut *d;
                match d.phase {
                    Phase::Initial => {
                        d.init.add(&d.pending, snap);
                        d.cold_total_ms = (snap.last_gpu_end_s - d.host_start) * 1000.0;
                    }
                    Phase::Scroll(_) => d.scroll.add(&d.pending, snap),
                    Phase::StreamPrep => {}
                    Phase::Stream(_) => d.stream.add(&d.pending, snap),
                }
                let next = match d.phase {
                    Phase::Initial => Some(Phase::Scroll(0)),
                    Phase::Scroll(i) if i + 1 < SCROLL_FRAMES => Some(Phase::Scroll(i + 1)),
                    Phase::Scroll(_) => Some(Phase::StreamPrep),
                    Phase::StreamPrep => Some(Phase::Stream(0)),
                    Phase::Stream(i) if i + 1 < STREAM_FRAMES => Some(Phase::Stream(i + 1)),
                    Phase::Stream(_) => None,
                };
                match next {
                    None => true,
                    Some(phase) => {
                        d.phase = phase;
                        d.pending = Pending::start(bench_prof::host_time_s());
                        false
                    }
                }
            }
        };
        if done {
            print_results(&driver.borrow(), window.scale_factor(), window.viewport_size());
            // XB_HOLD_MS: keep the last presented frame on screen (for screenshots) before quit.
            let hold_ms = std::env::var("XB_HOLD_MS").ok().and_then(|v| v.parse::<u64>().ok());
            match hold_ms {
                Some(hold) if hold > 0 => {
                    let timer = cx
                        .background_executor()
                        .timer(std::time::Duration::from_millis(hold));
                    cx.spawn(async move |cx| {
                        timer.await;
                        cx.update(|cx| cx.quit());
                    })
                    .detach();
                }
                _ => cx.quit(),
            }
            return;
        }
        // Start the frame (if a new one began above): mutate + notify inside this update so
        // GPUI draws and presents it right after the next-frame callbacks.
        if driver.borrow().pending.ticks == 0 {
            let mut d = driver.borrow_mut();
            let d = &mut *d;
            let phase = d.phase;
            let (extra, next_extra, stream_id) = (&d.extra_msgs, &mut d.next_extra, &mut d.stream_id);
            d.app.update(cx, |app, cx| {
                match phase {
                    Phase::Initial => {}
                    Phase::Scroll(sf) => {
                        let y = (sf + 1) as f32 * SCROLL_STEP;
                        app.scroll.set_offset(point(px(0.), px(-y)));
                    }
                    Phase::StreamPrep => {
                        *stream_id = 9000;
                        app.messages.push(make_stream_message(*stream_id));
                        let max_y = app.scroll.max_offset().y;
                        app.scroll.set_offset(point(px(0.), -max_y));
                    }
                    Phase::Stream(tf) => {
                        apply_stream_step(&mut app.messages, extra, next_extra, tf, stream_id);
                        let max_y = app.scroll.max_offset().y;
                        app.scroll.set_offset(point(px(0.), -max_y));
                    }
                }
                cx.notify();
            });
        }
        window.on_next_frame(move |window, cx| tick(driver, window, cx));
    }

    /// Forwards GPUI/Metal warnings and errors to stderr (stdout stays pure JSON).
    struct StderrLogger;
    impl log::Log for StderrLogger {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Warn
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
            }
        }
        fn flush(&self) {}
    }

    pub fn run() {
        if log::set_logger(&StderrLogger).is_ok() {
            log::set_max_level(log::LevelFilter::Warn);
        }
        let host_start = bench_prof::host_time_s();
        let t_start = Instant::now();
        let all = parse_corpus();
        let initial_msgs = all[..INITIAL_MESSAGES].to_vec();
        let extra_msgs = all[INITIAL_MESSAGES..].to_vec();
        let corpus_parse_ms = ms(t_start.elapsed());

        let t_setup = Instant::now();
        let application = gpui_platform::application();
        application.run(move |cx: &mut App| {
            let setup_ms = ms(t_setup.elapsed());

            let t_fonts = Instant::now();
            cx.text_system()
                .add_fonts(vec![
                    Cow::Borrowed(INTER_REGULAR),
                    Cow::Borrowed(INTER_BOLD),
                    Cow::Borrowed(INTER_ITALIC),
                    Cow::Borrowed(MONO),
                ])
                .expect("fonts");
            let font_load_ms = ms(t_fonts.elapsed());

            let t_imgs = Instant::now();
            let pictures: Vec<Arc<Image>> = PICTURES
                .iter()
                .map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b.to_vec())))
                .collect();
            let image_decode_ms = ms(t_imgs.elapsed());

            cx.activate(true);
            let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                show: true,
                // Never throttle to the inactive-window rate if focus is stolen mid-run;
                // vsync pacing and low-power/thermal policies stay at production defaults.
                inactive_frame_interval: None,
                ..Default::default()
            };

            // Initial frame: open_window (first synchronous draw) through GPU completion and
            // presentation of the last frame drawn before the window goes idle.
            let host_t0 = bench_prof::host_time_s();
            let pending = Pending::start(host_t0);
            let mut entity = None;
            let handle = cx
                .open_window(options, |_, cx| {
                    let e = cx.new(|_| ChatApp {
                        messages: initial_msgs,
                        pictures,
                        scroll: ScrollHandle::new(),
                    });
                    entity = Some(e.clone());
                    e
                })
                .expect("window");
            let driver = Rc::new(RefCell::new(Driver {
                app: entity.expect("root view"),
                extra_msgs,
                next_extra: 0,
                stream_id: 9000,
                phase: Phase::Initial,
                pending,
                host_start,
                corpus_parse_ms,
                setup_ms,
                font_load_ms,
                image_decode_ms,
                init: GpuAccum::default(),
                scroll: GpuAccum::default(),
                stream: GpuAccum::default(),
                cold_total_ms: 0.0,
            }));
            // ticks = 1 so the first callback doesn't re-run the (no-op) initial mutation.
            driver.borrow_mut().pending.ticks = 1;
            handle
                .update(cx, |_, window, _| {
                    window.on_next_frame(move |window, cx| tick(driver, window, cx));
                })
                .expect("window update");
        });
    }

    fn print_results(d: &Driver, scale: f32, viewport: gpui::Size<gpui::Pixels>) {
        use std::io::Write as _;
        let i = &d.init;
        let init_snap = i.snap;
        let sf = &d.scroll;
        let st = &d.stream;
        let mut o = String::new();
        let mut line = |l: String| {
            o.push_str(&l);
            o.push('\n');
        };
        line("{".into());
        line("  \"framework\": \"GPUI (zed v1.29.2)\",".into());
        line(format!("  \"messages\": {},", INITIAL_MESSAGES));
        line("  \"windowed\": true,".into());
        line(format!("  \"scale\": {:.2},", scale));
        line(format!(
            "  \"viewport_pt\": [{:.1}, {:.1}],",
            f32::from(viewport.width),
            f32::from(viewport.height)
        ));
        line("  \"initial\": {".into());
        line(format!("    \"corpus_parse_ms\": {:.4},", d.corpus_parse_ms));
        line(format!("    \"runtime_setup_ms\": {:.4},", d.setup_ms));
        line(format!("    \"font_load_ms\": {:.4},", d.font_load_ms));
        line(format!("    \"image_decode_ms\": {:.4},", d.image_decode_ms));
        line(format!("    \"tree_build_ms\": {:.4},", i.phases.view_req_ms));
        line("    \"style_ms\": 0.0,".into());
        line(format!("    \"text_shape_ms\": {:.4},", i.phases.text_shape_ms));
        line(format!("    \"text_shape_miss_ms\": {:.4},", i.phases.text_shape_miss_ms));
        line(format!("    \"text_shape_calls\": {},", init_snap.text_shape_calls));
        line(format!("    \"text_shape_misses\": {},", init_snap.text_shape_misses));
        line(format!("    \"layout_pure_ms\": {:.4},", i.phases.layout_pure_ms));
        line(format!("    \"prepaint_ms\": {:.4},", i.phases.prepaint_ms));
        line(format!("    \"paint_prep_ms\": {:.4},", i.phases.prepaint_ms + i.phases.paint_ms));
        line(format!("    \"paint_only_ms\": {:.4},", i.phases.paint_ms));
        line(format!("    \"gpu_encode_ms\": {:.4},", i.gpu_encode_ms));
        line(format!("    \"drawable_wait_ms\": {:.4},", i.drawable_wait_ms));
        line(format!("    \"gpu_exec_ms\": {:.4},", i.gpu_exec_ms));
        line(format!("    \"present_ms\": {:.4},", i.present_mean(i.present_ms)));
        line(format!("    \"frame_to_present_ms\": {:.4},", i.present_mean(i.frame_to_present_ms)));
        line(format!("    \"draws\": {},", i.draws));
        line(format!("    \"dropped_frames\": {},", i.dropped));
        line(format!("    \"first_frame_ms\": {:.4},", i.first_frame_ms));
        line(format!("    \"frame_total_ms\": {:.4},", i.phases.total_ms));
        line(format!("    \"cold_total_ms\": {:.4}", d.cold_total_ms));
        line("  },".into());
        for (name, a, last) in [("scroll_per_frame", sf, false), ("stream_per_frame", st, true)] {
            let p = &a.phases;
            line(format!("  \"{}\": {{", name));
            line(format!("    \"update_ms\": {:.4},", a.mean(p.view_req_ms)));
            line("    \"style_ms\": 0.0,".into());
            line(format!("    \"text_shape_ms\": {:.4},", a.mean(p.text_shape_ms)));
            line(format!("    \"layout_pure_ms\": {:.4},", a.mean(p.layout_pure_ms)));
            line(format!("    \"paint_prep_ms\": {:.4},", a.mean(p.prepaint_ms + p.paint_ms)));
            line(format!("    \"gpu_encode_ms\": {:.4},", a.mean(a.gpu_encode_ms)));
            line(format!("    \"drawable_wait_ms\": {:.4},", a.mean(a.drawable_wait_ms)));
            line(format!("    \"gpu_exec_ms\": {:.4},", a.mean(a.gpu_exec_ms)));
            line(format!("    \"cpu_frame_ms\": {:.4},", a.mean(a.cpu_frame_ms)));
            line(format!("    \"present_ms\": {:.4},", a.present_mean(a.present_ms)));
            line(format!("    \"frame_to_present_ms\": {:.4},", a.present_mean(a.frame_to_present_ms)));
            line(format!("    \"frames\": {},", a.frames));
            line(format!("    \"draws\": {},", a.draws));
            line(format!("    \"dropped_frames\": {},", a.dropped));
            line(format!("    \"frame_total_ms\": {:.4}", a.mean(p.total_ms)));
            line(format!("  }}{}", if last { "" } else { "," }));
        }
        line("}".into());
        let mut out = std::io::stdout().lock();
        out.write_all(o.as_bytes()).ok();
        out.flush().ok();
    }
}
