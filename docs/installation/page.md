---
title: Installation
---

Complete guide to building and installing the Open Apollo Linux driver for Universal Audio Apollo Thunderbolt interfaces.

---

## Before you begin

{% callout type="warning" title="Turn OFF the Apollo before installing" %}
If your Apollo is powered on and connected via Thunderbolt, DKMS module auto-loading during installation can cause the system to hang. **Power off the Apollo first.** You do not need to unplug the Thunderbolt cable — just turn the unit off. Power it back on after the install completes.
{% /callout %}

---

## Prerequisites

You need a Linux system with:

- **Linux kernel 6.8+** with headers installed (Ubuntu 24.04+, Fedora 40+, Arch, openSUSE Tumbleweed)
- **GCC** (or compatible C compiler)
- **Make**
- **Python 3.10+** (for the mixer daemon)
- A **Thunderbolt 3/4** port (USB-C Thunderbolt)

### Fedora / RHEL

```bash
sudo dnf install kernel-devel gcc make python3
```

### Ubuntu / Debian

```bash
sudo apt install linux-headers-$(uname -r) build-essential python3
```

### Arch Linux / CachyOS / Manjaro

```bash
sudo pacman -S linux-headers gcc make python
```

---

## Build and install

The repository includes an interactive installer that handles everything: dependency checks, kernel module build, DKMS registration, IOMMU detection, PipeWire/WirePlumber config deployment, and tray indicator setup.

```bash
git clone https://github.com/open-apollo/open-apollo.git
cd open-apollo
sudo bash scripts/install.sh
```

The installer will walk you through each step interactively. Options:

```bash
sudo bash scripts/install.sh --skip-init   # Skip hardware init
sudo bash scripts/install.sh --no-dkms     # Skip DKMS setup
sudo bash scripts/install.sh --help        # Show all options
```

### NixOS (declarative)

Add to your `flake.nix` inputs:

```nix
inputs.open-apollo.url = "github:rolotrealanis98/open-apollo";
```

Then in your `configuration.nix`:

```nix
imports = [ inputs.open-apollo.nixosModules.default ];
hardware.ua-apollo.enable = true;
```

Run `nixos-rebuild switch`. The module handles the kernel module build, `iommu=pt`, Thunderbolt (bolt), PipeWire, and required packages automatically. No manual steps required.

### Manual build (alternative)

If you prefer to build manually instead of using the installer:

```bash
cd open-apollo/driver
make
```

This produces `ua_apollo.ko` — the kernel module. Load it with:

```bash
sudo insmod ua_apollo.ko
```

### Verify the driver loaded

Check kernel messages:

```bash
dmesg | tail -20
```

You should see output like:

```
ua_apollo: Apollo x4 detected (device_type=0x1F)
ua_apollo: Firmware version: ...
ua_apollo: Registered ALSA card ...
```

Check that ALSA sees the device:

```bash
aplay -l
```

You should see an entry for your Apollo interface.

---

## IOMMU / Intel VT-d

Ubuntu and many other distributions enable IOMMU (Intel VT-d or AMD-Vi) by default. Without passthrough mode, the driver cannot communicate with the Apollo — BAR0 register reads return `0xFFFFFFFF` and the device is unusable.

The install script detects this automatically and offers to add `iommu=pt` to your GRUB configuration. If you need to do it manually:

1. Edit `/etc/default/grub`
2. Add `iommu=pt` to `GRUB_CMDLINE_LINUX_DEFAULT`:
   ```
   GRUB_CMDLINE_LINUX_DEFAULT="quiet splash iommu=pt"
   ```
3. Update GRUB:
   ```bash
   sudo update-grub          # Ubuntu/Debian
   sudo grub2-mkconfig -o /boot/grub2/grub.cfg  # Fedora
   ```
4. Reboot

This is safe and does not affect other devices. It simply tells the kernel to use passthrough mode for DMA, which is required for the Apollo's PCIe BAR0 registers to be accessible.

---

## PipeWire / WirePlumber configuration

The repository includes recommended PipeWire and WirePlumber configuration files in the `configs/` directory. These configure:

- S32LE sample format at 48 kHz
- MMAP disabled (required for stability)
- Suspend disabled (keeps the device active)

Deploy the configurations:

