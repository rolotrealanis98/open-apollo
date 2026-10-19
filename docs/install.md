# Installing open-apollo

open-apollo needs two things from you: a supported Apollo, and **a UAD install you already
have**, because it includes no Universal Audio files. The setup wizard does everything else.

- [Before you start](#before-you-start)
- [Setup wizard](#setup-wizard) (Thunderbolt Apollos; recommended)
- [Command-line install](#command-line-install) (step by step, or without a desktop)
- [USB Apollos](#usb-apollos)
- [Plug-ins](#plug-ins)
- [Updating, reinstalling and uninstalling](#updating-reinstalling-and-uninstalling)
- [Where things go](#where-things-go)
- [Troubleshooting](#troubleshooting)

## Before you start

| You need | Notes |
|---|---|
| An Apollo | Thunderbolt 3/4 models on a Thunderbolt 3/4 or USB4 port, or a USB model. Thunderbolt 2 models don't enumerate on Linux; see the [README](../README.md#thunderbolt-2-devices-not-supported). |
| A Linux distribution | Ubuntu 24.04+, Fedora 40+, Arch, openSUSE Tumbleweed, Linux Mint 22, Pop!_OS 24.04 or Manjaro. Kernel 6.8 or newer is the safe choice. |
| A UAD install | On a Windows partition, in a Windows VM, in a Wine prefix, or copied from a Windows PC: see [Getting a UAD install](uad-install.md). |
| Rust 1.89 or newer | From [rustup](https://rustup.rs): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh`. Distribution Rust packages are often too old. |
| Build tools | A C compiler and `make` (`build-essential`, `gcc make`, `base-devel`). The installer adds kernel headers, DKMS and the audio packages it needs. |
| Disk space | About 3 GB, mostly for the plug-in panels' art. |
| Network | For distribution packages, and for the plug-in panel specs (about 1 MB from [open-apollo-panels](https://github.com/rolotrealanis98/open-apollo-panels)). |

Most Intel systems also need the `iommu=pt` kernel parameter. The installer adds it to GRUB
when it's needed, and you restart afterwards.

## Setup wizard

From a terminal in your desktop session, as your normal user:

```bash
git clone https://github.com/rolotrealanis98/open-apollo.git
cd open-apollo
bash scripts/setup.sh
```

`scripts/setup.sh` checks for Rust and a C compiler, builds the wizard
(`open-apollo-setup`) and the importer (`oa-assets`), and starts the wizard. The first build
takes a few minutes. `bash scripts/setup.sh --demo` walks through every page without changing
anything.

The wizard's pages:

1. **System check.** A read-only look at your kernel, its headers, Secure Boot, Thunderbolt
   and your Apollo, with what to fix first.
2. **Find your UAD install.** It lists Windows partitions (mounted or not), Wine prefixes and
   VM disk images in the usual places (Omarchy, GNOME Boxes, libvirt). You can also pick a VM
   disk image or a folder. See
   [Getting a UAD install](uad-install.md).
3. **Review and install.** It shows where everything goes and lists what an earlier
   open-apollo install left. The earlier install is removed first, unless you tick **Keep my
   current install**. The report box sends an anonymous install report and is off by default;
   with it, a second box (on by default) adds the plug-ins your Apollo says are authorized. The
   page says exactly what each holds; see [Install logs and the install report](#install-logs-and-the-install-report).
   **Install** asks for your password once.
4. **The install itself.** One privileged run (`scripts/setup-root.sh`) does the next four
   pages in order and stops at the first failure:
   - **Remove previous install**: `uninstall.sh --yes --purge`.
   - **Import assets**: copies UAD Console's art, installs the plug-in panel specs and makes
     the panels' art. The panel specs and art are optional: without them, plug-in windows show
     a plain parameter list.
   - **DSP programs & firmware**: extracts them to `/lib/firmware/universal-audio`, and
     stores every plug-in's DSP modules in its `plugins/` folder for a later release.
   - **Driver**: builds the driver with DKMS and installs the audio settings and the mixer
     daemon (`install.sh --yes`).

   If your UAD install can't be used, it stops before anything is changed.
5. **Console.** Builds and installs the mixer console (`uaconsole`) as you, with no password,
   and restarts the mixer daemon.
6. **Done.** Start the Console from here or from your app menu. Restart first if the driver
   step asked you to (for `iommu=pt`, or a module Secure Boot must accept). This page, and any
   page where a step failed, shows the run's setup log to attach to a bug report.

**Secure Boot.** If Secure Boot is on and refuses the driver, the installer explains how to
enroll your DKMS signing key with `mokutil`. It never runs `mokutil` itself.

## Command-line install

The same steps by hand, for a system without a desktop or to see each one. Build the importer
first:

```bash
cargo build --release --manifest-path console-rs/Cargo.toml -p oa-assets
OA=$PWD/console-rs/target/release/oa-assets
```

Then find your UAD install. `$OA scan` lists what it can see. A Windows partition can be
mounted read-only with `$OA mount /dev/nvme0n1p3`. A shut-down VM's raw disk image can be
attached with `$OA attach windows.img`, and released afterwards with `$OA detach windows.img`.
Below, `/path/to/windows` is the folder that holds Windows' `C:\` (or a Wine prefix).

```bash
sudo $OA firmware --from /path/to/windows      # DSP modules, USB boot images -> /lib/firmware/universal-audio
sudo $OA dsp --from /path/to/windows           # plug-ins' DSP modules -> /lib/firmware/universal-audio/plugins (optional)
$OA import --from /path/to/windows             # UAD Console's art, plug-in icons and plug-in list -> ~/.local/share/open-apollo
sudo bash scripts/install-panels.sh            # plug-in panel specs -> /usr/share/open-apollo/plugin-specs
$OA plugins --from /path/to/windows            # plug-in panels' art -> ~/.local/share/open-apollo/generated
sudo bash scripts/install.sh                   # driver, DKMS, audio settings, mixer daemon
bash scripts/install-uaconsole.sh              # the mixer console, for you
```

`install.sh` walks you through power-cycling the Apollo for a clean Thunderbolt start, checks
the driver and DSP, and plays a test tone. `--yes` asks nothing: it takes every question's
default answer and skips the power-cycle pause and the test tone. `--skip-init` skips the
hardware initialization and `--no-dkms` skips the DKMS setup. Each script leaves a log of
its run and says where at the end (see below).

## USB Apollos

The wizard installs the Thunderbolt driver. For an Apollo Solo USB, extract UA's files and run
the USB installer:

```bash
sudo $OA firmware --from /path/to/windows      # the USB boot images and DSP modules it loads
sudo bash scripts/install-usb.sh
```

`install-usb.sh` installs the dependencies, builds a patched `snd-usb-audio`, and sets up the
init that uploads the boot image and starts the DSP each time the Apollo is plugged in. It
also installs the PipeWire configuration. It stops before touching your audio stack if the
boot image or the DSP modules are missing. For the Console, also run the `import`, panels,
`plugins` and `install-uaconsole.sh` lines from the command-line install above.

Twin USB and Twin X USB are untested.

## Plug-ins

What this release does with UAD plug-ins:

| | |
|---|---|
| Plug-in browser | The Console's plug-in list, icons and factory preset menus come from your UAD install (icons from its Plug-In Icons folder or, if UAD never saw an Apollo, from the `apollo-32bit.msi` in Windows' installer cache; `oa-assets icons` repeats just that step) (`~/.local/share/open-apollo/ua-catalogue/`, names only). Without that import the browser is empty: run `oa-assets catalogue --from /path/to/windows`, then Settings › Plug-Ins › RESCAN in the Console. |
| Plug-in windows | The Console draws each plug-in's window from the panel specs and the art setup made from your Powered plug-ins: 206 of 224 with a UAD 12 install, the rest as a plain parameter list. |
| Plug-in DSP modules | Setup stores them in `/lib/firmware/universal-audio/plugins/`. Only plug-in hosting (below) loads them. |
| Running plug-ins on the Apollo's DSP | Experimental and off by default; see below. Only plug-ins your Apollo itself reports as authorized load. |

You don't need plug-in licences for the browser and the windows. To rebuild the panels' art
alone, for example after a UAD update, run `oa-assets plugins --from /path/to/windows`;
`oa-assets status` says what is there.

### Running plug-ins on the Apollo (experimental)

open-apollo can insert UAD plug-ins on the Apollo's analog inputs, running on its DSP like UA
Console does. open-apollo ships no plug-in data for this: your computer builds each plug-in's
control mapping by running your own Powered plug-in DLLs offline (an x86 emulator; no Wine, no
device, no network). Turn it on with the **Plug-ins on the Apollo (experimental)** box on the
wizard's Review page, or from a terminal:

```bash
sudo bash scripts/install.sh --plugin-hosting=/path/to/windows
```

Either way the driver loads with `plugin_host=1 input_strips=1` (written to
`/etc/modprobe.d/open-apollo-plugin-hosting.conf`), then `scripts/setup-plugin-hosting.sh`
runs as you: it installs numpy, unicorn, capstone and pefile into
`~/.local/share/open-apollo/plugin-venv`, copies the plug-in DLLs it reads into
`~/.local/share/open-apollo-plugin-maps/source`, restarts the mixer daemon with
`--native-plugin-bundles`, and starts mapping in the background (the user service
`open-apollo-plugin-maps`, one worker per CPU core but one).

- Only the plug-ins your Apollo authorizes are mapped (the device's own licence answer). When a
  licence appears later the daemon maps it by itself; Settings > Plug-Ins > **RESCAN** asks at once.
- Setup shows each plug-in as it is mapped (`Mapping 12/37: UAD Pultec EQP-1A…`, a count and the
  time left) for a couple of minutes and then lets you finish; mapping goes on, and each plug-in
  appears in the Console's plug-in browser as soon as its maps are built. Progress:
  `tail -f ~/.local/share/open-apollo-plugin-maps/build.log`.
- Maps are kept: `uninstall.sh --purge` leaves them (`--purge-plugin-maps` deletes them), so a
  reinstall reuses them, and after a UAD update only the plug-ins whose DLL changed are mapped
  again (rerun the script to copy the new DLLs). `--remove` turns hosting off.

What to expect:
- The Console lists the plug-ins `mixer-engine/device_maps/native_known_good.json` marks as
  live-tested on an Apollo x4 at 48 kHz in both layouts: mono on any input and stereo on a
  linked pair, up to four per input. `--all` maps every installed plug-in, and the daemon's
  `--advertise-all-plugins` offers them too; those are untested.
- 48 kHz only. The daemon runs each plug-in's own code to plan control changes, from the copy in
  `~/.local/share/open-apollo-plugin-maps/source`: the Windows partition need not stay mounted.
- The plug-ins run with UA's own DSP code; a refused licence leaves the input as it was.
- The bundles are made from UA's code on your computer. Don't share or publish them.

## Updating, reinstalling and uninstalling

To update, pull the repository and run `bash scripts/setup.sh` again. setup always reinstalls
cleanly: it removes the earlier install and puts everything back from your UAD install.

To remove open-apollo:

```bash
sudo bash scripts/uninstall.sh            # keeps the extracted firmware and your imported art
sudo bash scripts/uninstall.sh --purge    # removes those too
bash scripts/uninstall.sh --list          # what is installed (no root needed)
```

It removes every `ua_apollo` DKMS version and the modules built from them, along with:
- the mixer daemon's units;
- the PipeWire, WirePlumber and UCM2 settings and the udev rules;
- the Console and the plug-in panel specs;
- a USB install.

It ends with a check that lists anything still there. `--yes` asks nothing.

## Where things go

| What | Where | Removed by |
|---|---|---|
| Driver (DKMS) | `/usr/src/ua_apollo-*`, `/lib/modules/*/updates/dkms/` | `uninstall.sh` |
| DSP modules, USB boot images (from your UAD install) | `/lib/firmware/universal-audio/` | `uninstall.sh --purge` |
| Plug-ins' DSP modules (from your UAD install) | `/lib/firmware/universal-audio/plugins/` | `uninstall.sh --purge` |
| UAD Console's art (from your UAD install) | `~/.local/share/open-apollo/console-resources/` | `uninstall.sh --purge` |
| Plug-in list and factory preset menus (from your UAD install) | `~/.local/share/open-apollo/ua-catalogue/` | `uninstall.sh --purge` |
| Plug-in panels' art (made from your UAD install) | `~/.local/share/open-apollo/generated/` | `uninstall.sh --purge` |
| Plug-in panel specs | `/usr/share/open-apollo/plugin-specs/` | `uninstall.sh` |
| Plug-in hosting: driver options | `/etc/modprobe.d/open-apollo-plugin-hosting.conf` | `uninstall.sh` |
| Plug-in hosting: maps and the DLLs they are made from (from your UAD install) | `~/.local/share/open-apollo-plugin-maps/` | `uninstall.sh --purge-plugin-maps` |
| Plug-in hosting: its Python and scratch | `~/.local/share/open-apollo/plugin-venv/`, `~/.cache/open-apollo/plugin-mapper/` | `uninstall.sh --purge` |
| Plug-in hosting: mapping service | systemd user unit `open-apollo-plugin-maps` | `uninstall.sh` |
| Mixer daemon | systemd user unit `ua-mixer-daemon` | `uninstall.sh` |
| Console | `~/.local/bin/uaconsole`, menu entry **Open Apollo Console** | `uninstall.sh` |
| Audio settings | `/etc/pipewire`, `/etc/wireplumber`, `/usr/share/alsa/ucm2`, `/etc/udev/rules.d` | `uninstall.sh` |
| Install logs | `~/.local/state/open-apollo/*.log` (`/var/log/open-apollo/` for a root run with no desktop user) | kept; delete them when you like |

Nothing from your UAD install leaves your computer. setup downloads only the plug-in panel
specs, and sends an install report only when you tick the box. The report has no user name,
host name, home path or serial numbers.

## Install logs and the install report

**Logs.** Every run leaves one log you can attach to a bug report, kept after the run:

| Run | Log |
|---|---|
| The setup wizard (everything it ran, the Console build included) | `~/.local/state/open-apollo/setup-<date>-<time>.log` |
| `sudo bash scripts/setup-root.sh` | `~/.local/state/open-apollo/setup-<date>-<time>.log` (the `--user`'s) |
| `sudo bash scripts/install.sh` | `~/.local/state/open-apollo/install-<date>-<time>.log` (the user who ran sudo) |
| `bash scripts/install-uaconsole.sh` | `~/.local/state/open-apollo/uaconsole-<date>-<time>.log` |
| `bash scripts/setup-plugin-hosting.sh` | `~/.local/state/open-apollo/plugin-hosting-<date>-<time>.log` |
| `sudo bash scripts/uninstall.sh` | `~/.local/state/open-apollo/uninstall-<date>-<time>.log` |

Dates and times are UTC. A script run from another one writes into its parent's log, so the
wizard's log holds the driver install, the importer, the firmware extraction and the plug-in
mapping too. Without a desktop user (a root login) the log goes to `/var/log/open-apollo/`.
Only you can read your logs (mode 600). Before a line is written, `scripts/oa_redact.py` takes
out your user name, home folder, host name, machine id, serial numbers and MAC addresses, and
the terminal colours. The logs hold the scripts' output only: file names and checksums, never
the contents of your UAD install. Read a log before you attach it all the same.
`OA_NO_LOG=1` turns logging off.

**The install report** is sent only when you tick the box (the wizard) or pass
`--send-report` (`install.sh`, `setup-root.sh`); otherwise it stays in
`/tmp/open-apollo-install-report.json`. It holds your distro, kernel, CPU, RAM, PCI and sound
devices, PipeWire's Apollo devices (other devices only counted), each install step's result
and the driver's kernel log lines, redacted the same way as the logs. It goes to
`open-apollo-api.rolotrealanis.workers.dev`.

**Authorized plug-ins**, in a report you send: so open-apollo can verify first the plug-ins
people own, the report also lists the render ids (UA's plug-in product numbers, such as 2276)
of the plug-ins your Apollo itself says are authorized, and which of those built for native
hosting on this computer. It is your Apollo's own answer, read through the mixer daemon or one read-only
query (`mixer-engine/plugin_report.py`), so it is there only when the driver runs with plug-in
hosting on. Nothing else about plug-ins is sent: no demo or licence details, no serial number,
account, plug-in names or files. Leave it out with the wizard's second box or
`--no-plugin-report`. The fields are listed in [install-report.md](install-report.md).

## Troubleshooting

| Symptom | What to do |
|---|---|
| Something failed and you want help | Attach the run's log from `~/.local/state/open-apollo/` (the wizard shows its name) to a [bug report](../SUPPORT.md#opening-an-issue). |
| The wizard finds no UAD install | See [Getting a UAD install](uad-install.md). Check with `oa-assets scan`. |
| "A DSP program in your install is a version open-apollo doesn't know yet" | Your UAD is newer than the versions open-apollo has checked. Please [open an issue](https://github.com/rolotrealanis98/open-apollo/issues) with your UAD version. |
| The Apollo isn't detected (`lspci -d 1a00:` is empty) | Power-cycle the Apollo, check the Thunderbolt cable and port, and authorize the device (`boltctl list`). |
| No sound after using a JACK app (Ardour) | `systemctl --user restart pipewire wireplumber pipewire-pulse`, then `apollo-setup-io`. |
| The Console shows no art | `bash scripts/install-uaconsole.sh --check`; see [console-rs/README.md](../console-rs/README.md#troubleshooting). |
| A re-opened plug-in window stays behind the mixer (Wayland) | The compositor places windows. On Hyprland enable `focus_on_activate`. |
| Plug-in windows show a plain parameter list | Run `oa-assets status`. The panel specs or their art are missing, or that panel isn't drawn yet. |
| The plug-in browser is empty | `oa-assets catalogue --from /path/to/windows`, then Settings › Plug-Ins › RESCAN in the Console. |

More hardware notes are on the [wiki](https://github.com/rolotrealanis98/open-apollo/wiki/Troubleshooting).
