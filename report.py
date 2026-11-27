#!/usr/bin/env python3
"""Build results/report.html from the windowed (real window + GPU) runs: results/*-windowed.json.

Run `python3 run_all.py --windowed` first.
"""
import html
import json
import os

ROOT = os.path.dirname(os.path.abspath(__file__))
RES = os.path.join(ROOT, "results")
ORDER = ["silk", "rune", "gpui", "ferroui", "chromium"]
NAMES = {
    "silk": "SilkUI",
    "rune": "Rune",
    "gpui": "GPUI (zed 1.29.2)",
    "ferroui": "FerroUI 0.5 + Skia",
    "chromium": "Chromium 158",
}
BUDGET_MS = 1000.0 / 120.0

# key, label, fill (CSS background)
PHASES = [
    ("setup", "setup + waits", "var(--s1)"),
    ("tree", "build tree", "var(--s2)"),
    ("update", "update / JS", "var(--s2)"),
    ("style", "styles", "var(--s3)"),
    ("shape", "shape text", "var(--s4)"),
    ("layout", "layout without text", "var(--s5)"),
    ("paint", "paint prep", "var(--s6)"),
    ("raster", "raster / render prep", "var(--s7)"),
    ("other", "other CPU", "var(--other)"),
    ("encode", "GPU encode + submit", "var(--s8)"),
    ("gpu", "GPU execution", "repeating-linear-gradient(135deg,var(--gpu) 0 3px,var(--gpu2) 3px 6px)"),
]
LABEL = {k: l for k, l, _ in PHASES}
FILL = {k: f for k, _, f in PHASES}


def g(d, *keys):
    return sum(d.get(k, 0) or 0 for k in keys)


def first_frame(name, r):
    """Post-setup first frame: tree build through GPU completion."""
    if name == "chromium":
        p = {"tree": g(r, "dom_build_ms"), "style": g(r, "style_ms"), "shape": g(r, "text_shape_ms"),
             "layout": g(r, "layout_pure_ms"), "paint": g(r, "paint_prep_ms"),
             "raster": g(r, "raster_cpu_ms", "composite_ms")}
        return p, False
    p = {"tree": g(r, "tree_build_ms", "view_ms"), "style": g(r, "style_ms"), "shape": g(r, "text_shape_ms"),
         "layout": g(r, "layout_pure_ms"), "paint": g(r, "paint_prep_ms"), "encode": g(r, "gpu_encode_ms"),
         "gpu": g(r, "gpu_exec_ms")}
    if name == "silk":
        p["raster"] = g(r, "glyph_raster_ms", "gpu_prepare_ms", "submit_ms")
    elif name == "rune":
        p["raster"] = g(r, "apply_ms")
    elif name == "ferroui":
        p["raster"] = max(0.0, g(r, "raster_ms") - g(r, "gpu_encode_ms"))
    return p, True


def per_frame(name, r):
    """CPU work + GPU execution per frame; waits for vsync/drawables excluded."""
    if name == "chromium":
        p = {"update": g(r, "v8_eval_ms"), "style": g(r, "style_ms"), "shape": g(r, "text_shape_ms"),
             "layout": g(r, "layout_pure_ms"), "paint": g(r, "paint_prep_ms"),
             "raster": g(r, "raster_cpu_ms", "composite_ms")}
        return p, False
    p = {"update": g(r, "update_ms"), "style": g(r, "style_ms"), "shape": g(r, "text_shape_ms"),
         "layout": g(r, "layout_pure_ms"), "paint": g(r, "paint_prep_ms"), "encode": g(r, "gpu_encode_ms"),
         "gpu": g(r, "gpu_exec_ms")}
    if name == "silk":
        p["raster"] = g(r, "glyph_raster_ms", "gpu_prepare_ms", "submit_ms")
    elif name == "ferroui":
        p["raster"] = max(0.0, g(r, "raster_ms") - g(r, "gpu_encode_ms"))
    elif name == "rune":
        named = g(r, "update_ms", "style_ms", "text_shape_ms", "layout_pure_ms", "paint_prep_ms")
        p["other"] = max(0.0, g(r, "cpu_build_ms") - named)
    elif name == "gpui":
        named = g(r, "update_ms", "style_ms", "text_shape_ms", "layout_pure_ms", "paint_prep_ms")
        p["other"] = max(0.0, g(r, "cpu_frame_ms") - named - g(r, "gpu_encode_ms"))
    return p, True


