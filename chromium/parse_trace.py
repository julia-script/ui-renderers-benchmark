#!/usr/bin/env python3
"""Parse Chromium trace from chat-bench into clean, non-overlapping per-phase timings."""
import json
import os
import sys
from collections import defaultdict


def merge_intervals(intervals):
    """Return total duration (us) of union of [a, b] intervals."""
    if not intervals:
        return 0.0
    intervals = sorted(intervals)
    total = 0.0
    cur_a, cur_b = intervals[0]
    for a, b in intervals[1:]:
        if a <= cur_b:
            if b > cur_b:
                cur_b = b
        else:
            total += cur_b - cur_a
            cur_a, cur_b = a, b
    total += cur_b - cur_a
    return total


def analyze_window(evs, r_pid, r_main_tid, start_us, end_us, label):
    """Break down events inside [start_us, end_us] on the renderer process."""
    def main_evs(names):
        return [
            (e["ts"], e["ts"] + e.get("dur", 0))
            for e in evs
            if e.get("ph") == "X"
            and e.get("pid") == r_pid
            and e.get("tid") == r_main_tid
            and e["name"] in names
            and e["ts"] >= start_us
            and e["ts"] < end_us
        ]

    def proc_evs(names, pid=None):
        return [
            (e["ts"], e["ts"] + e.get("dur", 0))
            for e in evs
            if e.get("ph") == "X"
            and (pid is None or e.get("pid") == pid)
            and e["name"] in names
            and e["ts"] >= start_us
            and e["ts"] < end_us
        ]

    # Top-level lifecycle phases on renderer main thread
    html_parse = merge_intervals(main_evs({"ParseHTML"}))
    css_parse = merge_intervals(main_evs({"CSSParserImpl::parseStyleSheet"}))
    v8_compile = merge_intervals(main_evs({"v8.compile", "v8.compileModule", "v8.parseOnBackground"}))
    v8_eval = merge_intervals(main_evs({"EvaluateScript", "v8.evaluateModule", "v8.callFunction", "FunctionCall"}))
    style = merge_intervals(main_evs({"Document::updateStyle", "UpdateLayoutTree"}))
    layout_intervals = main_evs({"LocalFrameView::performLayout"})
    layout_total = merge_intervals(layout_intervals)

    # Inside Layout: Text Shaping (HarfBuzz) and Font Loading
    shape_intervals = main_evs({"InlineNode::ShapeTextIncludingFirstLine"})
    shape_total = merge_intervals(shape_intervals)
    font_intervals = main_evs({"FontCache::GetFontPlatformData", "DecodeFont", "FontDataManager::onMatchFamilyStyle"})
    # Clip font intervals to inside layout vs outside layout
    font_total = merge_intervals(font_intervals)

    # Count shape & font intervals that actually lie inside LocalFrameView::performLayout
    def inside_any(iv, containers):
        a, b = iv
        for ca, cb in containers:
            if a >= ca and b <= cb:
                return True
        return False

    shape_in_layout = merge_intervals([iv for iv in shape_intervals if inside_any(iv, layout_intervals)])
    font_in_layout = merge_intervals([iv for iv in font_intervals if inside_any(iv, layout_intervals)])
    # Pure layout excludes text shaping and font lookup/decode that happen inside performLayout
    shape_plus_font_in_layout = merge_intervals(
        [iv for iv in shape_intervals if inside_any(iv, layout_intervals)]
        + [iv for iv in font_intervals if inside_any(iv, layout_intervals)]
    )
    layout_pure = max(0.0, layout_total - shape_plus_font_in_layout)

    # Render prep on renderer main thread: CompositingInputs, PrePaint, Paint, PushToCompositor, Commit
    prepaint = merge_intervals(main_evs({"PrePaint", "LocalFrameView::RunPrePaintLifecyclePhase", "LocalFrameView::RunCompositingInputsLifecyclePhase"}))
    paint = merge_intervals(main_evs({"LocalFrameView::RunPaintLifecyclePhase", "Paint"}))
    commit = merge_intervals(main_evs({"Commit", "LayerTreeHost::DoUpdateLayers", "LocalFrameView::pushPaintArtifactToCompositor"}))
    paint_prep = merge_intervals(
        main_evs({
            "PrePaint",
            "LocalFrameView::RunPrePaintLifecyclePhase",
            "LocalFrameView::RunCompositingInputsLifecyclePhase",
            "LocalFrameView::RunPaintLifecyclePhase",
            "LocalFrameView::pushPaintArtifactToCompositor",
            "Commit",
        })
    )

    # Rasterization on renderer worker threads (both wall union and total CPU across workers)
    raster_evs = proc_evs({"RasterTask", "DisplayItemList::Raster"}, pid=r_pid)
    raster_wall = merge_intervals(raster_evs)
    raster_cpu = sum(
        e.get("dur", 0)
        for e in evs
        if e.get("ph") == "X"
        and e.get("pid") == r_pid
        and e["name"] == "RasterTask"
        and e["ts"] >= start_us
        and e["ts"] < end_us
    )

    # Image decode on renderer workers / main
    img_decode_cpu = sum(
        e.get("dur", 0)
        for e in evs
        if e.get("ph") == "X"
        and e.get("pid") == r_pid
        and e["name"] in ("ImageDecodeTask", "Decode Image", "ImageFrameGenerator::decode")
        and e["ts"] >= start_us
        and e["ts"] < end_us
    )

    # Compositor draw & swap (Renderer compositor thread + Viz)
    draw_evs = proc_evs({"ProxyImpl::ScheduledActionDraw", "Display::DrawAndSwap", "LayerTreeHostImpl::DrawLayers", "DirectRenderer::DrawFrame"}, pid=r_pid)
    composite_wall = merge_intervals(draw_evs)

    # Main frames count in window
    main_frames = [
        e for e in evs
        if e.get("ph") == "X"
        and e.get("pid") == r_pid
        and e.get("tid") == r_main_tid
        and e["name"] == "ProxyMain::BeginMainFrame"
        and e["ts"] >= start_us
        and e["ts"] < end_us
    ]

    return {
        "label": label,
        "wall_ms": (end_us - start_us) / 1000.0,
        "main_frames": len(main_frames),
        "html_parse_ms": html_parse / 1000.0,
        "css_parse_ms": css_parse / 1000.0,
        "v8_compile_ms": v8_compile / 1000.0,
        "v8_eval_ms": v8_eval / 1000.0,
        "font_load_ms": font_total / 1000.0,
        "style_ms": style / 1000.0,
        "text_shape_ms": shape_total / 1000.0,
        "text_shape_calls": len(shape_intervals),
        "layout_total_ms": layout_total / 1000.0,
        "layout_pure_ms": layout_pure / 1000.0,
        "prepaint_ms": prepaint / 1000.0,
        "paint_ms": paint / 1000.0,
        "commit_ms": commit / 1000.0,
        "paint_prep_ms": paint_prep / 1000.0,
        "image_decode_ms": img_decode_cpu / 1000.0,
        "raster_wall_ms": raster_wall / 1000.0,
        "raster_cpu_ms": raster_cpu / 1000.0,
        "composite_ms": composite_wall / 1000.0,
    }


