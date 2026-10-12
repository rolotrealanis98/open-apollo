#!/bin/sh
# Builds UAConsoleiPad for the iOS simulator, installs it, copies a UAD Console Resources
# folder into the app's Documents/UADConsoleResources, launches it and saves a screenshot.
#
#   apple/run-ipad-sim.sh [screenshot.png]
#
# Env:
#   SIM_DEVICE         simulator name or UDID (default "iPad Pro 13-inch (M5)")
#   CONSOLE_RESOURCES  Resources folder to copy (default: the installed macOS UAD Console)
#   REBUILD_RUST=1     rebuild the XCFramework first (it is built when missing anyway)
#   RECOPY=1           replace an existing Documents/UADConsoleResources copy
#   BENCH_FRAMES=N     after the first frame, redraw N warm frames and log the averages
set -eu

here=$(cd "$(dirname "$0")" && pwd)
device=${SIM_DEVICE:-"iPad Pro 13-inch (M5)"}
resources=${CONSOLE_RESOURCES:-"/Applications/Universal Audio/UAD Console.app/Contents/Resources"}
shot=${1:-"$here/build/ipad-sim.png"}
bundle=org.openapollo.uaconsole
derived="$here/build/DerivedData"
mkdir -p "$here/build"

if [ "${REBUILD_RUST:-0}" = 1 ] || [ ! -d "$here/build/UAConsoleFFI.xcframework" ]; then
	"$here/build-xcframework.sh"
fi

echo "==> xcodebuild (iOS Simulator, $device)"
xcodebuild -project "$here/UAConsoleiPad/UAConsoleiPad.xcodeproj" -scheme UAConsoleiPad \
	-configuration Debug -destination "platform=iOS Simulator,name=$device" \
	-derivedDataPath "$derived" CODE_SIGN_IDENTITY=- build >"$here/build/xcodebuild.log" 2>&1 ||
	{
		grep -E "error:|warning: .*Swift" "$here/build/xcodebuild.log" | head -40
		echo "xcodebuild failed; see $here/build/xcodebuild.log"
		exit 1
	}
app="$derived/Build/Products/Debug-iphonesimulator/UAConsoleiPad.app"

echo "==> boot + install"
xcrun simctl boot "$device" 2>/dev/null || true
xcrun simctl bootstatus "$device" -b >/dev/null
xcrun simctl install "$device" "$app"

data=$(xcrun simctl get_app_container "$device" "$bundle" data)
dest="$data/Documents/UADConsoleResources"
if [ "${RECOPY:-0}" = 1 ] || [ ! -f "$dest/appearance/Appearance.json" ]; then
	echo "==> copying Resources into $dest"
	rm -rf "$dest"
	mkdir -p "$dest"
	for sub in appearance layouts images fonts; do
		[ -d "$resources/$sub" ] && ditto "$resources/$sub" "$dest/$sub"
	done
	[ -f "$resources/../Info.plist" ] && cp "$resources/../Info.plist" "$dest/Info.plist"
fi

echo "==> launch"
xcrun simctl terminate "$device" "$bundle" 2>/dev/null || true
log="$here/build/ipad-sim-stdout.log"
: >"$log"
SIMCTL_CHILD_UAC_BENCH_FRAMES="${BENCH_FRAMES:-0}" \
	xcrun simctl launch --terminate-running-process --stdout="$log" --stderr="$log" "$device" "$bundle"
sleep "${SHOT_DELAY:-5}"
xcrun simctl io "$device" screenshot "$shot"
echo "==> screenshot: $shot"
cat "$log" 2>/dev/null || true
