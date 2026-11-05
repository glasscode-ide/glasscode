#!/usr/bin/env bash
# Build glasscoded for Linux servers (x86_64 and arm64) with Docker, into
# target/glasscoded/glasscoded-linux-<arch>. The Mac app bundles these and installs the
# right one on a developer's server.
#
#   script/build-glasscoded.sh            # both architectures
#   ARCHS=x86_64 script/build-glasscoded.sh
set -euo pipefail
cd "$(dirname "$0")/.."

ARCHS="${ARCHS:-x86_64 aarch64}"
OUT="target/glasscoded"
mkdir -p "$OUT"

# Caches survive between runs: toolchain, crates, git dependencies and the build folder
# (kept in a Docker volume: Cargo on a macOS bind mount fails intermittently).
docker run --rm \
  -v "$PWD":/src \
  -v glasscode-rustup:/usr/local/rustup \
  -v glasscode-cargo-registry:/usr/local/cargo/registry \
  -v glasscode-cargo-git:/usr/local/cargo/git \
  -v glasscode-target:/target \
  -w /src \
  -e CARGO_TARGET_DIR=/target \
  -e ARCHS="$ARCHS" \
  -e GLASSCODE_BUILD="${GLASSCODE_BUILD:-}" \
  rust:1-bookworm bash -euo pipefail -c '
    export DEBIAN_FRONTEND=noninteractive
    if ! command -v x86_64-linux-gnu-gcc >/dev/null || ! command -v aarch64-linux-gnu-gcc >/dev/null; then
      apt-get update -qq && apt-get install -y -qq gcc-x86-64-linux-gnu gcc-aarch64-linux-gnu >/dev/null
    fi
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
    for arch in $ARCHS; do
      target="$arch-unknown-linux-gnu"
      rustup target add "$target" >/dev/null 2>&1
      cargo build --release --locked -p glasscoded --target "$target"
      cp "/target/$target/release/glasscoded" "target/glasscoded/glasscoded-linux-$arch"
      "$arch-linux-gnu-strip" "target/glasscoded/glasscoded-linux-$arch"
    done
  '
ls -la "$OUT"