def fmt(v):
    if v >= 100:
        return f"{v:,.0f}"
    if v >= 10:
        return f"{v:.1f}"
    if v >= 1:
        return f"{v:.2f}"
    return f"{v:.3f}"


def with_setup(name, r, p):
    """Add launch-to-first-frame time not covered by the measured frame work."""
    q = {"setup": max(0.0, g(r, "cold_total_ms") - sum(p.values()))}
    q.update(p)
    return q


def block(rows, budget=None):
    keys = [k for k, _, _ in PHASES]
    present = [k for k in keys if any(p.get(k, 0) > 0.0005 for _, p, _ in rows)]
    totals = [(n, p, sum(p.values()), gm) for n, p, gm in rows]
    ts = sorted((t for _, _, t, _ in totals), reverse=True)
    vmax = max(ts[0], budget or 0)
    legend = "".join(f'<span class="lg"><i style="background:{FILL[k]}"></i>{LABEL[k]}</span>' for k in present)
    body = []
    for name, p, total, gm in totals:
        segs = []
        for k in present:
            v = p.get(k, 0)
            if v <= 0 or v / total < 0.006:
                continue
            tip = f"{NAMES[name]} · {LABEL[k]}: {fmt(v)} ms ({v / total * 100:.0f}%)"
            segs.append(f'<span class="seg" style="flex-grow:{v:.5f};background:{FILL[k]}" '
                        f'data-tip="{html.escape(tip)}"></span>')
        note = "" if gm else ' <span class="nm2">+ GPU not measured</span>'
        cut = total > vmax
        frac = min(total, vmax) / vmax
        inner = f'{total / vmax * 100:.3f}%' if cut else '100%'
        cut_note = ' <span class="nm2">(axis cut)</span>' if cut else ''
        body.append(
            f'<div class="row"><div class="nm">{NAMES[name]}</div><div class="track">'
            f'<div class="bar{" cut" if cut else ""}" style="width:calc((100% - var(--lab)) * {frac:.5f})">'
            f'<div class="in" style="width:{inner}">{"".join(segs)}</div></div>'
            f'<span class="tot">{fmt(total)} ms{note}{cut_note}</span></div></div>')
    rule = ""
    if budget:
        rule = (f'<div class="budget" style="--f:{budget / vmax:.5f}"><span>120 Hz frame: {budget:.1f} ms</span></div>')
    head = "".join(f"<th>{LABEL[k]}</th>" for k in present)
    trs = "".join(
        f"<tr><th>{NAMES[n]}</th>" + "".join(
            f"<td>{fmt(p[k]) if k in p else ('n/m' if k in ('encode', 'gpu') else '–')}</td>" for k in present)
        + f"<td><b>{fmt(t)}</b></td></tr>" for n, p, t, _ in totals)
    return f"""
  <div class="legend">{legend}</div>
  <div class="chart">{rule}{''.join(body)}</div>
  <details><summary>Numbers (ms)</summary>
    <div class="tw"><table><thead><tr><th></th>{head}<th>total</th></tr></thead><tbody>{trs}</tbody></table></div>
  </details>"""


