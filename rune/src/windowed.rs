//! `--windowed`: the same chat workload in Rune's real production window (`rune::App`): an
//! on-screen AppKit window at the display's backing scale, drawn by Rune's renderer on its
//! default macOS GPU backend (Rune's native Metal backend behind wgpu's `custom` API), and
//! presented through its CAMetalLayer swap chain with Rune's production surface configuration
//! (`PresentMode::AutoVsync`, display-link pacing).
//!
//! The workload is driven one step per drawn frame from `AppSpec::poll`. After each frame is
//! presented, `App::on_frame_encoded` blocks on `device.poll(Wait)` until the GPU finished it,
//! reads that frame's GPU timestamps, and only then arms the next step, so every frame is
//! attributable. The compositor draws exactly what Rune's default frame draws
//! (`prepare_ui` + `draw`), adding begin/end-of-pass timestamp writes and their resolve.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chat_common::{
    HEIGHT, INITIAL_MESSAGES, Message, PICTURES, SCROLL_FRAMES, SCROLL_STEP, STREAM_FRAMES, WIDTH,
    apply_stream_step, make_stream_message, parse_corpus,
};
use rune::compose::{Compositor, Frame};
use rune::{App, AppSpec, FrameEncoded, FrameOutcome, Image, ImageCache, Tx, take_frame_phases};

use crate::{FAMILIES, FONTS, State, ms, view};

/// Gives up if the window never draws (occluded, no display) rather than hanging a benchmark run.
const WATCHDOG: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Initial,
    Scroll(usize),
    /// Pushes the streaming message and pins to the bottom: drawn but not measured, as headless.
    PrepStream,
    Stream(usize),
}

impl Step {
    fn next(self) -> Option<Step> {
        match self {
            Step::Initial => Some(Step::Scroll(0)),
            Step::Scroll(i) if i + 1 < SCROLL_FRAMES => Some(Step::Scroll(i + 1)),
            Step::Scroll(_) => Some(Step::PrepStream),
            Step::PrepStream => Some(Step::Stream(0)),
            Step::Stream(i) if i + 1 < STREAM_FRAMES => Some(Step::Stream(i + 1)),
            Step::Stream(_) => None,
        }
    }
}

/// The workload's progress, shared by `poll` (a plain fn) and the frame hook on the UI thread.
struct Driver {
    /// The step the next poll applies.
    pending: Option<Step>,
    /// Applied by a poll; the next drawn frame shows it.
    applied: Option<Step>,
    extra: Vec<Message>,
    next_extra: usize,
    stream_id: usize,
}

thread_local! {
    static DRIVER: RefCell<Option<Driver>> = const { RefCell::new(None) };
}

fn poll(s: &mut State, _: f64, tx: &mut Tx) -> bool {
    // Keep the display link running: one frame per refresh while the workload lasts.
    tx.wake_after(Duration::ZERO);
    DRIVER.with_borrow_mut(|d| {
        let d = d.as_mut().expect("driver");
        if d.applied.is_some() {
            return false;
        }
        let Some(step) = d.pending.take() else { return false };
        d.applied = Some(step);
        match step {
            Step::Initial => false,
            Step::Scroll(i) => {
                tx.scroll_to_instant("scroller", 0.0, (i + 1) as f32 * SCROLL_STEP);
                false
            }
            Step::PrepStream => {
                s.messages.push(make_stream_message(d.stream_id));
                tx.scroll_to_bottom_instant("scroller");
                true
            }
            Step::Stream(tf) => {
                apply_stream_step(&mut s.messages, &d.extra, &mut d.next_extra, tf, &mut d.stream_id);
                tx.scroll_to_bottom_instant("scroller");
                true
            }
        }
    })
}

struct Timestamps {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Nanoseconds per tick.
    period: f64,
}

#[derive(Default)]
struct Gpu {
    device: Option<wgpu::Device>,
    backend: String,
    adapter: String,
    timestamps: Option<Timestamps>,
    no_timestamps: Option<&'static str>,
    /// The last frame resolved its timestamps into `read`.
    wrote: bool,
    scale: f32,
    surface_px: (u32, u32),
    shaped: u32,
    rasterized: u64,
}

