#!/bin/bash
# Removing the device while an ALSA PCM is open must not touch freed memory,
# and the driver's allocations must still be freed once the files close.
#
# Boots a KASAN kernel in QEMU/KVM, binds ua_apollo to an emulated PCI
# device (QEMU secondary-vga, whose BAR0 is plain RAM; tests/hot-unplug/
# ua_fixture.c gives it a dummy interrupt line), opens playback, capture and
# /dev/ua_apollo0, removes the device, then uses and closes them.  This is
# the order PipeWire and the mixer daemon produce on a Thunderbolt unplug.
# kmemleak then reports anything ua_apollo allocated and never freed.
# Nothing runs on the host kernel.
#
# Usage: tests/test-hot-unplug-pcm-open.sh [driver-source-dir]
#   KASAN_KERNEL_DIR  built kernel tree with CONFIG_KASAN=y, CONFIG_SND_PCM=y
#                     (default ~/.cache/ua-kasan/linux-7.2.7)
# Exit: 0 pass, 1 fail, 77 skipped (no KASAN kernel or no KVM).
set -uo pipefail
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DRIVER_SRC="${1:-$PROJECT_DIR/driver}"
KDIR="${KASAN_KERNEL_DIR:-$HOME/.cache/ua-kasan/linux-7.2.7}"

skip() { echo "SKIP: $*"; exit 77; }
fail() { echo "FAIL: $*"; exit 1; }

[[ -f $KDIR/arch/x86/boot/bzImage ]] || skip "no kernel image in $KDIR"
grep -a -q '^CONFIG_KASAN=y' "$KDIR/.config" || skip "$KDIR is not a KASAN build"
[[ -w /dev/kvm ]] || skip "/dev/kvm not writable"
command -v qemu-system-x86_64 >/dev/null || skip "qemu-system-x86_64 missing"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir "$work/mod" "$work/fixture" "$work/root"
cp "$DRIVER_SRC"/*.c "$DRIVER_SRC"/*.h "$DRIVER_SRC"/Makefile "$work/mod/"
cp "$PROJECT_DIR/tests/hot-unplug/ua_fixture.c" "$work/fixture/"
echo 'obj-m := ua_fixture.o' >"$work/fixture/Makefile"
for m in mod fixture; do
	make -s -C "$KDIR" M="$work/$m" modules >"$work/build.log" 2>&1 ||
		{ cat "$work/build.log"; fail "$m build"; }
done
gcc -static -O2 -o "$work/root/init" "$PROJECT_DIR/tests/hot-unplug/init.c" ||
	fail "guest init build"
cp "$work/mod/ua_apollo.ko" "$work/fixture/ua_fixture.ko" "$work/root/"
mkdir "$work/root/dev" "$work/root/proc" "$work/root/sys"
(cd "$work/root" && find . | cpio -o -H newc --quiet) >"$work/initramfs"

timeout 600 qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 2G \
	-machine q35 -nographic -no-reboot -vga none \
	-kernel "$KDIR/arch/x86/boot/bzImage" -initrd "$work/initramfs" \
	-append "console=ttyS0 panic=-1 loglevel=7 kasan.fault=report kasan_multi_shot" \
	-device pcie-root-port,id=rp1,chassis=1,slot=1 \
	-device secondary-vga,bus=rp1 \
	>"$work/console.log" 2>&1
log=${UA_TEST_LOG:-$work/console.log}
[[ $log == "$work/console.log" ]] || cp "$work/console.log" "$log"

grep -a 'UA-TEST:' "$work/console.log"
grep -a -q 'UA-TEST: result COMPLETED' "$work/console.log" ||
	fail "guest did not complete (see ${UA_TEST_LOG:-console log})"
grep -a -q 'ua_remove: surprise=' "$work/console.log" ||
	fail "ua_remove never ran"

if grep -a -E -q 'BUG: KASAN|general protection fault|Oops:|kernel BUG' "$work/console.log"; then
	grep -a -E -A12 'BUG: KASAN|general protection fault|Oops:|kernel BUG' "$work/console.log" | head -40
	fail "memory error after removal with files open"
fi
grep -a -q 'UA-TEST: ioctl after removal: No such device' "$work/console.log" ||
	fail "ioctl after removal did not return ENODEV"
grep -a -q 'UA-TEST: kmemleak: [0-9]* unreferenced, 0 from ua_apollo' "$work/console.log" ||
	fail "kmemleak check missing or found ua_apollo leaks"
echo "PASS: removal with files open; no KASAN report, no ua_apollo leak"
