#!/usr/bin/env bash
# Builds release binaries for SilkUI (raw ui-tree), Rune, GPUI, and FerroUI.
# Pass specific targets to build a subset: ./build.sh silk rune
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
TARGETS=("$@")
if [[ ${#TARGETS[@]} -eq 0 ]]; then
  TARGETS=(silk rune gpui ferroui)
fi

for t in "${TARGETS[@]}"; do
  case "$t" in
    silk)
      echo "==> Building SilkUI (raw ui-tree)..."
      (cd "$ROOT/silk" && silk build --release --progress plain)
      ;;
    rune)
      echo "==> Building Rune..."
      cargo build --release --manifest-path "$ROOT/rune/Cargo.toml"
      ;;
    gpui)
      echo "==> Building GPUI..."
      cargo build --release --manifest-path "$ROOT/gpui/Cargo.toml"
      ;;
    ferroui)
      echo "==> Building FerroUI..."
      cargo build --release --manifest-path "$ROOT/ferroui/Cargo.toml"
      ;;
    *)
      echo "Unknown target: $t (expected: silk, rune, gpui, ferroui)" >&2
      exit 1
      ;;
  esac
done

echo "==> Build complete! Run: python3 run_all.py"
