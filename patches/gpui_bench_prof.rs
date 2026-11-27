//! Lightweight phase timing counters for chat-bench profiling inside GPUI.
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

pub static REQUEST_LAYOUT_NS: AtomicU64 = AtomicU64::new(0);
pub static TAFFY_LAYOUT_NS: AtomicU64 = AtomicU64::new(0);
pub static TEXT_SHAPE_NS: AtomicU64 = AtomicU64::new(0);
pub static TEXT_SHAPE_IN_TAFFY_NS: AtomicU64 = AtomicU64::new(0);
pub static TEXT_SHAPE_IN_REQ_NS: AtomicU64 = AtomicU64::new(0);
pub static TEXT_SHAPE_MISS_NS: AtomicU64 = AtomicU64::new(0);
pub static TEXT_SHAPE_CALLS: AtomicU64 = AtomicU64::new(0);
pub static TEXT_SHAPE_MISSES: AtomicU64 = AtomicU64::new(0);
pub static PREPAINT_NS: AtomicU64 = AtomicU64::new(0);
pub static PAINT_NS: AtomicU64 = AtomicU64::new(0);
pub static SCENE_FINISH_NS: AtomicU64 = AtomicU64::new(0);

// --- Windowed (Metal renderer) counters, written by gpui_apple's MetalRenderer::draw ---
/// CPU time inside MetalRenderer::draw (instance writes + encoding + commit), excl. drawable wait.
pub static GPU_ENCODE_NS: AtomicU64 = AtomicU64::new(0);
/// CPU time blocked in CAMetalLayer::nextDrawable (0 when a CAMetalDisplayLink hands out drawables).
pub static DRAWABLE_WAIT_NS: AtomicU64 = AtomicU64::new(0);
/// Sum of MTLCommandBuffer GPUEndTime - GPUStartTime for buffers completed since reset.
pub static GPU_EXEC_NS: AtomicU64 = AtomicU64::new(0);
/// Monotonic frame counters (never reset).
pub static FRAMES_COMMITTED: AtomicU64 = AtomicU64::new(0);
pub static FRAMES_COMPLETED: AtomicU64 = AtomicU64::new(0);
pub static FRAMES_PRESENTED: AtomicU64 = AtomicU64::new(0);
pub static FRAMES_DROPPED: AtomicU64 = AtomicU64::new(0);
/// Host times (CACurrentMediaTime seconds, stored as f64 bits) of the latest events.
pub static LAST_COMMIT_S: AtomicU64 = AtomicU64::new(0);
pub static LAST_GPU_END_S: AtomicU64 = AtomicU64::new(0);
pub static FIRST_GPU_END_S: AtomicU64 = AtomicU64::new(0);
pub static LAST_PRESENTED_S: AtomicU64 = AtomicU64::new(0);

/// Host time in seconds on the same clock as CACurrentMediaTime / Metal GPU timestamps.
#[cfg(target_vendor = "apple")]
pub fn host_time_s() -> f64 {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    let mut tb = Timebase { numer: 0, denom: 0 };
    // SAFETY: plain libSystem calls writing into a local struct.
    let ticks = unsafe {
        mach_timebase_info(&mut tb);
        mach_absolute_time()
    };
    ticks as f64 * tb.numer as f64 / tb.denom as f64 / 1e9
}
#[cfg(not(target_vendor = "apple"))]
pub fn host_time_s() -> f64 {
    0.0
}

fn store_f64(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Relaxed);
}
fn load_f64(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Relaxed))
}

/// Called after a frame's command buffer was committed.
pub fn record_commit(encode_ns: u64, drawable_wait_ns: u64) {
    GPU_ENCODE_NS.fetch_add(encode_ns, Relaxed);
    DRAWABLE_WAIT_NS.fetch_add(drawable_wait_ns, Relaxed);
    store_f64(&LAST_COMMIT_S, host_time_s());
    FRAMES_COMMITTED.fetch_add(1, Relaxed);
}

/// Called from the command buffer's completed handler (Metal thread).
pub fn record_gpu_complete(gpu_start_s: f64, gpu_end_s: f64) {
    if gpu_end_s > gpu_start_s && gpu_start_s > 0.0 {
        GPU_EXEC_NS.fetch_add(((gpu_end_s - gpu_start_s) * 1e9) as u64, Relaxed);
    }
    store_f64(&LAST_GPU_END_S, gpu_end_s);
    let _ = FIRST_GPU_END_S.compare_exchange(0, gpu_end_s.to_bits(), Relaxed, Relaxed);
    FRAMES_COMPLETED.fetch_add(1, Relaxed);
}

/// Called from the drawable's presented handler; `presented_s == 0` means the frame was dropped.
pub fn record_presented(presented_s: f64) {
    if presented_s > 0.0 {
        store_f64(&LAST_PRESENTED_S, presented_s);
    } else {
        FRAMES_DROPPED.fetch_add(1, Relaxed);
    }
    FRAMES_PRESENTED.fetch_add(1, Relaxed);
}

thread_local! {
    static IN_TEXT: Cell<u32> = const { Cell::new(0) };
    static IN_TAFFY: Cell<u32> = const { Cell::new(0) };
    static IN_REQ: Cell<u32> = const { Cell::new(0) };
}