def chart(cid, title, sub, rows, budget=None, alt=None):
    """alt = (toggle label, alt sub, alt rows): adds a switch between the two views."""
    if not alt:
        return f"""
<section id="{cid}">
  <h2>{title}</h2>
  <p class="sub">{sub}</p>{block(rows, budget)}
</section>"""
    label, alt_sub, alt_rows = alt
    return f"""
<section id="{cid}" class="tg">
  <div class="hd"><h2>{title}</h2>
    <label class="sw"><input type="checkbox" id="{cid}-tg"><span class="knob"></span>{label}</label></div>
  <div class="v-off"><p class="sub">{sub}</p>{block(rows, budget)}</div>
  <div class="v-on"><p class="sub">{alt_sub}</p>{block(alt_rows, budget)}</div>
</section>"""


def main():
    data = {}
    for n in ORDER:
        fp = os.path.join(RES, f"{n}-windowed.json")
        if os.path.exists(fp):
            with open(fp) as f:
                data[n] = json.load(f)
    if not data:
        raise SystemExit("no results/*-windowed.json; run: python3 run_all.py --windowed")
    msgs = next(iter(data.values()))["messages"]
    nodes = data.get("silk", {}).get("nodes")
    runs = min(d.get("runs", 1) for d in data.values())

    first = [(n, *first_frame(n, data[n]["initial"])) for n in data]
    # Launch-to-frame totals are wall time, so Chromium's unmeasured GPU time is inside "setup + waits".
    first_setup = [(n, with_setup(n, data[n]["initial"], p), True) for n, p, gm in first]
    scroll = [(n, *per_frame(n, data[n]["scroll_per_frame"])) for n in data]
    stream = [(n, *per_frame(n, data[n]["stream_per_frame"])) for n in data]

    sections = "".join([
        chart("first", f"First frame, {msgs:,}-message chat",
              "From building the UI tree to the GPU finishing the first frame. Process, browser and window "
              "startup are left out.", first,
              alt=("Include setup",
                   "From process launch to the first complete frame. Setup + waits is everything not in the "
                   "frame's measured work: process start, window (Chromium: browser + renderer process with a fresh "
                   "profile, HTML parse), fonts, images, and waits for the first present.", first_setup)),
        chart("scroll", "Scroll, work per frame",
              "30 frames × 480 px through the chat history. CPU work plus GPU execution per frame; time spent "
              "waiting for vsync or a drawable is left out.", scroll, BUDGET_MS),
        chart("stream", "Live stream, work per frame",
              "30 frames appending 6 words each, plus a new paragraph, a new turn with a table, and a streamed "
              "image.", stream, BUDGET_MS),
    ])
    nodes_txt = f", {nodes:,} UI nodes" if nodes else ""
    page = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>UI Renderer Phases</title>
<style>
:root {{
  color-scheme: light;
  --bg:#f9f9f7; --surface:#fcfcfb; --ink:#0b0b0b; --ink2:#52514e; --muted:#898781; --line:#e1e0d9;
  --s1:#2a78d6; --s2:#eb6834; --s3:#1baf7a; --s4:#eda100; --s5:#e87ba4; --s6:#008300; --s7:#6250d6; --s8:#e34948;
  --other:#b9b8b1; --gpu:#52514e; --gpu2:#c3c2b7;
}}
@media (prefers-color-scheme: dark) {{
  :root:not([data-theme="light"]) {{
    color-scheme: dark;
    --bg:#0d0d0d; --surface:#1a1a19; --ink:#f0efec; --ink2:#c3c2b7; --muted:#898781; --line:#2c2c2a;
    --s1:#3987e5; --s2:#d95926; --s3:#199e70; --s4:#c98500; --s5:#d55181; --s6:#0ca30c; --s7:#9085e9; --s8:#e66767;
    --other:#5c5b57; --gpu:#c3c2b7; --gpu2:#383835;
  }}
}}
:root[data-theme="dark"] {{
  color-scheme: dark;
  --bg:#0d0d0d; --surface:#1a1a19; --ink:#f0efec; --ink2:#c3c2b7; --muted:#898781; --line:#2c2c2a;
  --s1:#3987e5; --s2:#d95926; --s3:#199e70; --s4:#c98500; --s5:#d55181; --s6:#0ca30c; --s7:#9085e9; --s8:#e66767;
  --other:#5c5b57; --gpu:#c3c2b7; --gpu2:#383835;
}}
* {{ box-sizing:border-box; }}
body {{ margin:0; background:var(--bg); color:var(--ink);
  font:15px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif; font-variant-numeric:tabular-nums; }}
