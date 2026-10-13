# uaconsole: a native mixer console for open-apollo

`uaconsole` is a Rust rebuild of UA's UAD Console mixer. It runs natively on Linux, so
open-apollo users no longer need to run the Windows Console under Wine. It talks to the
open-apollo mixer daemon (`mixer-engine/ua_mixer_daemon.py`) on TCP 4710, the same
protocol UA's own Mixer Engine speaks.

> **Experimental.** The mixer window and engine connection are under active development.
> Expect missing features and rough edges.

The console uses UA's art, but **open-apollo does not ship it**. uaconsole reads the art at
run time from your own UAD Console install. See [Where the art comes from](#where-the-art-comes-from).

## Install

### From source (any distribution)

You need Rust 1.89 or newer from [rustup](https://rustup.rs); distribution Rust packages are
often too old. Then, as your normal user (not root):

```bash
bash scripts/install-uaconsole.sh
```

The script:

1. builds `uaconsole` with `cargo build --release`
2. installs it to `~/.local/bin/uaconsole`, along with the `open-apollo-console` launcher
3. adds an **Open Apollo Console** menu entry (`~/.local/share/applications/uaconsole.desktop`)
   with a generic mixer icon
4. checks for the Console art, the fonts and a Vulkan driver, and tells you what is missing

| Option | Effect |
|---|---|
| `--prefix DIR` | install into `DIR/bin` and `DIR/share` instead of `~/.local` |
| `--no-build` | install the binary already built in `console-rs/target/release` |
| `--resources DIR` | copy a Console `Resources` folder to `~/.local/share/open-apollo/console-resources` |
| `--check` | only look for the art, fonts and GPU driver; install nothing |
| `--smoke-test` | after installing, render one frame offscreen to check the GPU and art |
| `--uninstall` | remove the binary, launcher, menu entry and icon (a copied `Resources` folder is kept) |

### Nix

```bash
nix run github:rolotrealanis98/open-apollo#uaconsole
nix profile install github:rolotrealanis98/open-apollo#uaconsole
```

On NixOS, add `inputs.open-apollo.packages.${pkgs.system}.uaconsole` to
`environment.systemPackages`. The package bundles the fonts it needs (Nimbus Sans, Liberation
Sans) and the Vulkan, GL, Wayland and X11 libraries it loads at run time.

On another distribution with Nix installed, programs built by Nix cannot see the system's
GPU drivers. Run uaconsole through [nixGL](https://github.com/nix-community/nixGL), or install it
with `scripts/install-uaconsole.sh` instead.

## Run

1. Start the mixer daemon. The open-apollo installer and the tray's **Restart Daemon** do
   this; by hand:

   ```bash
   python3 mixer-engine/ua_mixer_daemon.py
   ```

2. Open the console in any of these ways:
   - the **Open Apollo Console** menu entry
   - **Open Console** in the open-apollo tray menu
   - the `uaconsole` command
   - the `open-apollo-console` command

uaconsole connects to the local daemon on `127.0.0.1:4710` by default. To use an engine on
another machine, pass `--engine`:

```bash
uaconsole --engine 192.0.2.20        # port 4710
uaconsole --engine studio-pc.local:4710
```

### Native or Wine: the `open-apollo-console` launcher

The tray's **Open Console** item and the `open-apollo-console` command
(`tools/open-apollo-console.sh` in the repo) start uaconsole when it is installed. Without it,
they fall back to UA's Windows UAD Console under Wine. The menu item reads **Open Console
(Wine)** when it will use Wine. The launcher passes `--engine 127.0.0.1:4710` to uaconsole
unless you choose another engine.

| Variable | Meaning |
|---|---|
| `OPEN_APOLLO_CONSOLE` | `auto` (default), `native` or `wine` |
| `UACONSOLE_BIN` | uaconsole binary to use (default: next to the launcher, then `uaconsole` on `PATH`, then `~/.local/bin/uaconsole`) |
| `UACONSOLE_ENGINE` | engine for uaconsole (default `127.0.0.1:4710`) |
| `OPEN_APOLLO_WINE_CONSOLE` | executable that starts the Windows Console under Wine, such as your own script that installs a rendering shim first. Without it, the launcher runs `wine "UAD Console.exe"` from the prefix, which may open a blank window on stock Wine. |
| `WINEPREFIX` | Wine prefix with UAD Console installed (default `~/wine-ua`) |

`open-apollo-console --which` prints which console it would start. The tray writes the
console's output to `~/.cache/open-apollo/console.log`.

## Where the art comes from

UA's images, layouts, `Appearance.json` and Console fonts are **not** in this repository or in
any package built from it, and uaconsole never downloads them. It reads them at run time from
the UAD Console that you installed, in this order:

1. `$CONSOLE_RESOURCES`
2. an imported or copied folder: `~/.local/share/open-apollo/console-resources`
   (`oa-assets import` puts it there; see below)
3. macOS: `/Applications/Universal Audio/UAD Console.app/Contents/Resources`
4. a Wine install: `$WINEPREFIX` (default `~/wine-ua`) +
   `drive_c/Program Files (x86)/Universal Audio/UAD Console/Resources`

Plug-in icons come from `Resources/Plug-In Icons` (Windows) or
`/Library/Application Support/Universal Audio/Plug-In Icons` (macOS). They are optional.

If you have none of these, get the `Resources` folder from your own UAD Console install:

- **Windows partition (dual boot):** mount it read-only and let `oa-assets` find UAD Console:
  `oa-assets scan`, then `oa-assets import`. `oa-assets mount /dev/…` mounts a partition
  read-only for you.
- **Windows VM:** install UAD in the VM, shut Windows down, then
  `oa-assets attach <raw disk image>` (Omarchy: `~/.windows/data.img`), `oa-assets import`, and
  `oa-assets detach <image>`.
- **Wine:** install UA's Windows UAD Console into a Wine prefix (open-apollo's Wine tooling uses
  `~/wine-ua`). uaconsole finds it there. For another prefix, set `WINEPREFIX`.
- **Copy:** copy the `Resources` folder from a Windows computer
  (`C:\Program Files (x86)\Universal Audio\UAD Console\Resources`) or a Mac
  (`/Applications/Universal Audio/UAD Console.app/Contents/Resources`), then run
  `bash scripts/install-uaconsole.sh --resources /path/to/Resources`.

The copy is for your own use on your own machine. Don't share it or commit it.
`bash scripts/install-uaconsole.sh --check` shows what uaconsole will find.

**Firmware.** The driver's input strips and the USB init need UA's Console strip DSP modules,
and USB Apollos need UA's FX3 boot images. open-apollo ships neither: `sudo oa-assets firmware
--from <Windows root>` reads them from your UAD install (any root `scan` accepts; without
`--from` it takes the first one found) and writes `dsp/<id>.bill` (0a5 0c2 0da 0db 0eb 12b
0c9, the on-disk records unmodified), `ApolloSolo.bin`, `ApolloTwin.bin`, `ApolloTwinX.bin`
and `manifest.json` to `/lib/firmware/universal-audio` (`--out DIR` puts them elsewhere).
Only versions whose SHA-256 is in `crates/oa-assets/data/firmware-pins.json` (hashes only, no UA
bytes) are written unless you pass `--accept-unknown`. Missing USB images are only a warning,
since PCIe Apollos don't need them. Exit codes: 0 done, 1 a required module missing, 2 an
unknown module version. Maintainers check a new UAD release with `oa-assets firmware --from
<root> --verify-golden`, which re-extracts into a temp dir and compares every file with the
pins. Like the Console art, these files are for your own machine only.

## Requirements

- **GPU:** rendering uses [wgpu](https://wgpu.rs). It prefers Vulkan and falls back to OpenGL ES
  3.0 through EGL. Mesa (AMD, Intel) and NVIDIA's driver both work. Software rendering
  (llvmpipe, lavapipe) works but is slow.
- **Display:** Wayland or X11.
- **Fonts:** Console's Helvetica and Arial labels are drawn with the metric-compatible
  **Nimbus Sans** (URW base35) and **Liberation Sans**:

  | Distribution | Packages |
  |---|---|
  | Debian, Ubuntu | `fonts-urw-base35 fonts-liberation` |
  | Fedora | `urw-base35-nimbus-sans-fonts liberation-sans-fonts` |
  | Arch | `gsfonts ttf-liberation` |
  | openSUSE | `urw-base35-fonts liberation-fonts` |

  Console's bundled fonts (such as Oxygen) are loaded from its `Resources/fonts`.
- **Console art:** see [above](#where-the-art-comes-from).
- **Mixer engine:** the open-apollo daemon on TCP 4710, or UA's Mixer Engine on another machine.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `UAD Console resources not found` | Run `bash scripts/install-uaconsole.sh --check`, then see [Where the art comes from](#where-the-art-comes-from). Point uaconsole at any folder with `CONSOLE_RESOURCES=/path/to/Resources`. |
| `no GPU adapter`, or a black window | Check that `vulkaninfo --summary` lists your GPU, and install your distribution's Vulkan driver (`mesa-vulkan-drivers`, `vulkan-radeon`, `vulkan-intel`). Try another backend with `WGPU_BACKEND=gl` or `WGPU_BACKEND=vulkan`. `--render` reads this variable; the window may not yet. |
| Labels look too wide or narrow, or use the wrong typeface | Install Nimbus Sans and Liberation Sans (table above). Check with `fc-list "Nimbus Sans"`. |
| Nothing moves, or no connection | Check that the daemon is listening: `ss -tln \| grep 4710`. Start it, or use **Restart Daemon** in the tray. |
| Nix build on a non-NixOS system finds no GPU | Use nixGL (see [Nix](#nix)). |
| Wayland issues | Force X11 for one run: `env -u WAYLAND_DISPLAY uaconsole`. |
| The tray offers **Open Console (Wine)** although uaconsole is installed | The tray could not find `uaconsole` on its `PATH` or in `~/.local/bin`. Set `UACONSOLE_BIN`, or log out and back in after the install. |

## Development

The workspace layout and frame pipeline are described in [ARCHITECTURE.md](ARCHITECTURE.md).

```bash
cd console-rs
cargo build --release
cargo run --release -p uaconsole -- --render /tmp/console.png
cargo test --workspace
```

With Nix, `nix develop` (from the repo root) provides the Rust toolchain, clippy, rustfmt,
rust-analyzer and Python. It also sets `LD_LIBRARY_PATH` and `FONTCONFIG_FILE` so that
`cargo run` finds the GPU, windowing libraries and fonts.

### Command-line flags

`uaconsole --help` is the authoritative list. `--render` (with `--compact` and `--scale`) is
available now; the other flags come with the windowed app shell.

| Flag | Purpose |
|---|---|
| `--render out.png [--compact] [--scale N]` | Headless render of one frame to a PNG: no window, no engine. Use it to compare against reference screenshots. |
| `--snapshot f.json` | With `--render`, draw a saved engine snapshot instead of the built-in preview data. |
| `--bench` | Headless timing of frame build and draw. |
| `--engine HOST[:PORT]` | Engine to connect to. The default is `127.0.0.1:4710`, the local open-apollo daemon; the port defaults to 4710. |
| `--mock` | Run without an engine, using the built-in preview mixer, for UI work. |

To test against a daemon that cannot touch hardware, run it software-only on a spare port:

```bash
python3 mixer-engine/ua_mixer_daemon.py --no-hardware --no-bonjour --port 4711 -v
uaconsole --engine 127.0.0.1:4711
```

The engine connection is read-only by default. Tests that write use a software-only daemon on
a spare port, never a daemon or engine that drives real hardware.

### Tests in CI

`.github/workflows/console-rs.yml` runs on every change under `console-rs/`, `flake.nix` or
`nix/`:

- `cargo build --workspace --all-targets --locked` and `cargo test --workspace --locked` on
  Ubuntu. These must pass. Keep `Cargo.lock` committed and current.
- `cargo fmt --check` and `cargo clippy`. These are advisory for now and do not fail the check.
- `nix flake check`, which builds `packages.uaconsole` and runs the same tests in the Nix sandbox.

The CI runners have **no GPU and no UA resources**, and neither does the Nix build sandbox. A
test that needs either must detect that and skip itself at run time, not fail:

```rust
#[test]
fn renders_strip() {
    let Some(gpu) = gfx::Headless::new() else {
        eprintln!("skipped: no GPU adapter");
        return;
    };
    let Some(resources) = houston::Resources::locate() else {
        eprintln!("skipped: no Console resources (set CONSOLE_RESOURCES)");
        return;
    };
    // ...
}
```

Don't assume the opposite either: a runner may expose a software adapter such as llvmpipe, so
GPU tests must also pass on one. Pure CPU code (layout, `DrawList` building, protocol, tapers)
should be tested without either; that is most of `houston`, `rack` and `ua-engine`. Never commit
UA files as test fixtures. Tests that need them read them from the developer's own install,
like `houston/tests/oracle.rs`.

## License

GPL-2.0-only, like the rest of open-apollo. UA's art is not covered by this license and is not
distributed; see [NOTICE.md](../NOTICE.md).
