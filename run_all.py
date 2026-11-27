#!/usr/bin/env python3
"""Run all 5 simulated chat benchmarks (SilkUI raw ui-tree, Rune, GPUI, FerroUI, Chromium),
compute multi-run medians, save JSON to results/, and print a side-by-side phase table.

Usage:
  python3 run_all.py                     # run all 5 frameworks (3 runs each, 1 for Chromium)
  python3 run_all.py --runs 5 silk rune  # run subset with 5 runs each
  python3 run_all.py --gpu chromium      # run Chromium with hardware GPU enabled
"""
import argparse
import glob
import json
import os
import platform
import statistics
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(ROOT, "results")
os.makedirs(RESULTS, exist_ok=True)


def med_dict(dicts):
    out = {}
    for k in dicts[0]:
        vals = [d[k] for d in dicts if k in d]
        if isinstance(vals[0], float):
            out[k] = round(statistics.median(vals), 4)
        elif isinstance(vals[0], int):
            out[k] = int(statistics.median(vals))
        else:
            out[k] = vals[0]
    return out


def find_silk_bin():
    hits = glob.glob(os.path.join(ROOT, "silk", "build", "*", "release", "chat-bench-silk"))
    return hits[0] if hits else None


def run_native(name, cmd, runs, use_stderr=False):
    samples = []
    for _ in range(runs):
        t0 = time.perf_counter()
        p = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
        wall = (time.perf_counter() - t0) * 1000.0
        if p.returncode != 0:
            print(f"[{name}] FAILED (code {p.returncode}):\n{p.stderr[-1000:]}", file=sys.stderr)
            return None
        raw = p.stderr if use_stderr else p.stdout
        s, e = raw.find("{"), raw.rfind("}")
        if s < 0 or e <= s:
            print(f"[{name}] No JSON found:\n{raw[-1000:]}", file=sys.stderr)
            return None
        d = json.loads(raw[s : e + 1])
        d["process_wall_ms"] = round(wall, 2)
        samples.append(d)

    merged = {
        "framework": samples[0]["framework"],
        "messages": samples[0]["messages"],
        "runs": len(samples),
        "machine": f"{platform.system()} {platform.machine()}",
        "initial": med_dict([r["initial"] for r in samples]),
        "scroll_per_frame": med_dict([r["scroll_per_frame"] for r in samples]),
        "stream_per_frame": med_dict([r["stream_per_frame"] for r in samples]),
    }
    if "nodes" in samples[0]:
        merged["nodes"] = samples[0]["nodes"]
    out_path = os.path.join(RESULTS, f"{name}.json")
    with open(out_path, "w") as f:
        json.dump(merged, f, indent=2)
    print(f"[{name}] {runs}-run median saved -> {out_path}")
    return merged


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=3, help="Runs per native framework (default: 3)")
    ap.add_argument("--gpu", action="store_true", help="Enable hardware GPU in Chromium")
    ap.add_argument("targets", nargs="*", help="Subset: silk rune gpui ferroui chromium")
    args = ap.parse_args()

    only = set(args.targets) if args.targets else {"silk", "rune", "gpui", "ferroui", "chromium"}
    results = {}

    if "silk" in only:
        b = find_silk_bin()
        if b:
            results["silk"] = run_native("silk", [b], args.runs, use_stderr=True)
        else:
            print("[silk] binary not built; run ./build.sh silk", file=sys.stderr)

    for name, bin_name in [("rune", "chat-bench-rune"), ("gpui", "chat-bench-gpui"), ("ferroui", "chat-bench-ferroui")]:
        if name in only:
            b = os.path.join(ROOT, name, "target", "release", bin_name)
            if os.path.exists(b):
                results[name] = run_native(name, [b], args.runs)
            else:
                print(f"[{name}] binary not built; run ./build.sh {name}", file=sys.stderr)

    if "chromium" in only:
        cmd = ["node", os.path.join(ROOT, "chromium", "run_chromium.mjs")]
        if args.gpu:
            cmd.append("--gpu")
        p = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
        if p.returncode == 0:
            subprocess.run(["python3", os.path.join(ROOT, "chromium", "parse_trace.py"), os.path.join(RESULTS, "chromium-trace.json")], cwd=ROOT, check=True)
            with open(os.path.join(RESULTS, "chromium.json")) as f:
                results["chromium"] = json.load(f)
        else:
            print(f"[chromium] FAILED:\n{p.stderr}", file=sys.stderr)

    for k in ("silk", "rune", "gpui", "ferroui", "chromium"):
        if k not in results:
            fp = os.path.join(RESULTS, f"{k}.json")
            if os.path.exists(fp):
                with open(fp) as f:
                    results[k] = json.load(f)

    print("\n" + "=" * 102)
    print("INITIAL LOAD (500 rich chat messages -> first complete frame, ms)")
    print("=" * 102)
    print(f"{'Framework':<24} {'Pre-Lay':>9} {'Style':>8} {'Shape':>8} {'Layout':>8} {'PaintPrp':>9} {'Raster':>8} {'ColdTot':>9}")
    for k in ("silk", "rune", "gpui", "ferroui", "chromium"):
        if k not in results or not results[k]:
            continue
        r = results[k]["initial"]
        if k == "chromium":
            pre = sum(r.get(x, 0) for x in ("browser_init_ms", "nav_dispatch_ms", "renderer_init_ms", "html_parse_ms", "css_parse_ms", "v8_compile_ms", "dom_build_ms", "js_other_ms", "font_load_ms"))
            raster = r.get("raster_cpu_ms", 0) + r.get("composite_ms", 0)
        else:
            pre = sum(r.get(x, 0) for x in ("corpus_parse_ms", "runtime_setup_ms", "theme_setup_ms", "font_load_ms", "image_decode_ms", "css_parse_ms", "tree_build_ms"))
            raster = r.get("raster_ms", r.get("glyph_raster_ms", 0))
        print(f"{results[k]['framework']:<24} {pre:9.2f} {r.get('style_ms',0):8.2f} {r.get('text_shape_ms',0):8.2f} {r.get('layout_pure_ms',0):8.2f} {r.get('paint_prep_ms',0):9.2f} {raster:8.2f} {r.get('cold_total_ms',0):9.2f}")

    for phase, title in [("scroll_per_frame", "SCROLL (30 frames x 480px, ms/frame)"), ("stream_per_frame", "STREAM (30 live streaming frames, ms/frame)")]:
        print("\n" + "=" * 102)
        print(title)
        print("=" * 102)
        print(f"{'Framework':<24} {'Update':>9} {'Style':>8} {'Shape':>8} {'Layout':>8} {'PaintPrp':>9} {'Raster':>8} {'Total':>9}")
        for k in ("silk", "rune", "gpui", "ferroui", "chromium"):
            if k not in results or not results[k]:
                continue
            r = results[k][phase]
            upd = r.get("update_ms", r.get("v8_eval_ms", 0))
            raster = r.get("raster_ms", r.get("raster_cpu_ms", r.get("glyph_raster_ms", 0)))
            tot = r.get("frame_total_ms", upd + r.get("style_ms", 0) + r.get("text_shape_ms", 0) + r.get("layout_pure_ms", 0) + r.get("paint_prep_ms", 0) + raster + r.get("composite_ms", 0))
            print(f"{results[k]['framework']:<24} {upd:9.3f} {r.get('style_ms',0):8.3f} {r.get('text_shape_ms',0):8.3f} {r.get('layout_pure_ms',0):8.3f} {r.get('paint_prep_ms',0):9.3f} {raster:8.3f} {tot:9.3f}")


if __name__ == "__main__":
    main()