/// Rune's default frame (`Frame::draw_ui`) with the frame pass's GPU time sampled at its
/// boundaries (`Renderer::draw_timed`: layer passes included when there are any).
struct TimedFrame(Rc<RefCell<Gpu>>);

impl Compositor<State> for TimedFrame {
    fn features(&self) -> wgpu::Features {
        // Rune's Metal backend has it (counter sampling at stage boundaries), so this keeps it.
        wgpu::Features::TIMESTAMP_QUERY
    }

    fn draws_own(&self) -> bool {
        false
    }

    fn render(&mut self, f: &mut Frame<'_, State>) {
        let mut g = self.0.borrow_mut();
        if g.device.is_none() {
            g.device = Some(f.device.clone());
            g.backend = rune::describe_gpu(f.device);
            g.adapter = f.device.adapter_info().name;
            if f.device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
                let resolve = f.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("bench timestamps"),
                    size: 16,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                let read = f.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("bench timestamps read"),
                    size: 16,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let set = f.device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("bench frame"), ty: wgpu::QueryType::Timestamp, count: 2 });
                g.timestamps = Some(Timestamps { set, resolve, read, period: f64::from(f.queue.get_timestamp_period()) });
            } else {
                g.no_timestamps = Some("the device has no TIMESTAMP_QUERY (adapter can't sample counters at stage boundaries)");
            }
        }
        g.scale = f.rt.env.scale;
        g.surface_px = f.size;
        g.shaped = f.rt.text.shaped;
        g.rasterized = f.rt.text.atlas.rasterized();
        let clear = f.rt.clear_color();
        f.ui.prepare_ui(f.device, f.queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);
        let writes = g.timestamps.as_ref().map(|t| wgpu::RenderPassTimestampWrites {
            query_set: &t.set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        });
        f.ui.draw_timed(f.encoder, f.target, clear, writes);
        if let Some(t) = &g.timestamps {
            f.encoder.resolve_query_set(&t.set, 0..2, &t.resolve, 0);
            f.encoder.copy_buffer_to_buffer(&t.resolve, 0, &t.read, 0, 16);
        }
        g.wrote = g.timestamps.is_some();
    }
}

/// One measured frame.
#[derive(Clone, Copy, Default)]
struct Sample {
    phases: [f64; 6],
    /// `FrameEncoded::update_ms`: from the frame's start (drawable acquired) until built.
    update_ms: f64,
    /// `FrameEncoded::encode_ms`: uploads, prepare, encode and submit (CPU).
    encode_ms: f64,
    /// From submit until the hook: the present call (CAMetalLayer drawable commit) and Rune's
    /// post-present bookkeeping.
    present_ms: f64,
    /// CPU blocked in `device.poll(Wait)` until the GPU finished the frame.
    gpu_wait_ms: f64,
    /// GPU time between the frame pass's begin and end timestamps.
    gpu_exec_ms: Option<f64>,
    /// Frame start (after drawable acquisition) until the GPU finished the frame.
    total_ms: f64,
    /// Since the previous frame's start (display pacing included).
    interval_ms: Option<f64>,
}

#[derive(Default)]
struct Results {
    initial: Option<Sample>,
    initial_start: Option<Instant>,
    initial_done: Option<Instant>,
    initial_counts: (u32, u64),
    scroll: Vec<Sample>,
    stream: Vec<Sample>,
    stray: usize,
    last_start: Option<Instant>,
    refresh_hz: Option<f64>,
}

struct Launch {
    t_start: Instant,
    t_run: Instant,
    corpus_parse_ms: f64,
    image_decode_ms: f64,
}