def main():
    default_trace = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "results", "chromium-trace.json")
    trace_path = sys.argv[1] if len(sys.argv) > 1 else default_trace
    d = json.load(open(trace_path))
    evs = d.get("traceEvents", [])

    # Find user_timing marks to identify our benchmark renderer PID/TID
    marks = {}
    r_pid = None
    r_main_tid = None
    for e in evs:
        if e.get("cat") == "blink.user_timing" and e.get("name", "").startswith("xb:"):
            marks[e["name"]] = e["ts"]
            r_pid = e["pid"]
            r_main_tid = e["tid"]

    if r_pid is None:
        print("ERROR: No xb: marks found in trace!", file=sys.stderr)
        sys.exit(1)

    # Identify browser process PID
    proc_names = {}
    for e in evs:
        if e.get("ph") == "M" and e.get("name") == "process_name":
            proc_names[e["pid"]] = e.get("args", {}).get("name", "")
    b_pid = next((p for p, n in proc_names.items() if n == "Browser"), None)

    # Earliest timestamp in trace (process start)
    t0 = min(e["ts"] for e in evs if e.get("ts", 0) > 0)

    def first_x(name, pid=None):
        xs = [e for e in evs if e.get("ph") == "X" and e["name"] == name and (pid is None or e["pid"] == pid)]
        return min(xs, key=lambda e: e["ts"]) if xs else None

    # Pre-page Chromium startup breakdown
    b_pre = first_x("BrowserMainLoop::PreMainMessageLoopRun", b_pid)
    nav = first_x("Navigator::Navigate", b_pid)
    r_init = first_x("RenderThreadImpl::Init", r_pid)
    r_commit = first_x("RenderFrameImpl::CommitNavigation", r_pid)

    browser_init_ms = ((b_pre["ts"] + b_pre.get("dur", 0)) - t0) / 1000.0 if b_pre else 0.0
    nav_dispatch_ms = ((nav["ts"] - (b_pre["ts"] + b_pre.get("dur", 0))) / 1000.0) if (b_pre and nav) else 0.0
    renderer_spawn_init_ms = ((r_commit["ts"] + r_commit.get("dur", 0)) - nav["ts"]) / 1000.0 if (nav and r_commit) else 0.0
    dom_build_ms = (marks["xb:dom_build:end"] - marks["xb:dom_build:start"]) / 1000.0

    # Initial window: from renderer navigation commit through xb:initial:end
    init_start = r_commit["ts"] if r_commit else t0
    init_end = marks["xb:initial:end"]
    initial = analyze_window(evs, r_pid, r_main_tid, init_start, init_end, "initial")
    # Subtract dom_build from v8_eval so JS compile/eval doesn't double-count DOM build
    initial["browser_init_ms"] = browser_init_ms
    initial["nav_dispatch_ms"] = max(0.0, nav_dispatch_ms)
    initial["renderer_init_ms"] = max(0.0, renderer_spawn_init_ms)
    initial["dom_build_ms"] = dom_build_ms
    initial["js_other_ms"] = max(0.0, initial["v8_eval_ms"] - dom_build_ms)
    initial["cold_total_ms"] = (init_end - t0) / 1000.0

    scroll = analyze_window(evs, r_pid, r_main_tid, marks["xb:scroll:start"], marks["xb:scroll:end"], "scroll")
    stream = analyze_window(evs, r_pid, r_main_tid, marks["xb:stream:start"], marks["xb:stream:end"], "stream")

    # Normalize scroll & stream to per-frame (30 frames each)
    SCROLL_N = 30
    STREAM_N = 30
    out = {
        "framework": "Chromium (Chrome 158)",
        "messages": 1000,
        "initial": initial,
        "scroll_total": scroll,
        "scroll_per_frame": {k: (v / SCROLL_N if isinstance(v, float) else v) for k, v in scroll.items()},
        "stream_total": stream,
        "stream_per_frame": {k: (v / STREAM_N if isinstance(v, float) else v) for k, v in stream.items()},
    }
    out_path = os.path.join(os.path.dirname(trace_path), "chromium.json")
    with open(out_path, "w") as f:
        json.dump(out, f, indent=2)

    print("=== CHROMIUM INITIAL (Cold -> First Frame, 1000 chat messages) ===")
    print(f"  Total cold wall:       {initial['cold_total_ms']:7.2f} ms")
    print(f"  --- Before Layout (Chromium overhead + page setup) ---")
    print(f"  Browser init:          {initial['browser_init_ms']:7.2f} ms")
    print(f"  Nav dispatch:          {initial['nav_dispatch_ms']:7.2f} ms")
    print(f"  Renderer spawn+init:   {initial['renderer_init_ms']:7.2f} ms")
    print(f"  HTML parse:            {initial['html_parse_ms']:7.2f} ms")
    print(f"  CSS parse:             {initial['css_parse_ms']:7.2f} ms")
    print(f"  V8 compile:            {initial['v8_compile_ms']:7.2f} ms")
    print(f"  JS/DOM build (1000msg): {initial['dom_build_ms']:7.2f} ms")
    print(f"  JS other:              {initial['js_other_ms']:7.2f} ms")
    print(f"  Font load/decode:      {initial['font_load_ms']:7.2f} ms")
    print(f"  Style recalc:          {initial['style_ms']:7.2f} ms")
    print(f"  --- Core UI Pipeline ---")
    print(f"  Text shaping (HB):     {initial['text_shape_ms']:7.2f} ms  ({initial['text_shape_calls']} calls)")
    print(f"  Layout (pure):         {initial['layout_pure_ms']:7.2f} ms  (total layout: {initial['layout_total_ms']:.2f} ms)")
    print(f"  Paint prep (P+P+C):    {initial['paint_prep_ms']:7.2f} ms  (prepaint {initial['prepaint_ms']:.2f}, paint {initial['paint_ms']:.2f}, commit {initial['commit_ms']:.2f})")
    print(f"  Image decode (CPU):    {initial['image_decode_ms']:7.2f} ms")
    print(f"  Raster (Skia CPU):     {initial['raster_cpu_ms']:7.2f} ms  (wall: {initial['raster_wall_ms']:.2f} ms)")
    print(f"  Composite/Draw:        {initial['composite_ms']:7.2f} ms")
    print()
    for name, pf in [("SCROLL (30 frames, per-frame avg)", out["scroll_per_frame"]), ("STREAM (30 frames, per-frame avg)", out["stream_per_frame"])]:
        print(f"=== CHROMIUM {name} ===")
        print(f"  JS/Update:     {pf['v8_eval_ms']:6.3f} ms | Style: {pf['style_ms']:6.3f} ms | Shape: {pf['text_shape_ms']:6.3f} ms | Layout(pure): {pf['layout_pure_ms']:6.3f} ms | PaintPrep: {pf['paint_prep_ms']:6.3f} ms | Raster(CPU): {pf['raster_cpu_ms']:6.3f} ms | Composite: {pf['composite_ms']:6.3f} ms")


if __name__ == "__main__":
    main()
