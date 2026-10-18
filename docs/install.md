# Installing open-apollo

open-apollo needs two things from you: a supported Apollo, and **a UAD install you already
have**, because it includes no Universal Audio files. The setup wizard does everything else.

- [Before you start](#before-you-start)
- [Setup wizard](#setup-wizard) (Thunderbolt Apollos; recommended)
- [Command-line install](#command-line-install) (step by step, or without a desktop)
- [USB Apollos](#usb-apollos)
- [NixOS](#nixos)
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
   current install**. The report box sends an anonymous install report and is off by default.
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
   step asked you to (for `iommu=pt`, or a module Secure Boot must accept).

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
$OA import --from /path/to/windows             # UAD Console's art -> ~/.local/share/open-apollo
sudo bash scripts/install-panels.sh            # plug-in panel specs -> /usr/share/open-apollo/plugin-specs
$OA plugins --from /path/to/windows            # plug-in panels' art -> ~/.local/share/open-apollo/generated
sudo bash scripts/install.sh                   # driver, DKMS, audio settings, mixer daemon
bash scripts/install-uaconsole.sh              # the mixer console, for you
```

`install.sh` walks you through power-cycling the Apollo for a clean Thunderbolt start, checks
the driver and DSP, and plays a test tone. `--yes` asks nothing: it takes every question's
default answer and skips the power-cycle pause and the test tone. `--skip-init` skips the
hardware initialization and `--no-dkms` skips the DKMS setup.

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

## NixOS

Add to your `flake.nix` inputs:

```nix
inputs.open-apollo.url = "github:rolotrealanis98/open-apollo";
```

Then in your `configuration.nix`:

```nix
imports = [ inputs.open-apollo.nixosModules.default ];
hardware.ua-apollo.enable = true;
```

`nixos-rebuild switch` builds the kernel module and sets up `iommu=pt`, Thunderbolt (bolt),
PipeWire and the packages it needs. The console is `nix run github:rolotrealanis98/open-apollo#uaconsole`
(see [console-rs/README.md](../console-rs/README.md#nix)). Run the `oa-assets` steps above by
hand for the Console art and the plug-in panels.

## Plug-ins

What this release does with UAD plug-ins:

| | |
|---|---|
| Plug-in windows | The Console draws each plug-in's window from the panel specs and the art setup made from your Powered plug-ins: 206 of 224 with a UAD 12 install, the rest as a plain parameter list. |
| Plug-in DSP modules | Setup stores them in `/lib/firmware/universal-audio/plugins/`. Nothing loads them yet. |
| Running plug-ins on the Apollo's DSP | Not in this release. It is being developed and will only load plug-ins your Apollo itself reports as authorized. |

You don't need plug-in licences for any of this. To rebuild the panels' art alone, for
example after a UAD update, run `oa-assets plugins --from /path/to/windows`; `oa-assets status`
says what is there.

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
| Plug-in panels' art (made from your UAD install) | `~/.local/share/open-apollo/generated/` | `uninstall.sh --purge` |
| Plug-in panel specs | `/usr/share/open-apollo/plugin-specs/` | `uninstall.sh` |
| Mixer daemon | systemd user unit `ua-mixer-daemon` | `uninstall.sh` |
| Console | `~/.local/bin/uaconsole`, menu entry **Open Apollo Console** | `uninstall.sh` |
| Audio settings | `/etc/pipewire`, `/etc/wireplumber`, `/usr/share/alsa/ucm2`, `/etc/udev/rules.d` | `uninstall.sh` |

Nothing from your UAD install leaves your computer. setup downloads only the plug-in panel
specs, and sends an install report only when you tick the box. The report has no user name,
host name, home path or serial numbers.

## Troubleshooting

| Symptom | What to do |
|---|---|
| The wizard finds no UAD install | See [Getting a UAD install](uad-install.md). Check with `oa-assets scan`. |
| "A DSP program in your install is a version open-apollo doesn't know yet" | Your UAD is newer than the versions open-apollo has checked. Please [open an issue](https://github.com/rolotrealanis98/open-apollo/issues) with your UAD version. |
| The Apollo isn't detected (`lspci -d 1a00:` is empty) | Power-cycle the Apollo, check the Thunderbolt cable and port, and authorize the device (`boltctl list`). |
| No sound after using a JACK app (Ardour) | `systemctl --user restart pipewire wireplumber pipewire-pulse`, then `apollo-setup-io`. |
| The Console shows no art | `bash scripts/install-uaconsole.sh --check`; see [console-rs/README.md](../console-rs/README.md#troubleshooting). |
| Plug-in windows show a plain parameter list | Run `oa-assets status`. The panel specs or their art are missing, or that panel isn't drawn yet. |

More hardware notes are on the [wiki](https://github.com/rolotrealanis98/open-apollo/wiki/Troubleshooting).
