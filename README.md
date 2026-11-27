# UI Renderers Per-Phase Benchmark

Per-phase profiling benchmark comparing five UI engines on the exact same **500-message simulated chat session** (750 richly formatted paragraphs with bold/italic/inline-code/links, 84 headings + 4-column data tables, 63 embedded PNG images with captions, 83 bullet lists, 50 code blocks, 62 blockquotes; 33,927 total words, ~22,850 UI nodes, unvirtualized):

- **SilkUI (raw `ui-tree`)** — direct `UiTree` without React/Loom, profiled via a custom zero-allocation ambient `Tracer` plus two `Prof.span` hooks around `TextMeasure.analyze`/`shape`.
- **Rune** (`anomalyco/rune`) — profiled via Rune's built-in `frame-profile` (`rune::take_frame_phases`).
- **GPUI** (`zed-industries/zed` `v1.29.2` `crates/gpui`; note that `zed-industries/gpui` currently only contains a placeholder README) — profiled via lightweight atomic counters (`gpui::bench_prof`) across `request_layout`, Taffy `compute_layout`, `LineLayoutCache`/cosmic-text shaping, `prepaint`, `paint`, and `scene.finish`.
- **FerroUI** (`0.5.0` + Skia) — profiled via lightweight atomic counters (`ferroui_base::bench_prof`) across `try_apply_styling`, `execute_initial_layout_pass`/`execute_layout_pass`, HarfBuzz `TextShaper::shape_text`, `CompositingRenderer::update_core`, and Skia `ServerCompositionTarget::render`.
- **Chromium** (Google Chrome via `--trace-startup` + CDP) — separates everything before layout (browser init, renderer spawn/init, HTML/CSS parse, V8 compile/eval, DOM build, font load, style recalc) from HarfBuzz text shaping (`InlineNode::ShapeTextIncludingFirstLine`), pure LayoutNG, PrePaint/Paint/Commit, Skia raster, and compositor draw.

## Workloads

All five frameworks run the exact same three stages at `1040 x 720` (`scale = 1.0`) with bundled `Inter` (Regular/Bold/Italic), `JetBrains Mono`, and 4 PNG images:

1. **Initial Load** — cold setup through first complete rendered frame of all 500 messages.
2. **Scroll (`30 frames x 480 px`)** — sweeping 14,400 px through chat history (text, tables, images, code).
3. **Live Stream (`30 frames`)** — appending 6 words/frame to an active assistant reply pinned to the bottom, inserting a new paragraph at frame 10, a new user + rich assistant turn (with a table) at frame 15, and a streamed image at frame 22.

## Prerequisites

- **Rust** (stable toolchain) + `cargo`
- **Silk** (`silk` CLI + Clang 22 for release builds; see `silk-lang/silk`)
- **Node.js 22+** and **Python 3.10+**
- **Google Chrome** or **Chromium**
- On Linux: `libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libwayland-dev wayland-protocols libfontconfig1-dev`

## Quick Start

```bash
# 1. Generate the shared 500-message chat dataset, fetch pinned frameworks, and apply profiling patches
#    (Optionally pass SILKUI=/path/to/local/SilkUI to reuse an existing checkout)
./setup.sh

# 2. Build all native release binaries (or pass a subset, e.g.: ./build.sh silk rune)
./build.sh

# 3. Run all 5 benchmarks (3-run medians for native engines + Chromium trace)
python3 run_all.py

# On a machine with a hardware GPU, also test GPU-accelerated Chromium:
python3 run_all.py --gpu chromium
```

## Output & Rendering Modes

`run_all.py` writes per-framework median JSON files to `results/` (gitignored) and prints side-by-side phase tables for Initial Load, Scroll, and Stream.

Note what the rendering phase measures in each harness:
- **FerroUI** and **Chromium** execute full Skia rasterization (CPU software rasterization by default, or GPU rasterization in Chromium with `--gpu`).
- **SilkUI** and **Rune** perform CPU glyph-mask/atlas rasterization while producing GPU scene/instance lists.
- **GPUI** (`HeadlessAppContext`) builds the full `Scene` without GPU shader execution.