pub fn run() {
    let t_start = Instant::now();
    let all = parse_corpus();
    let initial_msgs = all[..INITIAL_MESSAGES].to_vec();
    let extra = all[INITIAL_MESSAGES..].to_vec();
    let corpus_parse_ms = ms(t_start.elapsed());

    // Decoded before the window opens, so the first frame draws them (decoding is off-thread).
    let t_img = Instant::now();
    let cache = ImageCache::new(Arc::new(|| {}));
    let pictures: Vec<Image> = PICTURES.iter().map(|b| cache.load(&b[..])).collect();
    for p in &pictures {
        p.wait().expect("bundled picture decodes");
    }
    let image_decode_ms = ms(t_img.elapsed());

    DRIVER.with_borrow_mut(|d| {
        *d = Some(Driver { pending: None, applied: Some(Step::Initial), extra, next_extra: 0, stream_id: 9000 });
    });

    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        eprintln!("chat-bench-rune --windowed: no result after {WATCHDOG:?} (window occluded or not drawing?)");
        std::process::exit(2);
    });

    let gpu = Rc::new(RefCell::new(Gpu::default()));
    let results = Rc::new(RefCell::new(Results::default()));
    let state = State { messages: initial_msgs, pictures, _cache: Some(cache) };

    let spec = AppSpec {
        fonts: FONTS,
        families: FAMILIES,
        poll: Some(poll),
        ..AppSpec::new("Rune chat bench", (WIDTH, HEIGHT), || unreachable!("App::init supplies the state"), view)
    };
    let app = App::new(spec)
        // Rune's own flags would read `--windowed`; this harness owns its command line.
        .args(Vec::<String>::new())
        .init(move || state)
        .compositor(TimedFrame(gpu.clone()));
    let handle = app.handle();
    let mut launch = Launch { t_start, t_run: t_start, corpus_parse_ms, image_decode_ms };
    let hook_gpu = gpu.clone();
    let hook_results = results.clone();
    let _ = take_frame_phases();
    launch.t_run = Instant::now();
    let app = app.on_frame_encoded(move |fe: &FrameEncoded| {
        if fe.outcome() != FrameOutcome::Drawn {
            return;
        }
        let t_hook = Instant::now();
        let g = hook_gpu.borrow();
        let Some(device) = g.device.clone() else { return };
        let ts = g.timestamps.as_ref().filter(|_| g.wrote);
        if let Some(t) = ts {
            t.read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        }
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let t_done = Instant::now();
        let gpu_exec_ms = ts.map(|t| {
            let ticks: Vec<u64> = {
                let view = t.read.slice(..).get_mapped_range().expect("mapped timestamps");
                view.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
            };
            t.read.unmap();
            ticks[1].saturating_sub(ticks[0]) as f64 * t.period / 1e6
        });
        let p = take_frame_phases();
        let mut r = hook_results.borrow_mut();
        r.refresh_hz = fe.refresh_hz().or(r.refresh_hz);
        let start = fe.started().unwrap_or(fe.at());
        let sample = Sample {
            phases: [p[0], p[1], p[2], p[3], p[4], p[5]],
            update_ms: fe.update_ms().unwrap_or(0.0),
            encode_ms: fe.encode_ms().unwrap_or(0.0),
            present_ms: ms(t_hook.saturating_duration_since(fe.at())),
            gpu_wait_ms: ms(t_done - t_hook),
            gpu_exec_ms,
            total_ms: ms(t_done.saturating_duration_since(start)),
            interval_ms: r.last_start.map(|l| ms(start.saturating_duration_since(l))),
        };
        r.last_start = Some(start);
        let step = DRIVER.with_borrow_mut(|d| d.as_mut().expect("driver").applied.take());
        let Some(step) = step else {
            // A frame no step asked for (an appearance or backing change): waited out, not counted.
            r.stray += 1;
            return;
        };
        match step {
            Step::Initial => {
                r.initial = Some(sample);
                r.initial_start = Some(start);
                r.initial_done = Some(t_done);
                r.initial_counts = (g.shaped, g.rasterized);
            }
            Step::Scroll(_) => r.scroll.push(sample),
            Step::PrepStream => {}
            Step::Stream(_) => r.stream.push(sample),
        }
        if std::env::var_os("RUNE_BENCH_DEBUG").is_some() {
            eprintln!("{step:?}: total {:.2} ms, gpu {:?}, phases {:?}", sample.total_ms, sample.gpu_exec_ms, &sample.phases);
        }
        match step.next() {
            Some(next) => DRIVER.with_borrow_mut(|d| d.as_mut().expect("driver").pending = Some(next)),
            None => {
                print_json(&launch, &g, &r);
                // `RUNE_BENCH_HOLD_MS`: keep the last frame on screen (for a screenshot) before quitting.
                let hold = std::env::var("RUNE_BENCH_HOLD_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                if hold == 0 {
                    handle.quit();
                } else {
                    let handle = handle.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(hold));
                        handle.quit();
                    });
                }
            }
        }
    });
    app.run();
}