main {{ max-width:1080px; margin:0 auto; padding:40px 16px 64px; }}
h1 {{ font-size:26px; margin:0 0 6px; letter-spacing:-0.01em; }}
.lead {{ color:var(--ink2); margin:0 0 8px; max-width:820px; }}
section {{ margin-top:44px; }}
h2 {{ font-size:20px; margin:0 0 4px; }}
.sub {{ color:var(--muted); margin:0 0 14px; max-width:820px; }}
.legend {{ display:flex; flex-wrap:wrap; gap:6px 18px; margin-bottom:14px; color:var(--ink2); font-size:14px; }}
.lg {{ display:inline-flex; align-items:center; gap:7px; }}
.lg i {{ width:12px; height:12px; border-radius:3px; display:inline-block; }}
.chart {{ --name:170px; --lab:150px; display:grid; gap:12px; position:relative; padding-top:4px; }}
.chart:has(.budget) {{ padding-top:24px; }}
.row {{ display:grid; grid-template-columns:var(--name) 1fr; align-items:center; gap:14px; }}
.nm {{ text-align:right; font-size:14px; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }}
.track {{ display:flex; align-items:center; gap:10px; min-width:0; }}
.bar {{ flex:none; height:30px; min-width:3px; overflow:hidden; position:relative; border-radius:0 4px 4px 0; }}
.bar .in {{ display:flex; gap:2px; height:100%; }}
.bar.cut::after {{ content:""; position:absolute; right:14px; top:-4px; bottom:-4px; width:6px;
  background:var(--bg); transform:skewX(-20deg); }}
.tot {{ position:relative; z-index:1; background:var(--bg); padding:0 4px; margin-left:-4px; }}
.seg {{ flex-shrink:1; flex-basis:0; min-width:2px; height:100%; }}
.seg:first-child {{ border-radius:4px 0 0 4px; }}
.seg:last-child {{ border-radius:0 4px 4px 0; }}
.seg:only-child {{ border-radius:4px; }}
.seg:hover {{ filter:brightness(1.15); }}
.tot {{ color:var(--muted); font-size:14px; white-space:nowrap; }}
.nm2 {{ font-size:12px; }}
.budget {{ position:absolute; top:0; bottom:0; pointer-events:none;
  left:calc(var(--name) + 14px + (100% - var(--name) - 14px - var(--lab)) * var(--f));
  border-left:1.5px dashed var(--muted); }}
.budget span {{ position:absolute; top:0; left:6px; font-size:12px; color:var(--muted); white-space:nowrap; }}
.hd {{ display:flex; flex-wrap:wrap; align-items:center; justify-content:space-between; gap:8px 16px; margin-bottom:4px; }}
.hd h2 {{ margin:0; }}
.sw {{ display:inline-flex; align-items:center; gap:8px; font-size:14px; color:var(--ink2); cursor:pointer; user-select:none; }}
.sw input {{ position:absolute; opacity:0; width:1px; height:1px; }}
.knob {{ width:34px; height:20px; border-radius:10px; background:var(--line); position:relative; transition:background .15s; flex:none; }}
.knob::after {{ content:""; position:absolute; top:2px; left:2px; width:16px; height:16px; border-radius:50%;
  background:var(--ink); transition:transform .15s; }}
