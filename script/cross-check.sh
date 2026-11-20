#!/usr/bin/env bash
# Rule 10's compile checks for Windows and Linux, run from a Mac (for when CI cannot run them):
#
#   script/cross-check.sh            # Windows (core, app, daemon) and Linux (core, daemon)
#   script/cross-check.sh windows    # or: linux
#   TESTS=1 script/cross-check.sh    # the test targets too (--tests)
#
# A check runs the build scripts, and three of them need what a Mac does not have: ring's C
# wants the Windows CRT or Linux libc headers and an archiver, and gpui's and the app's resource
# files want llvm-rc. Check-only stand-ins cover them: clang with -ffreestanding and four tiny
# headers, an archiver that packs without a symbol table, and an llvm-rc that writes an empty
# file. A check never links those objects, so it says whether all the Rust code compiles for the
# target. It is not a link or a test run: those still need the platform (or CI).
set -euo pipefail
cd "$(dirname "$0")/.."

WHICH="${1:-all}"
JOBS="${CARGO_BUILD_JOBS:-4}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
STAND_INS="$PWD/$TARGET_DIR/cross-check-stand-ins"
EXTRA=()
if [ "${TESTS:-0}" = "1" ]; then
  EXTRA+=(--tests)
fi

need_target() {
  if ! rustup target list --installed | grep -qx "$1"; then
    echo "The Rust target $1 is not installed. Add it with: rustup target add $1" >&2
    exit 1
  fi
}

write_stand_ins() {
  mkdir -p "$STAND_INS/bin"
  cat > "$STAND_INS/assert.h" <<'EOF'
/* Check-only stand-in (script/cross-check.sh): the objects are never linked. */
#pragma once
#define assert(x) ((void)0)
#define static_assert _Static_assert
EOF
  cat > "$STAND_INS/string.h" <<'EOF'
/* Check-only stand-in (script/cross-check.sh): the objects are never linked. */
#pragma once
#include <stddef.h>
void *memcpy(void *, const void *, size_t);
void *memmove(void *, const void *, size_t);
void *memset(void *, int, size_t);
int memcmp(const void *, const void *, size_t);
size_t strlen(const char *);
EOF
  cat > "$STAND_INS/stdlib.h" <<'EOF'
/* Check-only stand-in (script/cross-check.sh): the objects are never linked. */
#pragma once
#include <stddef.h>
void abort(void);
unsigned long _byteswap_ulong(unsigned long);
unsigned long long _byteswap_uint64(unsigned long long);
unsigned short _byteswap_ushort(unsigned short);
EOF
  # lib.exe's way of being called (`-out:<lib> <objects>`), packed with ar.
  cat > "$STAND_INS/lib-ar" <<'EOF'
#!/bin/sh
out=""; files=""
for a in "$@"; do
  case "$a" in
    -out:*) out="${a#-out:}" ;;
    -*) ;;
    *) files="$files $a" ;;
  esac
done
rm -f "$out"
exec ar cr "$out" $files
EOF
  # ar for ELF objects: macOS's ranlib cannot index them, so no symbol table.
  cat > "$STAND_INS/ar-plain" <<'EOF'
#!/bin/sh
flags="$1"; shift
case "$flags" in
  s) exit 0 ;;
esac
flags=$(printf '%s' "$flags" | tr -d 's')
[ -z "$flags" ] && exit 0
exec ar "$flags" "$@"
EOF
  # llvm-rc: an empty resource file at `/fo <file>`.
  cat > "$STAND_INS/bin/llvm-rc" <<'EOF'
#!/bin/sh
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    /fo) out="$2"; shift ;;
  esac
  shift
done
[ -n "$out" ] && : > "$out"
exit 0
EOF
  chmod +x "$STAND_INS/lib-ar" "$STAND_INS/ar-plain" "$STAND_INS/bin/llvm-rc"
}

windows() {
  need_target x86_64-pc-windows-msvc
  echo "== Windows (x86_64-pc-windows-msvc): glasscode_core, glasscode, glasscoded"
  PATH="$STAND_INS/bin:$PATH" \
    CFLAGS_x86_64_pc_windows_msvc="-ffreestanding -isystem $STAND_INS" \
    AR_x86_64_pc_windows_msvc="$STAND_INS/lib-ar" \
    CARGO_BUILD_JOBS="$JOBS" \
    cargo check --locked --target x86_64-pc-windows-msvc -p glasscode_core -p glasscode -p glasscoded ${EXTRA[@]+"${EXTRA[@]}"}
}

linux() {
  need_target aarch64-unknown-linux-gnu
  echo "== Linux (aarch64-unknown-linux-gnu): glasscode_core, glasscoded"
  CC_aarch64_unknown_linux_gnu=clang \
    CFLAGS_aarch64_unknown_linux_gnu="--target=aarch64-unknown-linux-gnu -ffreestanding -isystem $STAND_INS" \
    AR_aarch64_unknown_linux_gnu="$STAND_INS/ar-plain" \
    CARGO_BUILD_JOBS="$JOBS" \
    cargo check --locked --target aarch64-unknown-linux-gnu -p glasscode_core -p glasscoded ${EXTRA[@]+"${EXTRA[@]}"}
}

write_stand_ins
case "$WHICH" in
  windows) windows ;;
  linux) linux ;;
  all) windows; linux ;;
  *) echo "usage: script/cross-check.sh [windows|linux|all]" >&2; exit 2 ;;
esac
echo "== The checks compiled."
