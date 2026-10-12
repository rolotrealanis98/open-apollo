#!/bin/sh
# Builds crates/uaconsole-ffi as a static library for iOS device, iOS simulator and macOS
# (Apple Silicon) and wraps them with the C header + module map into
#   apple/build/UAConsoleFFI.xcframework
# Run on a Mac with rustup and the targets installed:
#   rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin
# UAC_TARGETS overrides the target list (e.g. UAC_TARGETS=aarch64-apple-ios-sim for a
# simulator-only build).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd) # console-rs
out="$here/build"
# Prefer rustup's toolchain: Homebrew's cargo has no iOS standard library.
if [ -z "${CARGO:-}" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
	export PATH="$HOME/.cargo/bin:$PATH"
fi
cargo=${CARGO:-cargo}

export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-18.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-15.0}"
# Full debug info makes the archives several hundred MB; line tables keep backtraces.
export CARGO_PROFILE_RELEASE_DEBUG="${CARGO_PROFILE_RELEASE_DEBUG:-line-tables-only}"

targets="${UAC_TARGETS:-aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin}"
args=""
for t in $targets; do
	echo "==> cargo build --release --target $t -p uaconsole-ffi"
	"$cargo" build --manifest-path "$root/Cargo.toml" -p uaconsole-ffi --release --lib --target "$t"
	args="$args -library $root/target/$t/release/libuaconsole_ffi.a -headers $here/include"
done

mkdir -p "$out"
rm -rf "$out/UAConsoleFFI.xcframework"
# shellcheck disable=SC2086 # word splitting of $args is intended
xcodebuild -create-xcframework $args -output "$out/UAConsoleFFI.xcframework"
du -sh "$out/UAConsoleFFI.xcframework"/*/