```bash
sudo bash configs/deploy.sh
```

This copies the WirePlumber policy, UCM2 profiles, and the `apollo-setup-io` script to the correct system directories. Restart PipeWire and WirePlumber afterward:

```bash
systemctl --user restart pipewire wireplumber
```

The install script handles this automatically if you use `scripts/install.sh`.

---

## Post-install: powering on the Apollo

After the install completes:

1. **Power on the Apollo** using its rear power switch
2. **Wait approximately 20 seconds** for Thunderbolt enumeration to complete
3. The driver auto-loads via DKMS — no manual `insmod` needed
4. Run the init script to load firmware, activate the DSP, and start the daemon:
   ```bash
   sudo bash tools/apollo-init.sh
   ```
5. Set up PipeWire virtual I/O devices:
   ```bash
   apollo-setup-io
   ```

You can verify with:

```bash
wpctl status          # Apollo should appear under Audio devices
aplay -l              # ALSA card listing
```

---

## Initialize the Apollo

After loading the driver and deploying configs, the Apollo needs initialization to load firmware and activate the DSP. This is required after every cold boot (powering the Apollo from off).

### Using the tray indicator

Click the Open Apollo tray icon and select **"Initialize Apollo..."** — this runs the init script with a graphical progress dialog.

### Using the command line

```bash
sudo bash tools/apollo-init.sh
```

The init script handles:

1. **Loads the driver** (if not already loaded)
2. **Diagnoses DSP state** (cold boot, warm restart, or stalled)
3. **Loads firmware** (on cold boot — replays 15 firmware blocks)
4. **Triggers ACEFACE connect** (activates DSP routing tables)
5. **Starts the mixer daemon** (TCP:4710 + TCP:4720 + WS:4721)
6. **Sets the PipeWire pro-audio profile** (makes Apollo visible in desktop sound settings)

On a warm boot (Apollo was already powered on), the DSP may already be alive. The init script detects this and skips firmware loading automatically.

After running, the Apollo should appear in your desktop sound settings and in `wpctl status`.

### Options

```bash
sudo bash tools/apollo-init.sh --no-daemon  # Init only, don't start daemon
sudo bash tools/apollo-init.sh --force       # Force firmware replay
sudo bash tools/apollo-init.sh --status      # Print current Apollo state
```

### Troubleshooting init

If the script reports **DSP STALLED** or **PCIe DEAD**, power cycle the Apollo (unplug power cable, wait 5 seconds, replug) and run the script again.

---

## Set up virtual I/O devices

After the Apollo is initialized, run:

```bash
apollo-setup-io
```

This script discovers the Apollo's PCI address at runtime (Thunderbolt addresses vary between connections), sets the pro-audio PipeWire profile, generates `~/.config/pipewire/pipewire.conf.d/apollo-io-map.conf`, and restarts PipeWire to load it.

After running, the following virtual devices are available in PipeWire:

| Device | Type | Channels |
|--------|------|----------|
| Apollo Mic 1 | Source | Mono (capture ch 0) |
| Apollo Mic 2 | Source | Mono (capture ch 1) |
| Apollo Mic 1+2 | Source | Stereo (capture ch 0-1) |
| Apollo Mic 3 | Source | Mono (capture ch 2) |
| Apollo Mic 4 | Source | Mono (capture ch 3) |
| Apollo Line In 3+4 | Source | Stereo (capture ch 2-3) |
| Apollo Monitor L/R | Sink | Stereo (playback ch 0-1) |
| Apollo Line Out 1+2 | Sink | Stereo (playback ch 2-3) |
| Apollo Line Out 3+4 | Sink | Stereo (playback ch 4-5) |

`apollo-setup-io` must be run after each Apollo power-on. To run it automatically when the Apollo powers on, wire it to a udev rule or a systemd user service that triggers on Apollo device arrival.

---

## System tray indicator

The Open Apollo tray indicator is installed automatically by the install script and auto-starts on login.

### Icon colors

| Color | Meaning |
|-------|---------|
| **Green** | Everything is working — driver loaded, DSP alive, daemon running |
| **Yellow** | Needs attention — Apollo needs initialization, or daemon is stopped |
| **Red** | Apollo disconnected or PCIe link dead |
| **Gray** | Driver not loaded |

### Menu actions