#[derive(Default, Debug, Clone, Copy)]
pub struct Snapshot {
    pub request_layout_ns: u64,
    pub taffy_layout_ns: u64,
    pub text_shape_ns: u64,
    pub text_shape_in_taffy_ns: u64,
    pub text_shape_in_req_ns: u64,
    pub text_shape_miss_ns: u64,
    pub text_shape_calls: u64,
    pub text_shape_misses: u64,
    pub prepaint_ns: u64,
    pub paint_ns: u64,
    pub scene_finish_ns: u64,
    pub gpu_encode_ns: u64,
    pub drawable_wait_ns: u64,
    pub gpu_exec_ns: u64,
    pub frames_committed: u64,
    pub frames_completed: u64,
    pub frames_presented: u64,
    pub frames_dropped: u64,
    pub last_commit_s: f64,
    pub last_gpu_end_s: f64,
    pub first_gpu_end_s: f64,
    pub last_presented_s: f64,
}

pub fn reset() {
    REQUEST_LAYOUT_NS.store(0, Relaxed);
    TAFFY_LAYOUT_NS.store(0, Relaxed);
    TEXT_SHAPE_NS.store(0, Relaxed);
    TEXT_SHAPE_IN_TAFFY_NS.store(0, Relaxed);
    TEXT_SHAPE_IN_REQ_NS.store(0, Relaxed);
    TEXT_SHAPE_MISS_NS.store(0, Relaxed);
    TEXT_SHAPE_CALLS.store(0, Relaxed);
    TEXT_SHAPE_MISSES.store(0, Relaxed);
    PREPAINT_NS.store(0, Relaxed);
    PAINT_NS.store(0, Relaxed);
    SCENE_FINISH_NS.store(0, Relaxed);
    GPU_ENCODE_NS.store(0, Relaxed);
    DRAWABLE_WAIT_NS.store(0, Relaxed);
    GPU_EXEC_NS.store(0, Relaxed);
    FIRST_GPU_END_S.store(0, Relaxed);
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        request_layout_ns: REQUEST_LAYOUT_NS.load(Relaxed),
        taffy_layout_ns: TAFFY_LAYOUT_NS.load(Relaxed),
        text_shape_ns: TEXT_SHAPE_NS.load(Relaxed),
        text_shape_in_taffy_ns: TEXT_SHAPE_IN_TAFFY_NS.load(Relaxed),
        text_shape_in_req_ns: TEXT_SHAPE_IN_REQ_NS.load(Relaxed),
        text_shape_miss_ns: TEXT_SHAPE_MISS_NS.load(Relaxed),
        text_shape_calls: TEXT_SHAPE_CALLS.load(Relaxed),
        text_shape_misses: TEXT_SHAPE_MISSES.load(Relaxed),
        prepaint_ns: PREPAINT_NS.load(Relaxed),
        paint_ns: PAINT_NS.load(Relaxed),
        scene_finish_ns: SCENE_FINISH_NS.load(Relaxed),
        gpu_encode_ns: GPU_ENCODE_NS.load(Relaxed),
        drawable_wait_ns: DRAWABLE_WAIT_NS.load(Relaxed),
        gpu_exec_ns: GPU_EXEC_NS.load(Relaxed),
        frames_committed: FRAMES_COMMITTED.load(Relaxed),
        frames_completed: FRAMES_COMPLETED.load(Relaxed),
        frames_presented: FRAMES_PRESENTED.load(Relaxed),
        frames_dropped: FRAMES_DROPPED.load(Relaxed),
        last_commit_s: load_f64(&LAST_COMMIT_S),
        last_gpu_end_s: load_f64(&LAST_GPU_END_S),
        first_gpu_end_s: load_f64(&FIRST_GPU_END_S),
        last_presented_s: load_f64(&LAST_PRESENTED_S),
    }
}

pub struct TextScope(Option<Instant>);
impl TextScope {
    #[inline]
    pub fn enter() -> Self {
        TEXT_SHAPE_CALLS.fetch_add(1, Relaxed);
        let d = IN_TEXT.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        });
        Self(if d == 0 { Some(Instant::now()) } else { None })
    }
}
impl Drop for TextScope {
    #[inline]
    fn drop(&mut self) {
        IN_TEXT.with(|c| c.set(c.get().saturating_sub(1)));
        if let Some(t0) = self.0 {
            let dt = t0.elapsed().as_nanos() as u64;
            TEXT_SHAPE_NS.fetch_add(dt, Relaxed);
            if IN_TAFFY.with(|c| c.get() > 0) {
                TEXT_SHAPE_IN_TAFFY_NS.fetch_add(dt, Relaxed);
            } else if IN_REQ.with(|c| c.get() > 0) {
                TEXT_SHAPE_IN_REQ_NS.fetch_add(dt, Relaxed);
            }
        }
    }
}

pub struct TaffyScope(Option<Instant>);
impl TaffyScope {
    #[inline]
    pub fn enter() -> Self {
        let d = IN_TAFFY.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        });
        Self(if d == 0 { Some(Instant::now()) } else { None })
    }
}
impl Drop for TaffyScope {
    #[inline]
    fn drop(&mut self) {
        IN_TAFFY.with(|c| c.set(c.get().saturating_sub(1)));
        if let Some(t0) = self.0 {
            TAFFY_LAYOUT_NS.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
        }
    }
}

pub struct ReqScope(Option<Instant>);
impl ReqScope {
    #[inline]
    pub fn enter() -> Self {
        let d = IN_REQ.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        });
        Self(if d == 0 { Some(Instant::now()) } else { None })
    }
}
impl Drop for ReqScope {
    #[inline]
    fn drop(&mut self) {
        IN_REQ.with(|c| c.set(c.get().saturating_sub(1)));
        if let Some(t0) = self.0 {
            REQUEST_LAYOUT_NS.fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
        }
    }
}
