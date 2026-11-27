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