- **Initialize Apollo...** — runs `apollo-init.sh` (firmware load + DSP connect + daemon start)
- **Restart Daemon** — restarts the mixer daemon
- **Stop Daemon** — stops the mixer daemon
- **Reload PipeWire Profile** — re-applies the pro-audio profile
- **Open Daemon Log** — opens `/tmp/ua-mixer-daemon.log` in your default text editor

You can also launch the tray indicator manually from Activities by searching **"Open Apollo"**.

---

## DKMS setup (persist across kernel updates)

The install script sets up DKMS automatically. If you installed manually, you can configure DKMS separately so the module rebuilds when your kernel updates.

### Install DKMS

```bash
# Fedora
sudo dnf install dkms

# Ubuntu / Debian
sudo apt install dkms

# Arch
sudo pacman -S dkms
```

### Register the module

From the repository root:

```bash
sudo cp -r driver /usr/src/ua_apollo-0.1.0
sudo dkms add ua_apollo/0.1.0
sudo dkms build ua_apollo/0.1.0
sudo dkms install ua_apollo/0.1.0
```

The module will now rebuild automatically when you install a new kernel.

### Load on boot

Create a module load configuration:

```bash
echo "ua_apollo" | sudo tee /etc/modules-load.d/ua_apollo.conf
```

---

## Known limitations

- **Apollo must be powered off during installation** — DKMS auto-loading while the Apollo is connected can cause system hangs
- **`apollo-init.sh` must be run after each cold boot** — a systemd service for automatic initialization is planned for a future release
- **`apollo-setup-io` must be run after each Apollo power-on** — it regenerates the PipeWire loopback config with the current (dynamic) PCIe address
- **`iommu=pt` kernel parameter is required** on most modern systems — Ubuntu 24.04 enables IOMMU by default; without `iommu=pt`, BAR0 reads return `0xFFFFFFFF` and audio does not work
- **Audacity and raw-ALSA apps may freeze the desktop** — these apps probe ALSA devices directly, bypassing PipeWire, which can lock up the audio subsystem. Use PipeWire-native applications instead: `pw-record`, `pw-play`, REAPER, Ardour, or any JACK-compatible DAW
- **PCIe address varies** between Thunderbolt connections — the driver and `apollo-setup-io` both handle this automatically; never hardcode PCIe addresses in scripts

---

## USB Apollo installation

USB Apollo models (Solo USB, Twin USB, Twin X USB) use a separate installer that does not require a kernel module.

```bash
git clone https://github.com/rolotrealanis98/open-apollo.git
cd open-apollo
sudo bash scripts/install-usb.sh
```

The installer handles: dependency installation, FX3 firmware setup (prompts if missing), patched `snd-usb-audio` kernel module build (three patches — see below), DSP init daemon deployment, udev rule deployment, and PipeWire configuration.

{% callout type="note" %}
The Apollo must be plugged into a **USB 3.0** port. USB 2.0 cables and ports are not supported — the FX3 re-enumerates at SuperSpeed after firmware upload.
{% /callout %}

Firmware files (`ApolloSolo.bin`, etc.) can be obtained from UA's firmware page or extracted from UA Connect on Windows: `C:\Program Files (x86)\Universal Audio\Powered Plugins\Firmware\USB\`. Place them in `/lib/firmware/universal-audio/` — the installer will prompt you if they are missing.

### What the installer patches in snd-usb-audio

The stock `snd-usb-audio` module cannot work with Apollo USB devices. The installer builds an out-of-tree module with three patches:

1. **format.c** — fixed-rate quirk: when `GET_RANGE` STALLs (UA devices don't implement it), fall back to hardcoded rates (44100, 48000, 88200, 96000, 176400, 192000 Hz)
2. **implicit.c** — `IMPLICIT_FB_SKIP_DEV` for VID `0x2B5A`: prevents EP 0x83 feedback endpoint conflict that blocks stream open
3. **endpoint.c** — skips `endpoint_compatible()` check for UA VID: prevents "Incompatible EP setup" errors

### What runs automatically after plug-in

Once installed, the udev rule (`/etc/udev/rules.d/99-apollo-usb.rules`) handles the full init sequence on each plug-in:

1. `fx3-load.py` uploads `ApolloSolo.bin` to the Cypress FX3 (firmware loader PID → audio PID)
2. A 3-second delay lets `snd-usb-audio` fully probe the device
3. `usb-dsp-init.py --daemon` activates the FPGA, sets sample rate, and starts the EP6 drain daemon

The EP6 drain daemon is required on Intel xHCI controllers — without it, the Apollo's JFK notification flood (~2000 packets/sec) overflows the interrupt ring buffer and causes audio dropouts.

### Capture (10ch recording)

Playback (6ch) works automatically after plug-in. For capture, full DSP initialization is required:

```bash
sudo python3 tools/usb-full-init.py
```

{% callout type="warning" title="Firmware version dependency" %}
`usb-full-init.py` replays a 38-packet init sequence captured from Cauldron firmware v1.3 build 3. If your device runs a different firmware build, the sequence may crash at packet 28 (SRAM address mismatch). If this happens, unplug and re-plug the device to recover, and use the lightweight init for playback-only.
{% /callout %}

Capture through PipeWire currently returns silence even when raw ALSA capture works. Use `arecord -D hw:USB` as a workaround until this is resolved.

---

## Verified configurations

### Hardware-verified (Thunderbolt)

| Distro | Kernel | CPU | Thunderbolt | Status |
|--------|--------|-----|-------------|--------|
| Ubuntu 24.04.4 LTS | 6.17.0-19-generic | Intel i9-14900K | Thunderbolt 4 (Maple Ridge) | Fully working — 8/8 install cycles verified |
| Fedora 43 | 6.18.16-200.fc43.x86_64 | — | Thunderbolt 3 | Fully working |

Ubuntu 24.04 is the primary tested platform. Install testing was performed on overlayroot (ephemeral filesystem) to guarantee clean-state reproducibility across all 8 cycles.

### Hardware-verified (USB — Apollo Solo USB)

| Distro | Kernel | USB Controller | Playback | Capture |
|--------|--------|---------------|----------|---------|
| Ubuntu Studio 24.04 | 6.17.0-20-generic | Intel Tiger Lake-H xHCI | Working | Working (usb-full-init.py) |
| CachyOS | 6.19.10-1-cachyos | AMD USB | Working | Needs usb-full-init.py |

### Docker install matrix (build + config deploy, no hardware)

Automated testing validates that the driver compiles and all configs deploy correctly across supported distros. Requires **kernel 6.8+** — the driver uses `linux/hrtimer_types.h` which was split from `linux/hrtimer.h` in 6.8.

| Distro | Kernel Headers | Driver Build | WirePlumber | Configs |
|--------|---------------|-------------|-------------|---------|
| Ubuntu 24.04 | 6.8 | PASS | 0.4 (lua) | All deployed |
| Fedora 42 | 6.12 | PASS | 0.5 (conf) | All deployed |
| Fedora 41 | 6.11 | PASS | 0.5 (conf) | All deployed |
| Fedora 40 | 6.8 | PASS | 0.5 (conf) | All deployed |
| Debian trixie (13) | 6.x | PASS | 0.5 (conf) | All deployed |
| Arch (latest) | latest | PASS | 0.5 (conf) | All deployed |
| openSUSE Tumbleweed | latest | PASS | 0.5 (conf) | All deployed |
| Linux Mint 22 | 6.8 | PASS | 0.4 (lua) | All deployed |
| Pop!_OS 24.04 | 6.8 | PASS | 0.4 (lua) | All deployed |
| Manjaro (latest) | latest | PASS | 0.5 (conf) | All deployed |

**Not supported** (kernel too old):

| Distro | Kernel Headers | Reason |
|--------|---------------|--------|
| Ubuntu 22.04 | 5.15 | Missing `hrtimer_types.h` |
| Ubuntu 20.04 | 5.4 | Missing `hrtimer_types.h`, no PipeWire |
| Debian bookworm (12) | 6.1 | Missing `hrtimer_types.h` |
| Debian bullseye (11) | 5.10 | Missing `hrtimer_types.h`, no PipeWire |

---

## Next steps

- [Supported Devices](/docs/supported-devices) — check your Apollo model's status
- [Daemon Setup](/docs/daemon-setup) — configure the mixer daemon for full mixer control
- [Troubleshooting](/docs/troubleshooting) — common issues and solutions
