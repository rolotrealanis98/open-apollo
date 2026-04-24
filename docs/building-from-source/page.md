---
title: Building from Source
---

This page covers how to build the `ua_apollo` kernel module from source, including prerequisites, build options, and DKMS setup for automatic rebuilds on kernel updates.

For the standard quick-start process (build, load, verify), see the [Installation](/docs/installation) guide.

---

## Prerequisites

You need the following packages installed on your system:

- **Linux kernel 6.8+** with headers matching your running kernel
- **GCC** (the kernel module build system requires it)
- **GNU Make**
- **kmod** tools (`insmod`, `modprobe`, `depmod`)

### Fedora / RHEL

```bash
sudo dnf install kernel-devel gcc make kmod
```

### Ubuntu / Debian

```bash
sudo apt install linux-headers-$(uname -r) build-essential kmod
```

### Arch Linux

```bash
sudo pacman -S linux-headers gcc make kmod
```

Verify your kernel headers are installed and match:

```bash
ls /lib/modules/$(uname -r)/build
```

If this directory does not exist, your kernel headers are not installed or do not match the running kernel.

---

## Build Process

### Step 1: Clone the Repository

```bash
git clone https://github.com/rolotrealanis98/open-apollo.git
cd open-apollo
```

> **Note:** You will also need the firmware file before loading the driver. See [Step 4](#step-4-install-firmware) for details.

### Step 2: Build the Module

```bash
cd driver
make
```

This invokes the kernel build system:

```
make -C /lib/modules/$(uname -r)/build M=$(pwd) modules
```

On success, you will see `ua_apollo.ko` in the `driver/` directory. The module is composed of three object files:

| Object | Source | Purpose |
|--------|--------|---------|
| `ua_core.o` | `ua_core.c` | PCIe probe, device detection, chardev, ioctls |
| `ua_audio.o` | `ua_audio.c` | ALSA PCM, mixer controls, DMA, transport |
| `ua_dsp.o` | `ua_dsp.c` | DSP ring buffers, firmware loading, routing |

### Step 3: Install the Module

```bash
sudo make install
```

This runs `modules_install` and `depmod -a`, placing the module into your kernel's module tree so `modprobe ua_apollo` works.

### Step 4: Install Firmware

The mixer DSP firmware blob must be placed in `/lib/firmware/ua-apollo-mixer.bin`. The driver loads this file automatically via the kernel's `request_firmware()` API during probe.

The firmware binary is **not included in this repository** — it is Universal Audio's proprietary code. You must obtain it from an existing UAD installation:

**From macOS:**
```bash
# The firmware is embedded in the kext bundle
sudo cp /Library/Extensions/UAD2System.kext/Contents/Resources/ua-apollo-mixer.bin \
    /lib/firmware/ua-apollo-mixer.bin
```

**From Windows:**
```bash
# Typically located in the UAD program directory
# Copy from: C:\Program Files\Universal Audio\ua-apollo-mixer.bin
sudo cp /path/to/ua-apollo-mixer.bin /lib/firmware/ua-apollo-mixer.bin
```

Verify the file has the correct magic number:
```bash
xxd /lib/firmware/ua-apollo-mixer.bin | head -1
# Should start with: 4d464155 (UAFM in little-endian)
```

---

## Make Targets

| Target | Description |
|--------|-------------|
| `make` or `make all` | Build the kernel module (`ua_apollo.ko`) |
| `make clean` | Remove all build artifacts |
| `make install` | Install module to kernel tree and run `depmod` |
| `make firmware-install` | Install firmware blob to `/lib/firmware/` (requires `ua-apollo-mixer.bin` in repo root) |

---

## Build Options

### Custom Kernel Directory

To build against a different kernel (e.g., for cross-compilation or a non-running kernel):

```bash
make KDIR=/path/to/kernel/build
```

The default is:

```
KDIR ?= /lib/modules/$(shell uname -r)/build
```

### Compiler Flags

The Makefile applies `-Og` (optimize for debugging) to both `ua_core.c` and `ua_audio.c` as a workaround for a GCC 13.3 internal compiler error (ICE in `try_forward_edges` at `-O1`/`-O2`). This is defined in the Makefile as:

```makefile
CFLAGS_ua_core.o := -Og
CFLAGS_ua_audio.o := -Og
```

You do not need to change these unless you are working on the driver source code and want to test with different optimization levels.

---

## Common Build Errors

### Missing kernel headers

```
make[1]: *** /lib/modules/6.x.y/build: No such file or directory
```

**Fix:** Install the kernel-devel package matching your running kernel. On Fedora: `sudo dnf install kernel-devel-$(uname -r)`.

### Version mismatch

```
ERROR: modpost: module ua_apollo.ko is from kernel X but loaded on kernel Y
```

**Fix:** Rebuild after a kernel update: `make clean && make`. Or use DKMS (see below) to handle this automatically.

### GCC ICE (Internal Compiler Error)

```
during GIMPLE pass: try_forward_edges
cfgcleanup.cc:580: ICE
```

**Fix:** This is a known GCC 13.3 bug triggered by large functions at `-O1`/`-O2`. The Makefile already works around this with `-Og`. If you see this error, ensure you are using the unmodified Makefile.

### Symbol version warnings

```
WARNING: modpost: module ua_apollo.ko: symbol xyz has no CRC
```

**Fix:** This typically means the kernel was built with `CONFIG_MODVERSIONS` but the headers don't include CRC data. Ensure `kernel-devel` matches your exact running kernel version.

---

## DKMS Setup

DKMS (Dynamic Kernel Module Support) automatically rebuilds the module whenever you install a new kernel.

### Step 1: Install DKMS

```bash
# Fedora
sudo dnf install dkms

# Ubuntu/Debian
sudo apt install dkms
```

### Step 2: Create a DKMS Configuration

Create `/usr/src/ua_apollo-0.1.0/dkms.conf`:

```ini
PACKAGE_NAME="ua_apollo"
PACKAGE_VERSION="0.1.0"
BUILT_MODULE_NAME[0]="ua_apollo"
BUILT_MODULE_LOCATION[0]="driver/"
DEST_MODULE_LOCATION[0]="/extra"
AUTOINSTALL="yes"
MAKE[0]="make -C ${kernel_source_dir} M=${dkms_tree}/${PACKAGE_NAME}/${PACKAGE_VERSION}/build/driver modules"
CLEAN="make -C ${kernel_source_dir} M=${dkms_tree}/${PACKAGE_NAME}/${PACKAGE_VERSION}/build/driver clean"
```

### Step 3: Copy Source and Register

```bash
sudo cp -r . /usr/src/ua_apollo-0.1.0/
sudo dkms add -m ua_apollo -v 0.1.0
sudo dkms build -m ua_apollo -v 0.1.0
sudo dkms install -m ua_apollo -v 0.1.0
```

### Step 4: Verify

```bash
dkms status
```

You should see `ua_apollo/0.1.0` listed as installed. On your next kernel update, DKMS will automatically rebuild the module.

---

## Verifying the Build

After building, check the module information:

```bash
modinfo driver/ua_apollo.ko
```

Expected output includes:

```
description:    Universal Audio Apollo Thunderbolt PCIe driver
license:        GPL
author:         Open Apollo contributors
```

To confirm the module loads correctly:

```bash
sudo insmod driver/ua_apollo.ko
dmesg | tail -20
```

Look for lines like:

```
ua_apollo 0000:xx:00.0: Apollo x4: FPGA rev 0x........
ua_apollo 0000:xx:00.0: audio: 24 play ch, 22 rec ch, 4096 frame buf
```
