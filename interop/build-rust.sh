#!/usr/bin/env bash
# Cross-compile netsu-rs to a statically-linked Linux musl binary for the
# interop matrix.
#
# musl static linking means the container needs no glibc and no Rust toolchain.
# The target follows the host architecture: Apple-Silicon containers run
# aarch64 natively, and emulating x86_64 would make throughput numbers
# meaningless for a tool whose entire job is measuring throughput.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="$repo_root/interop/bin"

case "$(uname -m)" in
  arm64 | aarch64) target="aarch64-unknown-linux-musl"; arch="aarch64" ;;
  x86_64 | amd64) target="x86_64-unknown-linux-musl";  arch="x86_64" ;;
  *) echo "unsupported host arch: $(uname -m)" >&2; exit 1 ;;
esac

echo "==> building netsu-rs for $target"

if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
  echo "==> installing rust target $target"
  rustup target add "$target"
fi

# `cross` handles the musl linker without a system cross-toolchain. Fall back
# to plain cargo when the target can link natively (Linux hosts usually can
# with musl-tools installed).
if command -v cross >/dev/null 2>&1; then
  builder="cross"
else
  builder="cargo"
  echo "==> 'cross' not found, using cargo (install with: cargo install cross)"
fi

cd "$repo_root/netsu-rs"
"$builder" build --release --target "$target"

mkdir -p "$out_dir"
cp "target/$target/release/netsu" "$out_dir/netsu-rs-$arch"
chmod +x "$out_dir/netsu-rs-$arch"

echo "==> wrote $out_dir/netsu-rs-$arch"
file "$out_dir/netsu-rs-$arch" || true
