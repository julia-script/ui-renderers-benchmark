// Windowed-mode GPU probe for chat-bench-ferroui (measurement only, no rendering changes).
//
// Interposes -[<concrete MTLCommandBuffer class> commit] and -presentDrawable: so that every
// command buffer the process commits (Skia Graphite's submits, FerroUI's present buffer, the frame
// completion markers) gets a completed handler. That gives:
//   - GPU execution time: sum of (GPUEndTime - GPUStartTime) over completed command buffers;
//   - GPU completion: the UI thread can wait until every buffer committed so far has completed;
//   - present latency: presentDrawable: encode -> drawable presentedTime (on glass).
#import <Metal/Metal.h>
#import <QuartzCore/QuartzCore.h>
#import <objc/runtime.h>
#include <pthread.h>
#include <stdint.h>
#include <time.h>

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_cv = PTHREAD_COND_INITIALIZER;
static uint64_t g_committed, g_completed, g_gpu_ns;
static uint64_t g_present_req, g_present_done, g_present_dropped, g_present_ns;
static IMP g_orig_commit, g_orig_present;
static int g_installed;

static void probe_commit(id self, SEL cmd) {
    pthread_mutex_lock(&g_mu);
    g_committed++;
    pthread_mutex_unlock(&g_mu);
    [(id<MTLCommandBuffer>)self addCompletedHandler:^(id<MTLCommandBuffer> cb) {
        CFTimeInterval s = cb.GPUStartTime, e = cb.GPUEndTime;
        pthread_mutex_lock(&g_mu);
        if (s > 0 && e > s) g_gpu_ns += (uint64_t)((e - s) * 1e9);
        g_completed++;
        pthread_cond_broadcast(&g_cv);
        pthread_mutex_unlock(&g_mu);
    }];
    ((void (*)(id, SEL))g_orig_commit)(self, cmd);
}

static void probe_present(id self, SEL cmd, id<MTLDrawable> drawable) {
    CFTimeInterval t = CACurrentMediaTime();
    pthread_mutex_lock(&g_mu);
    g_present_req++;
    pthread_mutex_unlock(&g_mu);
    [drawable addPresentedHandler:^(id<MTLDrawable> d) {
        CFTimeInterval p = d.presentedTime;
        pthread_mutex_lock(&g_mu);
        if (p > 0) { g_present_done++; if (p > t) g_present_ns += (uint64_t)((p - t) * 1e9); }
        else g_present_dropped++;
        pthread_cond_broadcast(&g_cv);
        pthread_mutex_unlock(&g_mu);
    }];
    ((void (*)(id, SEL, id))g_orig_present)(self, cmd, drawable);
}

int bench_gpu_probe_install(void) {
    if (g_installed) return 1;
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) return 0;
        id<MTLCommandQueue> queue = [device newCommandQueue];
        id<MTLCommandBuffer> cb = [queue commandBuffer];
        Class cls = object_getClass(cb);
        SEL commit = @selector(commit), present = @selector(presentDrawable:);
        Method mc = class_getInstanceMethod(cls, commit), mp = class_getInstanceMethod(cls, present);
        if (!mc || !mp) return 0;
        g_orig_commit = method_getImplementation(mc);
        g_orig_present = method_getImplementation(mp);
        class_replaceMethod(cls, commit, (IMP)probe_commit, method_getTypeEncoding(mc));
        class_replaceMethod(cls, present, (IMP)probe_present, method_getTypeEncoding(mp));
        g_installed = 1;
    }
    return 1;
}

// Blocks until every command buffer committed before the call has completed (timeout in ms).
// Returns 1 on success, 0 on timeout.
int bench_gpu_wait_idle(uint32_t timeout_ms) {
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += timeout_ms / 1000;
    deadline.tv_nsec += (long)(timeout_ms % 1000) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000L; }
    pthread_mutex_lock(&g_mu);
    uint64_t target = g_committed;
    int ok = 1;
    while (g_completed < target) {
        if (pthread_cond_timedwait(&g_cv, &g_mu, &deadline) != 0) { ok = 0; break; }
    }
    pthread_mutex_unlock(&g_mu);
    return ok;
}

// Blocks until every presentDrawable: so far has been presented or dropped (timeout in ms).
int bench_gpu_wait_presented(uint32_t timeout_ms) {
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += timeout_ms / 1000;
    deadline.tv_nsec += (long)(timeout_ms % 1000) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000L; }
    pthread_mutex_lock(&g_mu);
    int ok = 1;
    while (g_present_done + g_present_dropped < g_present_req) {
        if (pthread_cond_timedwait(&g_cv, &g_mu, &deadline) != 0) { ok = 0; break; }
    }
    pthread_mutex_unlock(&g_mu);
    return ok;
}

typedef struct {
    uint64_t committed, completed, gpu_ns, present_req, present_done, present_dropped, present_ns;
} BenchGpuSnapshot;

void bench_gpu_snapshot(BenchGpuSnapshot *out) {
    pthread_mutex_lock(&g_mu);
    out->committed = g_committed;
    out->completed = g_completed;
    out->gpu_ns = g_gpu_ns;
    out->present_req = g_present_req;
    out->present_done = g_present_done;
    out->present_dropped = g_present_dropped;
    out->present_ns = g_present_ns;
    pthread_mutex_unlock(&g_mu);
}
