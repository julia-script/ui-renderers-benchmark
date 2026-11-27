#!/usr/bin/env bash
# Fetches pinned framework sources and applies lightweight per-phase instrumentation patches.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"

echo "==> Generating shared 500-message chat dataset..."
python3 "$ROOT/gen_chat.py"

# 1. SilkUI (use existing SILKUI=... env var or shallow-clone silk-lang/SilkUI)
if [[ -n "${SILKUI:-}" ]]; then
  SILKUI_DIR="$(cd "$SILKUI" && pwd)"
else
  SILKUI_DIR="$ROOT/silk/vendor/SilkUI"
  if [[ ! -d "$SILKUI_DIR/.git" ]]; then
    echo "==> Cloning silk-lang/SilkUI..."
    mkdir -p "$ROOT/silk/vendor"
    git clone --depth 1 https://github.com/silk-lang/SilkUI.git "$SILKUI_DIR"
  fi
fi
if ! grep -q 'ui_tree.text.shape' "$SILKUI_DIR/modules/tree/src/provider.silk"; then
  echo "==> Patching SilkUI provider.silk for text shaping spans..."
  git -C "$SILKUI_DIR" apply "$ROOT/patches/silk-provider-trace.patch"
fi
SILK_VER=$(grep -m1 -oh '"0\.1\.0-dev\.[^"]*"' "$SILKUI_DIR/silk.toml" | tr -d '"')
LIB_FONTS_REV=$(grep -m1 -oE 'lib-fonts\.git", rev = "[0-9a-f]{40}' "$SILKUI_DIR/modules/tree/silk.toml" | grep -oE '[0-9a-f]{40}')
sed -e "s#@SILKUI@#$SILKUI_DIR#g" \
    -e "s#@SILK_VERSION@#$SILK_VER#g" \
    -e "s#@LIB_FONTS@#$LIB_FONTS_REV#g" \
    "$ROOT/silk/silk.toml.in" > "$ROOT/silk/silk.toml"
ln -sfn "$ROOT/data" "$ROOT/silk/data"
ln -sfn "$ROOT/assets" "$ROOT/silk/assets"

# 2. Rune (anomalyco/rune pinned commit; uses Rune's built-in frame-profile feature)
RUNE_REV="${RUNE_REV:-d170ff6e4a9691e3db0b0ad83e7e719b720afc84}"
if [[ ! -d "$ROOT/rune/vendor/rune/.git" ]]; then
  echo "==> Fetching Rune ($RUNE_REV)..."
  mkdir -p "$ROOT/rune/vendor"
  git init -q "$ROOT/rune/vendor/rune"
  git -C "$ROOT/rune/vendor/rune" remote add origin https://github.com/anomalyco/rune.git
  git -C "$ROOT/rune/vendor/rune" fetch -q --depth 1 origin "$RUNE_REV"
  git -C "$ROOT/rune/vendor/rune" checkout -q FETCH_HEAD
fi

# 3. GPUI (zed-industries/zed v1.29.2 sparse checkout; zed-industries/gpui is currently README-only)
GPUI_REV="${GPUI_REV:-2de9e435a5d98657f35c260b5f4dd0979df659e6}"
if [[ ! -d "$ROOT/gpui/vendor/zed/crates/gpui" ]]; then
  echo "==> Fetching GPUI from zed-industries/zed ($GPUI_REV)..."
  mkdir -p "$ROOT/gpui/vendor"
  git init -q "$ROOT/gpui/vendor/zed"
  git -C "$ROOT/gpui/vendor/zed" remote add origin https://github.com/zed-industries/zed.git
  git -C "$ROOT/gpui/vendor/zed" config core.sparseCheckout true
  cat > "$ROOT/gpui/vendor/zed/.git/info/sparse-checkout" <<'LIST'
/Cargo.toml
/Cargo.lock
/LICENSE*
/crates/gpui*/
/crates/bench_metrics/
/crates/collections/
/crates/http_client/
/crates/refineable/
/crates/scheduler/
/crates/sum_tree/
/crates/ztracing/
/crates/ztracing_macro/
/crates/util/
/crates/util_macros/
/crates/perf/
/crates/media/
/crates/zlog/
/crates/reqwest_client/
LIST
  git -C "$ROOT/gpui/vendor/zed" fetch -q --depth 1 --filter=blob:none origin "$GPUI_REV"
  git -C "$ROOT/gpui/vendor/zed" checkout -q FETCH_HEAD
  # Apply instrumentation patch and remove .git so Cargo fingerprint ignores sparse excludes
  cp "$ROOT/patches/gpui_bench_prof.rs" "$ROOT/gpui/vendor/zed/crates/gpui/src/bench_prof.rs"
  git -C "$ROOT/gpui/vendor/zed" apply "$ROOT/patches/gpui.patch"
  rm -rf "$ROOT/gpui/vendor/zed/.git" "$ROOT/gpui/vendor/zed/.gitignore"
fi

# 4. FerroUI (0.5.0 from crates.io, vendor + patch ferroui-base)
if [[ ! -d "$ROOT/ferroui/vendor/ferroui-base" ]]; then
  echo "==> Fetching FerroUI 0.5.0 ferroui-base from crates.io..."
  mkdir -p "$ROOT/ferroui/vendor"
  curl -fsSL "https://crates.io/api/v1/crates/ferroui-base/0.5.0/download" -o /tmp/ferroui-base-0.5.0.crate
  tar -xzf /tmp/ferroui-base-0.5.0.crate -C "$ROOT/ferroui/vendor"
  mv "$ROOT/ferroui/vendor/ferroui-base-0.5.0" "$ROOT/ferroui/vendor/ferroui-base"
  rm -f /tmp/ferroui-base-0.5.0.crate
  patch -p1 -d "$ROOT/ferroui/vendor/ferroui-base/src" < "$ROOT/patches/ferroui-base.patch"
fi

echo "==> Setup complete! Run ./build.sh next."
