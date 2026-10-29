#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

echo "Building agy-delta (release)..."
cargo build --release

mkdir -p .agents/plugins/delta/bin
rm -f .agents/plugins/delta/bin/agy-delta
cp target/release/agy-delta .agents/plugins/delta/bin/agy-delta
chmod +x .agents/plugins/delta/bin/agy-delta

if [[ "$(uname -s)" == "Darwin" ]]; then
  codesign --force --sign - .agents/plugins/delta/bin/agy-delta
fi

echo "Installed plugin binary to .agents/plugins/delta/bin/agy-delta"
