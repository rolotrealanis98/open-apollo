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
| `uac_resize`, `uac_needs_frame`, `uac_frame`, `uac_invalidate` | call `uac_needs_frame` every display refresh: it pumps the engine; render only when it says so |
| `uac_set_window_size(ctx, w_pt, h_pt)` | pin the Console window (e.g. 1920×1050); `<= 0` follows the view (default) |
| `uac_pointer(ctx, DOWN/MOVE/UP/CANCEL/SECONDARY, x_pt, y_pt, DOUBLE/FINE)` | rack `Interaction` routed through `ViewState`; taps fire on release (so long press = CANCEL + SECONDARY); drags elsewhere pan; `uac_dragging` while a control drag is active |
| `uac_scroll(ctx, x, y, dx, dy)` | browser list / cells column under the point, else pan |
| `uac_text_editing`, `uac_key_text`, `uac_key(ENTER/BACKSPACE/ESCAPE)` | rename, tempo, browser search; Escape closes popovers |
| `uac_set_zoom(ctx, zoom, anchor_x, anchor_y)` | pinch; `zoom <= 0` fits the window |
| `uac_set_compact` | browser + navigator hidden, aux/monitor packed after the strips |
| `uac_connect_engine(ctx, host, port, writes)` | `engine_model::EngineMixer` live over ua-engine (+ the 4720 helper); **read-only unless `writes`** |
| `uac_use_snapshot(ctx, path)`, `uac_set_model_preview` | offline models |
| `uac_model_state`, `uac_model_status` | `UAC_MODEL_LIVE` …; "Apollo x4 · live · read-only" |
| `uac_take_plugin_request(ctx, &req)` | a tap on a loaded insert/Unison: strip, slot, engine slot path, `EffectName` for the host's plug-in window |
| `uac_get_stats`, `uac_adapter_name`, `uac_write_png` (+ `.draws.json`), `uac_last_error`, `uac_destroy` | |

Pointer coordinates are view points. The context owns the zoom and pan that map them to
Console window points.

Layout follows the view like Console's window does: a 1470×1105 pt view lays out exactly like
a 1470×1105 pt Console window (zoom 1). Compact takes its width from the strips and its height
from the window.

Engine connections run on their own threads. `uac_needs_frame` applies queued updates
(`MixerModel::pump`) and asks for a frame when values change, or at most every 33 ms while
meters move. Read-only, the connection refuses `set`/`post` and the writer gate stays closed:
controls move locally and nothing is sent. `uac_set_model_engine(…, UAC_ENGINE_WRITES)` is the
older flag form of the same call.

Rust tests (`cargo test -p uaconsole-ffi`) exercise the C entry points offscreen on any GPU
(Vulkan on Linux): coordinate mapping, a solo tap, zoom, window-size following, navigator and
rename routing through `ViewState`, plug-in requests, a snapshot, and a read-only connection to
ua-engine's `MockEngine` (a fader drag sends nothing).

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

## UA's HTML plug-in GUIs in WKWebView (`ua-content://`)

UAD Console 1.3.1 loads a lunacomponent GUI as
`ua-content://gui/index.html?host=<h>&port=<p>&path=<insert path>&plugin_id=<component id>`
and answers the scheme with one `WKURLSchemeHandler` per web view (`UAPWSchemeHandler`,
registered with `setURLSchemeHandler:forURLScheme:`). An Apple host (ConsoleLink, or a SwiftUI
shell over `uaconsole-ffi`) should do the same, so the page sees the origin and URL keys it sees
in Console. The rules are in `crates/plugin-ui/WEBBRIDGE.md` §8, and `plugin_ui::content` is
the Rust version used on Linux.

- **Root.** The component's `<id>.lunacomponent/gui.bundle/Contents/Resources/html` folder.
  Ignore the URL host (`gui`), take the percent-decoded path, standardize it, and serve it only
  if it stays inside the root.
- **Reply.** Status 200, `HTTP/1.1`, `Content-Type` by extension (default
  `application/octet-stream`), `Content-Length`, `Access-Control-Allow-Origin: *`,
  `Cache-Control: no-cache`. Read off the main thread, and deliver only while the task is
  still active: calling a stopped task throws. Fail anything else with
  `URLError(.fileDoesNotExist)`.
