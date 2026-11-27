//! `--windowed`: the same workload in a real on-screen macOS window through FerroUI's production
//! desktop path (ferroui-native windowing, Skia Graphite on Metal into a CAMetalLayer, frames
//! rendered on the CVDisplayLink-driven render thread and presented with the layer's vsync).
//!
//! Every frame step: mutate on the UI thread, take the composition batch that carries the change,
//! wait (asynchronously, the main loop keeps running) until the render thread reports that batch
//! rendered and presented, then wait until every Metal command buffer committed so far has
//! completed on the GPU. Only then is the next step driven.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use chat_common::{
    HEIGHT, INITIAL_MESSAGES, INTER_BOLD, INTER_ITALIC, INTER_REGULAR, MONO, PICTURES, SCROLL_FRAMES, SCROLL_STEP,
    STREAM_FRAMES, WIDTH, apply_stream_step, make_stream_message, parse_corpus,
};
use ferroui_base::bench_prof;
use ferroui_base::media::fonts::EmbeddedFontCollection;
use ferroui_base::media::imaging::Bitmap;
use ferroui_base::media::IImage;
use ferroui_base::platform::{register_assets, IPlatformGraphics};
use ferroui_base::rendering::composition::Compositor;
use ferroui_base::threading::{CancellationToken, Dispatcher, DispatcherPriority, DispatcherTimer};
use ferroui_base::utilities::UriRef;
use ferroui_base::{FerroLocator, LocatorExtensions, Ref, Vector};
use ferroui_controls::{AppBuilder, Application, ScrollViewer, Window, WindowStartupLocation};
use ferroui_desktop::AppBuilderDesktopExtensions;
use ferroui_native::{FerroNativePlatformOptions, FerroNativeRenderingMode};
use ferroui_themes_fluent::FluentTheme;
use ferroui_view::prelude::*;

use super::{MESSAGES, PICTURE_BITMAPS, SANS, family, ms, ns_to_ms, page};

mod gpu_probe {
    #[repr(C)]
    #[derive(Default, Clone, Copy, Debug)]
    pub struct Snap {
        pub committed: u64,
        pub completed: u64,
        pub gpu_ns: u64,
        pub present_req: u64,
        pub present_done: u64,
        pub present_dropped: u64,
        pub present_ns: u64,
    }
    unsafe extern "C" {
        fn bench_gpu_probe_install() -> i32;
        fn bench_gpu_wait_idle(timeout_ms: u32) -> i32;
        fn bench_gpu_wait_presented(timeout_ms: u32) -> i32;
        fn bench_gpu_snapshot(out: *mut Snap);
    }
    pub fn install() -> bool {
        unsafe { bench_gpu_probe_install() != 0 }
    }
    pub fn wait_idle() -> bool {
        unsafe { bench_gpu_wait_idle(5000) != 0 }
    }
    pub fn wait_presented() -> bool {
        unsafe { bench_gpu_wait_presented(500) != 0 }
    }
    pub fn snapshot() -> Snap {
        let mut s = Snap::default();
        unsafe { bench_gpu_snapshot(&mut s) };
        s
    }
    impl Snap {
        pub fn since(&self, o: &Snap) -> Snap {
            Snap {
                committed: self.committed - o.committed,
                completed: self.completed - o.completed,
                gpu_ns: self.gpu_ns - o.gpu_ns,
                present_req: self.present_req - o.present_req,
                present_done: self.present_done - o.present_done,
                present_dropped: self.present_dropped - o.present_dropped,
                present_ns: self.present_ns - o.present_ns,
            }
        }
    }
}

/// A one-shot flag that can be raised from any thread and awaited on the UI dispatcher.
#[derive(Clone, Default)]
struct Flag(Arc<Mutex<(bool, Option<Waker>)>>);

