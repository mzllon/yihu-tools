#!/usr/bin/env bash
# Build a native, dynamically linked package. Does not install anything.
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
cargo build --workspace --release --locked
version=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "yihu"))')
arch=$(uname -m)
name="yihu-${version}-linux-${arch}"
mkdir -p "$ROOT/dist"
stage=$(mktemp -d "$ROOT/dist/.package.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/$name/icons"
# cargo metadata respects CARGO_TARGET_DIR, unlike a hardcoded target/release.
target=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
install -m755 "$target/release/yihu" "$target/release/autodark-agent" "$target/release/yihu-panel" "$stage/$name/"
install -m755 "$ROOT/scripts/reinstall.sh" "$stage/$name/reinstall.sh"
cp "$ROOT/tools/yihu/icons/"*.png "$stage/$name/icons/"
if [[ -f "$ROOT/packaging/nautilus/copy_absolute_path.py" ]]; then
  cp "$ROOT/packaging/nautilus/copy_absolute_path.py" "$stage/$name/"
fi
printf '%s\n' "$arch" > "$stage/$name/ARCH"
printf '%s\n' "$version" > "$stage/$name/VERSION"
printf '%s\n' "$ROOT/target/release/yihu" > "$stage/$name/BUILD_EXE"
(cd "$stage/$name"; sha256sum yihu autodark-agent yihu-panel icons/*.png ARCH VERSION BUILD_EXE reinstall.sh > SHA256SUMS
  if [[ -f copy_absolute_path.py ]]; then sha256sum copy_absolute_path.py >> SHA256SUMS; fi)
tar -C "$stage" -czf "$ROOT/dist/$name.tar.gz" "$name"
sha256sum "$ROOT/dist/$name.tar.gz"