fn mean(samples: &[Sample], f: impl Fn(&Sample) -> f64) -> f64 {
    samples.iter().map(f).sum::<f64>() / samples.len().max(1) as f64
}

/// Milliseconds since `t_start` for each of Rune's launch marks.
fn launch_marks(t_start: Instant) -> Vec<(&'static str, f64)> {
    rune::marks::marks().into_iter().map(|(name, at)| (name, ms(at.saturating_duration_since(t_start)))).collect()
}

fn mark_gap(marks: &[(&'static str, f64)], from: &str, to: &str) -> Option<f64> {
    let at = |n: &str| marks.iter().find(|(m, _)| *m == n).map(|(_, t)| *t);
    Some((at(to)? - at(from)?).max(0.0))
}

fn section(out: &mut String, s: &[Sample], gpu_exec: bool) {
    let n = |f: &dyn Fn(&Sample) -> f64| mean(s, f);
    out.push_str(&format!("    \"update_ms\": {:.4},\n", n(&|x| (x.phases[1] - x.phases[2]).max(0.0))));
    out.push_str("    \"style_ms\": 0.0,\n");
    out.push_str(&format!("    \"text_shape_ms\": {:.4},\n", n(&|x| x.phases[3])));
    out.push_str(&format!("    \"layout_pure_ms\": {:.4},\n", n(&|x| (x.phases[2] - x.phases[3]).max(0.0))));
    out.push_str(&format!("    \"paint_prep_ms\": {:.4},\n", n(&|x| x.phases[4])));
    out.push_str(&format!("    \"view_ms\": {:.4},\n", n(&|x| x.phases[0])));
    out.push_str(&format!("    \"apply_ms\": {:.4},\n", n(&|x| x.phases[5])));
    out.push_str(&format!("    \"cpu_build_ms\": {:.4},\n", n(&|x| x.update_ms)));
    out.push_str(&format!("    \"gpu_encode_ms\": {:.4},\n", n(&|x| x.encode_ms)));
    if gpu_exec {
        out.push_str(&format!("    \"gpu_exec_ms\": {:.4},\n", n(&|x| x.gpu_exec_ms.unwrap_or(0.0))));
    }
    out.push_str(&format!("    \"present_ms\": {:.4},\n", n(&|x| x.present_ms)));
    out.push_str(&format!("    \"gpu_wait_ms\": {:.4},\n", n(&|x| x.gpu_wait_ms)));
    let intervals: Vec<f64> = s.iter().filter_map(|x| x.interval_ms).collect();
    out.push_str(&format!("    \"frame_interval_ms\": {:.4},\n", intervals.iter().sum::<f64>() / intervals.len().max(1) as f64));
    out.push_str(&format!("    \"frames\": {},\n", s.len()));
    out.push_str(&format!("    \"frame_total_ms\": {:.4}\n", n(&|x| x.total_ms)));
}

fn print_json(l: &Launch, g: &Gpu, r: &Results) {
    let i = r.initial.unwrap_or_default();
    let p = i.phases;
    let marks = launch_marks(l.t_start);
    let font_load_ms = mark_gap(&marks, "text.begin", "text.fonts").unwrap_or(0.0);
    let first_start = r.initial_start.unwrap_or(l.t_run);
    // Window, GPU device and renderer, text system and runtime: everything from `App::run` to the
    // first frame's start, less the font load reported apart.
    let runtime_setup_ms = (ms(first_start.saturating_duration_since(l.t_run)) - font_load_ms).max(0.0);
    let cold_total_ms = ms(r.initial_done.unwrap_or(l.t_start).saturating_duration_since(l.t_start));
    let gpu_exec = g.timestamps.is_some();
    let mut o = String::new();
    o.push_str("{\n");
    o.push_str("  \"framework\": \"Rune\",\n");
    o.push_str(&format!("  \"messages\": {},\n", INITIAL_MESSAGES));
    o.push_str("  \"windowed\": true,\n");
    o.push_str(&format!("  \"scale\": {:.2},\n", g.scale));
    o.push_str(&format!("  \"surface_px\": [{}, {}],\n", g.surface_px.0, g.surface_px.1));
    o.push_str(&format!("  \"gpu_backend\": {:?},\n", g.backend));
    o.push_str(&format!("  \"adapter\": {:?},\n", g.adapter));
    o.push_str("  \"present_mode\": \"AutoVsync (Rune production surface config; CAMetalLayer displaySyncEnabled)\",\n");
    if let Some(hz) = r.refresh_hz {
        o.push_str(&format!("  \"refresh_hz\": {hz:.2},\n"));
    }
    match g.no_timestamps {
        Some(why) => o.push_str(&format!("  \"gpu_exec_ms_omitted\": {why:?},\n")),
        None => o.push_str("  \"gpu_exec_source\": \"wgpu timestamp queries at the frame pass's begin/end (Metal counter sampling at stage boundaries)\",\n"),
    }
    o.push_str(&format!("  \"stray_frames\": {},\n", r.stray));
    o.push_str("  \"launch_marks_ms\": {");
    o.push_str(&marks.iter().map(|(n, t)| format!("{n:?}: {t:.3}")).collect::<Vec<_>>().join(", "));
    o.push_str("},\n");
    o.push_str("  \"initial\": {\n");
    o.push_str(&format!("    \"corpus_parse_ms\": {:.4},\n", l.corpus_parse_ms));
    o.push_str(&format!("    \"font_load_ms\": {:.4},\n", font_load_ms));
    o.push_str(&format!("    \"image_decode_ms\": {:.4},\n", l.image_decode_ms));
    o.push_str(&format!("    \"runtime_setup_ms\": {:.4},\n", runtime_setup_ms));
    o.push_str(&format!("    \"view_ms\": {:.4},\n", p[0]));
    o.push_str(&format!("    \"tree_build_ms\": {:.4},\n", (p[1] - p[2]).max(0.0)));
    o.push_str("    \"style_ms\": 0.0,\n");
    o.push_str(&format!("    \"text_shape_ms\": {:.4},\n", p[3]));
    o.push_str(&format!("    \"text_shape_calls\": {},\n", r.initial_counts.0));
    o.push_str(&format!("    \"layout_total_ms\": {:.4},\n", p[2]));
    o.push_str(&format!("    \"layout_pure_ms\": {:.4},\n", (p[2] - p[3]).max(0.0)));
    o.push_str(&format!("    \"paint_prep_ms\": {:.4},\n", p[4]));
    o.push_str(&format!("    \"apply_ms\": {:.4},\n", p[5]));
    o.push_str(&format!("    \"glyphs_rasterized\": {},\n", r.initial_counts.1));
    o.push_str(&format!("    \"cpu_build_ms\": {:.4},\n", i.update_ms));
    o.push_str(&format!("    \"gpu_encode_ms\": {:.4},\n", i.encode_ms));
    if let Some(gpu) = i.gpu_exec_ms {
        o.push_str(&format!("    \"gpu_exec_ms\": {gpu:.4},\n"));
    }
    o.push_str(&format!("    \"present_ms\": {:.4},\n", i.present_ms));
    o.push_str(&format!("    \"gpu_wait_ms\": {:.4},\n", i.gpu_wait_ms));
    o.push_str(&format!("    \"frame_total_ms\": {:.4},\n", i.total_ms));
    o.push_str(&format!("    \"cold_total_ms\": {:.4}\n", cold_total_ms));
    o.push_str("  },\n");
    o.push_str("  \"scroll_per_frame\": {\n");
    section(&mut o, &r.scroll, gpu_exec);
    o.push_str("  },\n");
    o.push_str("  \"stream_per_frame\": {\n");
    section(&mut o, &r.stream, gpu_exec);
    o.push_str("  }\n");
    o.push_str("}\n");
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(o.as_bytes());
    let _ = out.flush();
}