impl Flag {
    fn fire(&self) {
        let waker = {
            let mut g = self.0.lock().unwrap();
            g.0 = true;
            g.1.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

impl Future for Flag {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut g = self.0.lock().unwrap();
        if g.0 {
            Poll::Ready(())
        } else {
            g.1 = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

fn sleep(d: Duration) -> Flag {
    let flag = Flag::default();
    let f = flag.clone();
    DispatcherTimer::run_once(move || f.fire(), d, DispatcherPriority::NORMAL).detach();
    flag
}

/// Commits what the UI thread changed, waits until the render thread has rendered and presented
/// that batch, then until the GPU has completed every command buffer committed so far.
async fn present_frame(compositor: &Rc<Compositor>) {
    let batch = compositor.request_commit_async();
    let flag = Flag::default();
    let f = flag.clone();
    batch.rendered().on_completed(move || f.fire());
    flag.await;
    if !gpu_probe::wait_idle() {
        eprintln!("[ferroui --windowed] warning: GPU completion wait timed out");
    }
}

struct Pre {
    t_start: Instant,
    corpus_parse_ms: f64,
    setup_ms: f64,
    theme_ms: f64,
    image_decode_ms: f64,
    tree_build_ms: f64,
    build_snap: bench_prof::Snapshot,
    backend: String,
}

struct Section {
    prof: bench_prof::Snapshot,
    gpu: gpu_probe::Snap,
    update_ms: f64,
    /// UI-thread time of the step before it waits for the frame (mutation, re-render, layout).
    ui_ms: f64,
    total_ms: f64,
}

fn scroll_to_bottom(scroller: &NodeRef<ScrollViewer>) {
    if let Some(s) = scroller.get() {
        let max = (s.extent().height - s.viewport().height).max(0.0);
        s.set_offset(Vector::new(0.0, max));
    }
}

pub fn run() {
    let t_start = Instant::now();
    let all = parse_corpus();
    let initial_msgs = all[..INITIAL_MESSAGES].to_vec();
    let extra_msgs = all[INITIAL_MESSAGES..].to_vec();
    MESSAGES.with(|m| *m.borrow_mut() = initial_msgs);
    let corpus_parse_ms = ms(t_start.elapsed());

    if !gpu_probe::install() {
        eprintln!("[ferroui --windowed] warning: Metal command-buffer probe not installed");
    }

    register_assets(
        "XBench",
        &[
            ("/Assets/Inter-Regular.ttf", INTER_REGULAR),
            ("/Assets/Inter-Bold.ttf", INTER_BOLD),
            ("/Assets/Inter-Italic.ttf", INTER_ITALIC),
            ("/Assets/JetBrainsMono-Regular.ttf", MONO),
        ],
    );
    let args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--windowed").collect();
    let t_setup = Instant::now();
    // Production defaults of the macOS backend, minus the OpenGL fallback, so that "a GPU platform
    // graphics exists" means Metal. Metal is first in the default list and always available here.
    let native_options = FerroNativePlatformOptions {
        rendering_mode: vec![FerroNativeRenderingMode::Metal, FerroNativeRenderingMode::Software],
        ..FerroNativePlatformOptions::default()
    };
    AppBuilder::configure::<Application>()
        .use_platform_detect()
        .with(Rc::new(native_options))
        .configure_fonts(|fm| {
            fm.add_font_collection(Rc::new(EmbeddedFontCollection::new(
                UriRef::parse_absolute("fonts:XBench").expect("key"),
                UriRef::parse_absolute("ferres://XBench/Assets").expect("source"),
            )));
        })
        .start(
            move |app, _| {
                let setup_ms = ms(t_setup.elapsed());
                let gpu = FerroLocator::current().get_service::<Arc<dyn IPlatformGraphics>>();
                let backend = if gpu.is_some() {
                    "Skia Graphite (Metal) -> CAMetalLayer, ferroui-native, CVDisplayLink render thread".to_string()
                } else {
                    "Skia CPU raster -> software framebuffer blit, ferroui-native".to_string()
                };

                let t_theme = Instant::now();
                app.styles().add(FluentTheme::new().as_style());
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
                        .title("chat-bench (FerroUI windowed)")
                        .width(WIDTH as f64)
                        .height(HEIGHT as f64)
                        .can_resize(false)
                        .font_family(family(SANS))
                        .child(page().tick(tick).scroller(scroller))
                        .into()
                });
                let tree_build_ms = ms(t_build.elapsed());
                let build_snap = bench_prof::snapshot();
                win.set_window_startup_location(WindowStartupLocation::CenterScreen);

                let pre = Pre {
                    t_start,
                    corpus_parse_ms,
                    setup_ms,
                    theme_ms,
                    image_decode_ms,
                    tree_build_ms,
                    build_snap,
                    backend,
                };
                let driver_win = win.clone();
                Dispatcher::ui_thread()
                    .spawn_local(async move { drive(driver_win, tick, scroller, extra_msgs, pre).await });
                // The main loop of the desktop application. The window is shown by the driver (its
                // first job), so that showing it is inside the measured initial phase; the driver
                // exits the process when the benchmark is done.
                drop(win);
                app.run_with_token(&CancellationToken::new());
                eprintln!("[ferroui --windowed] main loop ended before the benchmark finished");
                std::process::exit(1);
            },
            &args,
        );
}

async fn drive(
    win: Ref<Window>,
    tick: Signal<u64>,
    scroller: NodeRef<ScrollViewer>,
    extra_msgs: Vec<chat_common::Message>,
    pre: Pre,
) {
    let compositor = Compositor::try_get_default_compositor().expect("the platform compositor");

    // Phase 1: show the window and wait for the first complete frame on screen (GPU done).
    gpu_probe::wait_presented();
    let g0 = gpu_probe::snapshot();
    bench_prof::reset();
    let t_init = Instant::now();
    win.show();
    win.activate();
    loop {
        present_frame(&compositor).await;
        let drawn = bench_prof::snapshot().frames_drawn > 0;
        let laid_out = scroller.get().is_some_and(|s| s.extent().height > 0.0);
        if drawn && laid_out {
            break;
        }
        assert!(t_init.elapsed() < Duration::from_secs(30), "no first frame within 30 s");
    }
    let init_frame_ms = ms(t_init.elapsed());
    let cold_total_ms = ms(pre.t_start.elapsed());
    let init_snap = bench_prof::snapshot();
    gpu_probe::wait_presented();
    let init_gpu = gpu_probe::snapshot().since(&g0);
    let scale = win.render_scaling();

    // Let the window server finish bringing the window up before the timed phases.
    sleep(Duration::from_millis(300)).await;
    present_frame(&compositor).await;

    // Phase 2: 30 scroll frames x 480 px.
    gpu_probe::wait_presented();
    let g0 = gpu_probe::snapshot();
    bench_prof::reset();
    let mut scroll = Section { prof: Default::default(), gpu: Default::default(), update_ms: 0.0, ui_ms: 0.0, total_ms: 0.0 };
    for sf in 0..SCROLL_FRAMES {
        let y = ((sf + 1) as f64) * (SCROLL_STEP as f64);
        let t0 = Instant::now();
        if let Some(s) = scroller.get() {
            let max = (s.extent().height - s.viewport().height).max(0.0);
            s.set_offset(Vector::new(0.0, y.min(max)));
        }
        let t1 = Instant::now();
        present_frame(&compositor).await;
        let t2 = Instant::now();
        scroll.update_ms += ms(t1 - t0);
        scroll.total_ms += ms(t2 - t0);
    }
    scroll.prof = bench_prof::snapshot();
    gpu_probe::wait_presented();
    scroll.gpu = gpu_probe::snapshot().since(&g0);

    // Prepare phase 3: add the streaming card and pin to the bottom (one frame, untimed).
    let mut tick_val = 0u64;
    let mut stream_id = 9000usize;
    MESSAGES.with(|m| m.borrow_mut().push(make_stream_message(stream_id)));
    tick_val += 1;
    tick.set(tick_val);
    win.update_layout();
    scroll_to_bottom(&scroller);
    present_frame(&compositor).await;
    sleep(Duration::from_millis(100)).await;
    present_frame(&compositor).await;

    // Phase 3: 30 stream frames with the shared mutations.
    gpu_probe::wait_presented();
    let g0 = gpu_probe::snapshot();
    bench_prof::reset();
    let mut stream = Section { prof: Default::default(), gpu: Default::default(), update_ms: 0.0, ui_ms: 0.0, total_ms: 0.0 };
    let mut next_extra = 0usize;
    for tf in 0..STREAM_FRAMES {
        let t0 = Instant::now();
        MESSAGES.with(|m| apply_stream_step(&mut m.borrow_mut(), &extra_msgs, &mut next_extra, tf, &mut stream_id));
        tick_val += 1;
        tick.set(tick_val);
        let t1 = Instant::now();
        // The new extent is needed to pin to the bottom in the same frame (the headless harness gets
        // it from the dispatcher's render pass before its single render tick).
        win.update_layout();
        scroll_to_bottom(&scroller);
        let t_ui = Instant::now();
        present_frame(&compositor).await;
        let t2 = Instant::now();
        stream.update_ms += ms(t1 - t0);
        stream.ui_ms += ms(t_ui - t0);
        stream.total_ms += ms(t2 - t0);
    }
    stream.prof = bench_prof::snapshot();
    gpu_probe::wait_presented();
    stream.gpu = gpu_probe::snapshot().since(&g0);

    print_json(&pre, &init_snap, &init_gpu, init_frame_ms, cold_total_ms, scale, &scroll, &stream);

    if let Some(hold) = std::env::var("XB_HOLD_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        sleep(Duration::from_millis(hold)).await;
    }
    std::process::exit(0);
}

fn present_ms(g: &gpu_probe::Snap) -> f64 {
    if g.present_done == 0 { 0.0 } else { ns_to_ms(g.present_ns) / g.present_done as f64 }
}

#[allow(clippy::too_many_arguments)]
fn print_json(
    pre: &Pre,
    init: &bench_prof::Snapshot,
    init_gpu: &gpu_probe::Snap,
    init_frame_ms: f64,
    cold_total_ms: f64,
    scale: f64,
    scroll: &Section,
    stream: &Section,
) {
    use std::io::Write;
    let b = &pre.build_snap;
    let init_layout_pure =
        init.layout_total_ns.saturating_sub(init.text_shape_in_layout_ns).saturating_sub(init.style_in_layout_ns);
    let init_paint_prep = init.paint_prep_ns.saturating_sub(init.text_shape_in_paint_ns);

    let pure = |s: &bench_prof::Snapshot| {
        s.layout_total_ns.saturating_sub(s.text_shape_in_layout_ns).saturating_sub(s.style_in_layout_ns)
    };
    let prep = |s: &bench_prof::Snapshot| s.paint_prep_ns.saturating_sub(s.text_shape_in_paint_ns);

    let sf_n = SCROLL_FRAMES as f64;
    let st_n = STREAM_FRAMES as f64;
    let sp = &stream.prof;
    // Headless derives this as the step's time minus the measured phases. Here the step's wall time
    // also contains the wait for the display-link tick and the GPU, so the residual is taken over the
    // UI-thread part of the step only (mutation + re-render + layout), at least the mutation itself.
    let stream_reconcile_ms =
        (stream.ui_ms - ns_to_ms(sp.style_ns + sp.text_shape_ns + pure(sp))).max(stream.update_ms);

    let mut o = String::new();
    let mut line = |s: String| {
        o.push_str(&s);
        o.push('\n');
    };
    line("{".into());
    line("  \"framework\": \"FerroUI (0.5.0 + Skia)\",".into());
    line(format!("  \"messages\": {},", INITIAL_MESSAGES));
    line("  \"windowed\": true,".into());
    line(format!("  \"scale\": {:.2},", scale));
    line(format!("  \"backend\": \"{}\",", pre.backend));
    line("  \"initial\": {".into());
    line(format!("    \"corpus_parse_ms\": {:.4},", pre.corpus_parse_ms));
    line(format!("    \"runtime_setup_ms\": {:.4},", pre.setup_ms));
    line(format!("    \"theme_setup_ms\": {:.4},", pre.theme_ms));
    line(format!("    \"image_decode_ms\": {:.4},", pre.image_decode_ms));
    line(format!("    \"tree_build_ms\": {:.4},", (pre.tree_build_ms - ns_to_ms(b.style_ns)).max(0.0)));
    line(format!("    \"style_ms\": {:.4},", ns_to_ms(b.style_ns + init.style_ns)));
    line(format!("    \"text_shape_ms\": {:.4},", ns_to_ms(b.text_shape_ns + init.text_shape_ns)));
    line(format!("    \"text_shape_calls\": {},", b.text_shape_calls + init.text_shape_calls));
    line(format!("    \"layout_total_ms\": {:.4},", ns_to_ms(init.layout_total_ns)));
    line(format!("    \"layout_pure_ms\": {:.4},", ns_to_ms(init_layout_pure)));
    line(format!("    \"paint_prep_ms\": {:.4},", ns_to_ms(init_paint_prep)));
    line(format!("    \"raster_ms\": {:.4},", ns_to_ms(init.raster_ns)));
    line(format!("    \"gpu_encode_ms\": {:.4},", ns_to_ms(init.gpu_encode_ns)));
    line(format!("    \"gpu_exec_ms\": {:.4},", ns_to_ms(init_gpu.gpu_ns)));
    line(format!("    \"present_ms\": {:.4},", present_ms(init_gpu)));
    line(format!("    \"frames_drawn\": {},", init.frames_drawn));
    line(format!("    \"gpu_cmd_buffers\": {},", init_gpu.committed));
    line(format!("    \"frame_total_ms\": {:.4},", init_frame_ms));
    line(format!("    \"cold_total_ms\": {:.4}", cold_total_ms));
    line("  },".into());
    for (name, sec, n, update) in [
        ("scroll_per_frame", scroll, sf_n, scroll.update_ms / sf_n),
        ("stream_per_frame", stream, st_n, stream_reconcile_ms / st_n),
    ] {
        let p = &sec.prof;
        line(format!("  \"{name}\": {{"));
        line(format!("    \"update_ms\": {:.4},", update));
        line(format!("    \"style_ms\": {:.4},", ns_to_ms(p.style_ns) / n));
        line(format!("    \"text_shape_ms\": {:.4},", ns_to_ms(p.text_shape_ns) / n));
        line(format!("    \"layout_pure_ms\": {:.4},", ns_to_ms(pure(p)) / n));
        line(format!("    \"paint_prep_ms\": {:.4},", ns_to_ms(prep(p)) / n));
        line(format!("    \"raster_ms\": {:.4},", ns_to_ms(p.raster_ns) / n));
        line(format!("    \"gpu_encode_ms\": {:.4},", ns_to_ms(p.gpu_encode_ns) / n));
        line(format!("    \"gpu_exec_ms\": {:.4},", ns_to_ms(sec.gpu.gpu_ns) / n));
        line(format!("    \"present_ms\": {:.4},", present_ms(&sec.gpu)));
        line(format!("    \"frames_drawn\": {},", p.frames_drawn));
        line(format!("    \"gpu_cmd_buffers\": {},", sec.gpu.committed));
        line(format!("    \"frame_total_ms\": {:.4}", sec.total_ms / n));
        line(if name == "scroll_per_frame" { "  },".into() } else { "  }".into() });
    }
    line("}".into());
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(o.as_bytes());
    let _ = out.flush();
}
