# console-rs on Apple platforms

Native shells for macOS and iPad around the same Rust code that runs on Linux:
`houston` (skins, layouts, images) → `rack` (draw list, hits, input, `MixerModel`) →
`gfx` (wgpu, Metal backend on Apple).

```
apple/
  include/uaconsole.h        C API (hand-written; mirrors crates/uaconsole-ffi)
  include/module.modulemap   lets Swift `import UAConsoleFFI`
  build-xcframework.sh       Rust staticlibs → build/UAConsoleFFI.xcframework
  run-ipad-sim.sh            build, install, copy Resources, launch, screenshot (simulator)
  tests/smoke.c              C smoke test of the header against the macOS slice
  UAConsoleiPad/             SwiftUI app: CAMetalLayer view driven by the C API
  build/                     generated (gitignored): XCFramework, DerivedData, logs
```

UA's art is **never** bundled. The macOS binary reads the installed UAD Console; the iPad
app reads a `Resources` folder the user imports (see below).

## Prerequisites (Mac)

- Xcode 27 with the iOS simulator runtime.
- rustup (Homebrew's `rust` has no iOS standard library). Installing rustup without touching
  shell profiles keeps Homebrew's cargo as the default; the scripts prefer `~/.cargo/bin`:

  ```sh
  curl https://sh.rustup.rs -sSf | RUSTUP_INIT_SKIP_PATH_CHECK=yes sh -s -- -y --no-modify-path
  ~/.cargo/bin/rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin
  ```

## macOS: native binary

```sh
cd console-rs
~/.cargo/bin/cargo build --release -p uaconsole
target/release/uaconsole --render out.png [--compact]   # headless, Metal
```

Resources are found at `/Applications/Universal Audio/UAD Console.app/Contents/Resources`.
The windowed app is the winit `uaconsole` binary. There is no SwiftUI macOS host yet; the
XCFramework has a macOS slice for one.

## The C API (`crates/uaconsole-ffi`)

One opaque `UacContext` per view, used from one thread (the main thread on iOS):

| call | |
|---|---|
| `uac_create(resources, CAMetalLayer*, w_px, h_px, scale)` | NULL layer = offscreen (tests) |
| `uac_resize`, `uac_needs_frame`, `uac_frame`, `uac_invalidate` | render only when dirty |
| `uac_pointer(ctx, DOWN/MOVE/UP/CANCEL, x_pt, y_pt, DOUBLE/FINE)` | drags controls; drags elsewhere pan |
| `uac_set_zoom(ctx, zoom, anchor_x, anchor_y)` | pinch; `zoom <= 0` fits the window |
| `uac_set_compact`, `uac_set_model_preview` | layout and the offline preview model |
| `uac_set_model_engine` | **TODO stub** (returns `UAC_ERR_UNSUPPORTED`): ua-engine adapter |
| `uac_get_stats`, `uac_adapter_name`, `uac_write_png`, `uac_last_error`, `uac_destroy` | |

Pointer coordinates are view points. The context owns the zoom and pan that map them to
Console window points. Rust tests (`cargo test -p uaconsole-ffi`) exercise the C entry points
offscreen on any GPU (Vulkan on Linux), including the coordinate mapping, a solo tap and zoom.

## iPad

### Build the XCFramework

```sh
console-rs/apple/build-xcframework.sh          # ios-arm64, ios-arm64-simulator, macos-arm64
UAC_TARGETS=aarch64-apple-ios-sim console-rs/apple/build-xcframework.sh   # simulator only
```

Each slice is about 200 MB because a staticlib carries every dependency's object code. The
linked app is much smaller (about 15 MB for a Debug simulator build).

### Simulator

```sh
console-rs/apple/run-ipad-sim.sh [shot.png]
# SIM_DEVICE="iPad Pro 13-inch (M5)"  CONSOLE_RESOURCES=…  REBUILD_RUST=1  RECOPY=1  BENCH_FRAMES=240
```

The script builds with `xcodebuild -scheme UAConsoleiPad -destination 'platform=iOS
Simulator,name=…' CODE_SIGN_IDENTITY=-`, then installs the app. It copies the Mac's Console
`appearance`, `layouts`, `images` and `fonts` into the app container's
`Documents/UADConsoleResources`, launches the app (`simctl launch --stderr=`) and runs
`simctl io … screenshot`. The app logs its first frame and, with `BENCH_FRAMES`, warm-frame
averages to `apple/build/ipad-sim-stdout.log`.

Alternatively open `UAConsoleiPad/UAConsoleiPad.xcodeproj` in Xcode (build the XCFramework
first) and run on a simulator.

### Device install

Device builds need your signing identity, and the login keychain is locked in a plain SSH
session. Use one of these:

- Xcode GUI: open the project, choose your team in Signing & Capabilities, pick the iPad and
  press Run.
- Command line, in a session that can unlock the keychain (at the Mac, or `ssh -t`):

  ```sh
  security unlock-keychain ~/Library/Keychains/login.keychain-db
  console-rs/apple/build-xcframework.sh
  cd console-rs/apple/UAConsoleiPad
  xcodebuild -scheme UAConsoleiPad -configuration Release \
    -destination 'generic/platform=iOS' -derivedDataPath ../build/DerivedData \
    DEVELOPMENT_TEAM=<YOUR_TEAM_ID> -allowProvisioningUpdates build
  xcrun devicectl list devices
  xcrun devicectl device install app --device <DEVICE_ID> \
    ../build/DerivedData/Build/Products/Release-iphoneos/UAConsoleiPad.app
  xcrun devicectl device process launch --device <DEVICE_ID> org.openapollo.uaconsole
  ```

  `DEVELOPMENT_TEAM` is deliberately empty in the project. Pass yours on the command line or
  set it in Xcode, but don't commit it.

To get the Resources onto the iPad, tap **Import…** and pick a UAD Console `Resources`
folder (or `UAD Console.app` or its `Contents`) anywhere in Files, e.g. iCloud Drive. The app
copies it into Documents. You can also drop the folder into the app's Documents with Finder
file sharing (iPad → Files → uaconsole) as `UADConsoleResources` or `Resources`.

## Measured (2026-10-11, Mac Studio M2 Ultra, macOS 27.2, Xcode 27.1)

| where | first frame | warm frame |
|---|---|---|
| `uaconsole --render` (full 1920×1050 pt @2×, Metal) | 250–300 ms incl. PNG decode/upload; 1044 quads, 425 batches, 35 textures (10.8 MB), 1073 glyphs | build 0.19 ms |
| `tests/smoke.c` via the C API, offscreen Metal, compact, 2752×2064 px | 211 ms | build 0.15 ms, render 1.26 ms CPU |
| iPad Pro 13" (M5) simulator, CAMetalLayer, compact | 102 ms; 1054 quads, 421 batches, 38 textures, 1035 glyphs | build 0.20 ms, render 8.6 ms CPU (includes the drawable wait), 60 fps |

## Untested / known gaps

- **Physical iPad**: no device build or install yet (it needs signing; see above).
  Also untested: reading `/System/Library/Fonts` from the iOS sandbox. `gfx` loads it on iOS
  because fontdb has no iOS system-font support, and the simulator reads the host's fonts.
- **Touch and pinch in the app**: nobody has tapped the UI yet; simctl cannot inject touches.
  The Rust tests cover the pointer mapping, and the C smoke test covers pan and zoom.
- **The Files import flow** (`fileImporter` → copy) has not been exercised. The simulator
  run copies the folder in with the script.
- Landscape orientation on the simulator (screenshots so far are portrait), Stage Manager
  and resizing, and memory pressure on real hardware. Images decode lazily and the art
  folder is about 200 MB on disk.
- `uac_set_model_engine` is a stub. No engine connection is made from the Apple shells.
- Only arm64 is built: no x86_64 simulator or Intel Mac slices.
