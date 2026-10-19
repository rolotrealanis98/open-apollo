# open-apollo on Asahi Linux (Apple Silicon)

A Thunderbolt Apollo works on an Apple Silicon Mac running Asahi Linux, but not on the stock
Asahi kernel: that kernel cannot tunnel PCIe over Thunderbolt yet, so the Apollo never shows up
on the PCI bus. This page builds a second kernel that can, boots it next to the stock one, and
installs open-apollo on it.

**Verified** on 2026-10-19: Mac Studio M2 Ultra (`j475d`), Arch Linux ARM / Asahi
(`linux-asahi 7.1.13.asahi3`, Omarchy desktop), Apollo x4 (Thunderbolt 3, Intel Alpine Ridge LP
controller) on a rear Thunderbolt port, open-apollo 2.1.0. The Apollo enumerates, the driver
binds with MSI interrupts and no IOMMU faults. Playback and capture together ran in PipeWire at
48 kHz for 10 minutes at a 64-frame quantum and 4 minutes at 256 with no errors on the Apollo's
playback and capture nodes (the recording client counted 31, logged only after playback had
stopped), and no IOMMU faults. A 440 Hz test tone played clean.

- [What you need to know first](#what-you-need-to-know-first)
- [Which Macs](#which-macs)
- [1. Get the kernel source](#1-get-the-kernel-source)
- [2. Build the kernel package](#2-build-the-kernel-package)
- [3. Install it](#3-install-it)
- [4. Boot it](#4-boot-it)
- [5. Check the Apollo](#5-check-the-apollo)
- [6. Install open-apollo](#6-install-open-apollo)
- [Going back to the stock kernel](#going-back-to-the-stock-kernel)
- [Known limits](#known-limits)

## What you need to know first

- **The kernel is third party and out of tree.** It is
  [Ophuscado/linux](https://github.com/Ophuscado/linux) branch `tbt/7.1`: the Asahi kernel
  `asahi-7.1.13-3` plus 10 commits. Five of them bring Sven Peter's USB4/Thunderbolt host driver
  and Type-C pieces over from Asahi's own 7.2 work-in-progress tree; the other five add the PCIe
  controllers behind the Thunderbolt ports, an IOMMU offset those ports need, and the device-tree
  nodes. Much of it was written with LLM help, which Asahi's contribution policy does not accept,
  so none of it is headed upstream as is. **Do not report problems with this kernel to Asahi
  Linux.** Read the diff before you build it (`git diff origin/asahi...tbt-7.1`).
- **The kernel and its device tree have to travel together.** Asahi's bootloader (m1n1) hands
  one device tree to whatever kernel boots, and by default it takes it from the newest
  `/lib/modules/*-ARCH` directory. The stock kernel must keep seeing the stock device tree, and
  the Thunderbolt kernel needs its own. So the package's release string ends in `-tbt`, never
  `-ARCH`, and it boots from a bundle of its own (step 4).
- **Pin the stock kernel version.** `tbt/7.1` sits on exactly `asahi-7.1.13-3`. If your
  `linux-asahi` is a different version, the driver and the stock device tree may not match;
  rebase the branch onto your installed tag first (`pacman -Q linux-asahi`, tags `asahi-<ver>-<rel>`).
- You need about 10 GB of disk for the build and an hour or less (four minutes on an M2 Ultra).

## Which Macs

The branch adds Thunderbolt nodes for the M2 Pro/Max/Ultra desktops only:

| Mac | Device tree | Status |
|---|---|---|
| Mac Studio, M2 Ultra | `t6022-j475d.dtb` | **Verified** with an Apollo x4 |
| Mac Studio, M2 Max | `t6021-j475c.dtb` | Same files, untested |
| Mac mini, M2 Pro | `t6020-j474s.dtb` | Same files, untested |
| Any M1, M3 or laptop | — | No Thunderbolt nodes in this branch |

Check yours with `tr -d '\0' </proc/device-tree/compatible`.

## 1. Get the kernel source

Your installed kernel's tag, and a worktree of the Thunderbolt branch on top of it:

```bash
pacman -Q linux-asahi                       # 7.1.13.asahi3-2 -> tag asahi-7.1.13-3
git clone https://github.com/AsahiLinux/linux.git ~/linux   # or reuse a clone you have
cd ~/linux
git remote add ophuscado https://github.com/Ophuscado/linux.git
git fetch origin asahi --tags
git fetch ophuscado tbt/7.1
git worktree add ~/linux-tbt -b tbt-7.1 ophuscado/tbt/7.1
git log --oneline asahi-7.1.13-3..tbt-7.1  # expect the 10 commits, nothing else
```

## 2. Build the kernel package

The recipe is [`asahi/PKGBUILD`](asahi/PKGBUILD). It builds the worktree as
`linux-asahi-tbt`, release `7.1.13-1-tbt`, and installs the kernel, its modules and its device
trees under `/lib/modules/7.1.13-1-tbt/`. The config is the stock one with Thunderbolt turned on:

```bash
mkdir ~/linux-asahi-tbt && cd ~/linux-asahi-tbt
cp ~/open-apollo/docs/asahi/PKGBUILD .
zcat /proc/config.gz > config
~/linux-tbt/scripts/config --file config --set-str LOCALVERSION -tbt \
  -e USB4 -m USB4_APPLE_SOC -m RESET_APPLE_CIO -e HOTPLUG_PCI_PCIE
TBT_SRC=~/linux-tbt makepkg -od   # prepare: prints the config changes and "7.1.13-1-tbt"
TBT_SRC=~/linux-tbt makepkg -ed   # build and package
```

`USB4` has to be `=y`, not `=m`: the Apple PCIe driver "depends on USB4 || !USB4", and with
`USB4=m` it would become a module too, taking the Mac's built-in Ethernet, Wi-Fi and USB-A ports
with it until it loads. The recipe uses the Rust toolchain the stock kernel was built with
(`RUSTUP_TOOLCHAIN`, 1.93.1 for `7.1.13.asahi3`; install it with `rustup toolchain install`).
Without `pahole` installed, the build leaves out BTF debug info; that does not matter here.

Before installing, compare the new device tree with the stock one. Only Thunderbolt nodes should
be added, and the display nodes must not change:

```bash
D=t6022-j475d.dtb    # yours, from the table above
dtc -q -I dtb -O dts -s /lib/modules/$(uname -r)/dtbs/$D > stock.dts
dtc -q -I dtb -O dts -s pkg/linux-asahi-tbt/usr/lib/modules/7.1.13-1-tbt/dtbs/$D > tbt.dts
diff stock.dts tbt.dts | grep '^[<>].*\(dcp\|display\)'   # phandle numbers move; nothing else should
```

## 3. Install it

```bash
sudo cp --preserve=timestamps /boot/efi/m1n1/boot.bin /boot/efi/m1n1/boot-stock.bin   # your way back
sudo pacman -U ~/linux-asahi-tbt/linux-asahi-tbt-*.pkg.tar.*
```

`/boot/efi` is Asahi's EFI system partition, the one that holds `m1n1/boot.bin` (on some
installs it is mounted at `/boot`). pacman's hooks build
`/boot/initramfs-linux-asahi-tbt.img` and rebuild the stock `boot.bin` from the stock kernel,
which leaves it as it was. If your system snapshots with snapper or similar, take one first.

## 4. Boot it

m1n1 can boot a kernel directly: `boot.bin` is m1n1 followed by a config line with the kernel
command line, the device tree, the initramfs (gzip or xz) and the gzipped kernel. That is how the
verified system boots, and it is how this kernel gets its own device tree:

```bash
K=7.1.13-1-tbt
D=t6022-j475d.dtb                                   # yours, from the table above
ESP=/boot/efi
CMDLINE="$(sed -e 's/BOOT_IMAGE=[^ ]* //' /proc/cmdline)"   # or your own root=... line
tmp=$(mktemp -d)
printf 'chosen.bootargs=%s\n' "$CMDLINE" > "$tmp/config"
# m1n1 cannot unpack zstd, mkinitcpio's default. The initramfs is readable by root only.
case "$(sudo file -b /boot/initramfs-linux-asahi-tbt.img)" in
  Zstandard*) sudo zstd -dc /boot/initramfs-linux-asahi-tbt.img | gzip -9 > "$tmp/initrd" ;;
  *)          sudo cat /boot/initramfs-linux-asahi-tbt.img > "$tmp/initrd" ;;
esac
gzip -9 -c /boot/vmlinuz-linux-asahi-tbt > "$tmp/Image.gz"
cat /usr/lib/asahi-boot/m1n1.bin "$tmp/config" /lib/modules/$K/dtbs/$D \
    "$tmp/initrd" "$tmp/Image.gz" > "$tmp/boot.bin"
sudo mkdir -p "$ESP/m1n1/tbt" && sudo cp "$tmp/boot.bin" "$ESP/m1n1/tbt/boot.bin"
sudo cp "$ESP/m1n1/tbt/boot.bin" "$ESP/m1n1/boot.bin"
```

This skips U-Boot and GRUB for as long as that `boot.bin` is in place. Any later stock kernel or
initramfs update rewrites `boot.bin` with the stock bundle again (the safe direction); copy
`m1n1/tbt/boot.bin` back to return to the Thunderbolt kernel.

m1n1 fills in the Thunderbolt controllers' tunables from Apple's device tree at boot; m1n1 1.6.1
(Asahi's current package) does this, so no m1n1 update is needed.

Power the Apollo on, plug it into a rear Thunderbolt port with a Thunderbolt 3 or 4 cable, and
reboot.

## 5. Check the Apollo

```bash
uname -r                                   # 7.1.13-1-tbt
boltctl list                               # Apollo x4, generation Thunderbolt 3, authorized
lspci -nn | grep 1a00                      # Multimedia controller [1a00:0002]
journalctl -k -b | grep -ci 'translation fault'   # 0
```

What the verified system logged: the six Thunderbolt PCIe controllers "waiting for a tunnel",
the IOMMUs reporting "Device addresses arrive with offset 0x10000000000", the Apollo's router
(`1176:0008`) at 2 × 20 Gb/s, then "Link up" on its PCIe controller about eight seconds into boot.
boltd authorized the Apollo on its own (`iommu_dma_protection` is 1 on Asahi). If yours stays
unauthorized, `boltctl enroll --policy auto <uuid>`.

## 6. Install open-apollo

The installer builds the driver with DKMS against `/lib/modules/$(uname -r)/build`. The package
above ships no headers, so point that at the kernel build tree:

```bash
sudo ln -sfn ~/linux-asahi-tbt/src/build /lib/modules/7.1.13-1-tbt/build
```

(Leave `~/linux-asahi-tbt` and `~/linux-tbt` in place: DKMS rebuilds from them.) Then install as
on any other distribution: the [setup wizard](install.md#setup-wizard) or the
[command-line install](install.md#command-line-install). The UAD install it reads from can be
[a copied folder](uad-install.md#option-4-a-copied-folder) taken from a Windows PC or from a
Windows VM on another machine, so the Mac needs no Windows VM of its own. On Apple Silicon the
installer skips the `iommu=pt` step, which is for Intel's IOMMU.

To try the driver before installing anything, build it by hand and load it:

```bash
make -C ~/open-apollo/driver KDIR=~/linux-asahi-tbt/src/build
sudo insmod ~/open-apollo/driver/ua_apollo.ko
aplay -l | grep -i apollo
```

## Going back to the stock kernel

- From Linux: `sudo cp /boot/efi/m1n1/boot-stock.bin /boot/efi/m1n1/boot.bin` and reboot.
- If Linux does not come up: hold the power button until "Loading startup options", start
  macOS, mount Asahi's EFI partition (`diskutil list`, the small `EFI` partition that is not
  macOS's own; `sudo diskutil mount diskXsY`) and make the same copy under `/Volumes/EFI*/m1n1/`.
- To remove it: `sudo pacman -R linux-asahi-tbt`, then `sudo update-m1n1` to rebuild the stock
  `boot.bin`.

## Known limits

- **Thunderbolt 3 on the Apple host** had never run on Linux before this; it works with the
  Apollo x4's Alpine Ridge LP controller. The Apple Thunderbolt firmware logs a "Gen2/3 link
  error" on both lanes once during bring-up, then the link trains at full speed.
- **Hot-plug, unplug and suspend** are untested. When a tunnel goes away the kernel removes
  everything behind it at once; don't unplug the Apollo while it is in use.
- **Legacy (INTx) interrupts** are not routed on the tunneled ports (`of_irq_parse_pci ... -22`
  in the log); the Apollo uses MSI, which works.
- The tunneled PCIe controllers stay powered whether or not anything is plugged in.
- Everything in [Known issues](../README.md#known-issues) applies, including the raw 22-channel
  input node.