- **Keys.** `host` first (the client's `getURLParams` glues the first key to the URL), `port`
  = a UANet WebSocket endpoint (plugin-ui's bridge, or the server that owns the insert),
  `path` = the insert node with no trailing `/`, `plugin_id` last.
- **Registration.** A scheme handler can only be set on a `WKWebViewConfiguration` before the
  view is created, and `ua-content` is not a scheme WebKit handles itself.
- `uaconsole-ffi` draws the mixer only; it creates no web views. A tap on a loaded insert
  queues a `UacPluginRequest` (`uac_take_plugin_request`): `slot_path` is the `path` key and
  `effect_name` picks the lunacomponent (its id is `plugin_id`). The Swift host owns the
  `WKWebView`, the handler, the window placement and the UANet endpoint for `port`.

```swift
import WebKit

/// Serves ua-content://gui/<file> from one lunacomponent's html folder (UAD Console 1.3.1's rules).
final class UAContentSchemeHandler: NSObject, WKURLSchemeHandler {
    private let root: URL
    private var active = Set<ObjectIdentifier>()
    private static let types = [
        "html": "text/html", "htm": "text/html", "js": "text/javascript", "mjs": "text/javascript",
        "css": "text/css", "json": "application/json", "map": "application/json",
        "png": "image/png", "jpg": "image/jpeg", "jpeg": "image/jpeg", "gif": "image/gif",
        "svg": "image/svg+xml", "webp": "image/webp", "ico": "image/x-icon", "woff": "font/woff",
        "woff2": "font/woff2", "ttf": "font/ttf", "otf": "font/otf", "wasm": "application/wasm",
        "mp4": "video/mp4", "webm": "video/webm", "mp3": "audio/mpeg", "wav": "audio/wav",
        "txt": "text/plain", "xml": "application/xml", "pdf": "application/pdf",
    ]

    init(htmlFolder: URL) { root = htmlFolder.standardizedFileURL }

    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        guard let url = task.request.url, let file = resolve(url) else {
            return task.didFailWithError(URLError(.fileDoesNotExist))
        }
        let id = ObjectIdentifier(task)
        active.insert(id)
        DispatchQueue.global(qos: .userInitiated).async {
            let data = try? Data(contentsOf: file)
            DispatchQueue.main.async { [self] in
                guard active.remove(id) != nil else { return } // stopped meanwhile
                guard let data else { return task.didFailWithError(URLError(.fileDoesNotExist)) }
                let headers = [
                    "Content-Type": Self.types[file.pathExtension.lowercased()] ?? "application/octet-stream",
                    "Content-Length": String(data.count),
                    "Access-Control-Allow-Origin": "*",
                    "Cache-Control": "no-cache",
                ]
                task.didReceive(HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: headers)!)
                task.didReceive(data)
                task.didFinish()
            }
        }
    }

    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {
        active.remove(ObjectIdentifier(task))
    }

    /// The URL's path (decoded; the host is ignored), standardized, and only inside the root.
    private func resolve(_ url: URL) -> URL? {
        let path = URLComponents(url: url, resolvingAgainstBaseURL: false)?.path ?? ""
        let file = root.appendingPathComponent(path.isEmpty ? "index.html" : path).standardizedFileURL
        return file.path.hasPrefix(root.path + "/") ? file : nil
    }
}

// One handler per web view, set before the view exists.
let config = WKWebViewConfiguration()
config.setURLSchemeHandler(UAContentSchemeHandler(htmlFolder: htmlFolder), forURLScheme: "ua-content")
let web = WKWebView(frame: CGRect(x: 0, y: 0, width: 950, height: 175), configuration: config) // manifest.json "window"
var page = URLComponents(string: "ua-content://gui/index.html")!
page.queryItems = [ // host first
    URLQueryItem(name: "host", value: "127.0.0.1"),
    URLQueryItem(name: "port", value: String(bridgePort)),
    URLQueryItem(name: "path", value: "/devices/0/inputs/0/effects/0"),
    URLQueryItem(name: "plugin_id", value: "uaudio_ua_1176_rev_a"),
]
web.load(URLRequest(url: page.url!))
```

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
- The UAConsoleiPad sample still shows the preview model; ConsoleLink's Console 1:1 screen
  (`feat/console-1to1`) is the host that connects to an engine.
- Only arm64 is built: no x86_64 simulator or Intel Mac slices.