.sw input:checked + .knob {{ background:var(--s1); }}
.sw input:checked + .knob::after {{ transform:translateX(14px); background:#fff; }}
.sw input:focus-visible + .knob {{ outline:2px solid var(--s1); outline-offset:2px; }}
.tg .v-on {{ display:none; }}
.tg:has(.sw input:checked) .v-on {{ display:block; }}
.tg:has(.sw input:checked) .v-off {{ display:none; }}
details {{ margin-top:14px; color:var(--ink2); font-size:14px; }}
summary {{ cursor:pointer; color:var(--muted); }}
.tw {{ overflow-x:auto; }}
table {{ border-collapse:collapse; margin-top:8px; min-width:100%; }}
th, td {{ padding:6px 10px; border-bottom:1px solid var(--line); text-align:right; white-space:nowrap; }}
thead th {{ color:var(--muted); font-weight:500; }}
tbody th {{ text-align:left; font-weight:500; color:var(--ink); }}
.notes {{ margin-top:48px; color:var(--ink2); font-size:14px; max-width:820px; padding-left:18px; }}
.notes li {{ margin:5px 0; }}
#tip {{ position:fixed; pointer-events:none; background:var(--surface); color:var(--ink); border:1px solid var(--line);
  border-radius:8px; padding:6px 10px; font-size:13px; box-shadow:0 4px 16px rgba(0,0,0,.25); display:none; z-index:10; }}
@media (max-width:640px) {{
  .chart {{ --name:0px; }}
  .row {{ grid-template-columns:1fr; gap:4px; }}
  .nm {{ text-align:left; }}
  .budget {{ left:calc((100% - var(--lab)) * var(--f)); }}
}}
</style></head>
<body><main>
<h1>UI renderers vs Chromium, real windows</h1>
<p class="lead">{msgs:,}-message simulated chat (tables, images, code{nodes_txt}), unvirtualized. Every engine
opens a real 1040 × 720 window at 2× (Retina), renders on the GPU (Metal) and presents each frame with vsync on;
each frame waits for the GPU to finish before the next starts. Median of {runs} runs per engine, run one at a time.
macOS, Apple M1 Max, 120 Hz display.</p>
{sections}
<ul class="notes">
  <li><b>Chromium's GPU execution isn't measured</b>: its trace has no GPU timestamps, so its bars are CPU work only
  (raster = Skia's CPU-side raster tasks on the GPU path, plus compositing). The other engines' hatched segment is
  GPU start-to-end time from Metal or WebGPU timestamps.</li>
  <li>Waits for vsync and drawables are left out, so bars compare work, not frame rate. The dashed line is one
  120 Hz frame: work past it can't hold 120 fps.</li>
  <li>Phase boundaries come from each engine's own profiling hooks and are close, not identical. FerroUI's
  “raster / render prep” is Skia render time minus encode and includes some drawable wait. Rune's and GPUI's
  “other CPU” is frame CPU time not covered by a named phase.</li>
  <li>Silk redraws kept scenes on scroll: half its scroll frames were offset-only redraws without resubmitting.</li>
  <li>GPUI's first-frame text shaping is real CoreText shaping (44,544 calls, 4,541 cache misses).</li>
</ul>
</main>
<div id="tip"></div>
<script>
const tip = document.getElementById('tip');
document.addEventListener('pointermove', e => {{
  const t = e.target.closest('[data-tip]');
  if (!t) {{ tip.style.display = 'none'; return; }}
  tip.textContent = t.dataset.tip;
  tip.style.display = 'block';
  const r = tip.getBoundingClientRect();
  let x = e.clientX + 14, y = e.clientY + 14;
  if (x + r.width > innerWidth - 8) x = e.clientX - r.width - 14;
  if (y + r.height > innerHeight - 8) y = e.clientY - r.height - 14;
  tip.style.left = x + 'px'; tip.style.top = y + 'px';
}});
</script>
</body></html>"""
    out = os.path.join(RES, "report.html")
    with open(out, "w") as f:
        f.write(page)
    embed = page.replace("</style>", "body{background:transparent!important} main{padding:0!important;max-width:none}</style>")
    with open(os.path.join(RES, "report-embed.html"), "w") as f:
        f.write(embed)
    print(out)


if __name__ == "__main__":
    main()
