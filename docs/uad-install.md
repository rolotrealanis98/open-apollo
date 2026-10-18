# Getting a UAD install for setup

open-apollo includes no Universal Audio software, firmware or artwork. What your Apollo and the
Console need from UA's software is read from **a UAD install you already have**. Setup reads it
read-only, and nothing leaves your computer. The copies are for your own use on this machine;
don't share them. See [NOTICE.md](../NOTICE.md).

- [What setup takes from your install](#what-setup-takes-from-your-install)
- [Installing UA's software on Windows](#installing-uas-software-on-windows)
- [Option 1: Windows on its own partition or boot disk](#option-1-windows-on-its-own-partition-or-boot-disk) (dual boot)
- [Option 2: a Windows virtual machine](#option-2-a-windows-virtual-machine)
- [Option 3: a Wine prefix](#option-3-a-wine-prefix)
- [Option 4: a copied folder](#option-4-a-copied-folder)
- [After a UAD update](#after-a-uad-update)
- [Checking what setup will find](#checking-what-setup-will-find)

## What setup takes from your install

| From | What it becomes | Where it goes |
|---|---|---|
| UAD Console | the Console's art and fonts | `~/.local/share/open-apollo/console-resources/` |
| The Powered plug-ins | the plug-in windows' art (about 2.7 GB, made from recipes) | `~/.local/share/open-apollo/generated/` |
| The UAD driver and Powered plug-ins | the DSP programs the Apollo runs, and the USB Apollos' boot images | `/lib/firmware/universal-audio/` |
| The Powered plug-ins | every plug-in's DSP modules, kept for plug-in processing in a later release | `/lib/firmware/universal-audio/plugins/` |

The plug-in panel specs (the layout of each plug-in window; ours, not UA's) are a 1 MB
download from [open-apollo-panels](https://github.com/rolotrealanis98/open-apollo-panels).
With a UAD 12 install, 206 of 224 plug-in windows draw their own panel; the others show a
plain parameter list.

Running plug-ins on the Apollo's DSP isn't in this release. Setup stores their DSP modules
now so that a later release doesn't need your UAD install again.

## Installing UA's software on Windows

Install UA's Windows software as you would on any Windows PC:

1. Install **UA Connect** from [uaudio.com](https://www.uaudio.com) and sign in with your UA
   account.
2. From UA Connect, install **UAD** (the plug-ins and their firmware) and **UAD Console**.
3. Shut Windows down fully (below) before Linux reads its disk.

You don't need an Apollo connected to Windows, and you don't need any plug-in licences:
setup only reads files the installer puts on disk. Tested with UAD 12.0 and UAD Console
1.3.1. Setup checks every DSP program against a list of known SHA-256 values and stops on a
version it doesn't know (see [After a UAD update](#after-a-uad-update)).

**Shutting Windows down fully.** Start, Power, then hold **Shift** while you click **Shut
down**. Sleep, hibernate, and a plain shut down with Fast Startup on, all leave the disk in a
state Linux won't open. To stop this for good, turn off Fast Startup (Control Panel › Power
Options › Choose what the power buttons do), or run `powercfg /h off` in an administrator
terminal.

**BitLocker and device encryption.** New Windows 11 installs often encrypt the system drive
on their own (Settings › Privacy & security › Device encryption). Linux can't read an
encrypted partition as it is. Either:
- turn device encryption off in that setting and let Windows finish decrypting;
- unlock the partition in your Linux file manager with its 48-digit recovery key (it is at
  account.microsoft.com/devices/recoverykey), then press **Rescan** in setup;
- or copy the folders listed under [Option 4](#option-4-a-copied-folder) to a USB stick from
  Windows.

Then pick whichever of these fits your computer.

## Option 1: Windows on its own partition or boot disk

The easiest case, and the most common one. Windows can be on any disk the Linux computer can
see:

| Where Windows is | Notes |
|---|---|
| Another partition of the same drive | the usual dual boot |
| A second internal drive | the same; pick Windows in your computer's boot menu (often F12, F11 or Esc at power-on) to install UA's software |
| An external USB or Thunderbolt drive you boot Windows from | for computers with only Linux inside; see below |

1. Boot Windows, install UA's software (above) and **shut Windows down fully**.
2. Boot Linux. If Windows is on an external drive, plug it in.
3. Run setup (`bash scripts/setup.sh`). "Find your UAD install" lists the Windows partition
   whether or not it is mounted. Setup opens it read-only for the install and closes it
   afterwards.

**A Windows boot disk on USB.** If the computer has no Windows, you can put Windows on an
external SSD and boot from it. On any Windows PC, [Rufus](https://rufus.ie) writes a Windows
ISO to the drive with its **Windows To Go** option. Boot the Linux computer from that drive,
install UA's software, shut Windows down fully, boot Linux and run setup as above. A USB 3 or
Thunderbolt SSD works far better than a USB stick. Setup needs the drive only while it runs;
plug it in again only to run setup after a UAD update.

## Option 2: a Windows virtual machine

For computers with only Linux on them, when you'd rather not boot Windows at all.

1. Create a Windows VM:
   - Omarchy: `omarchy-windows-vm`.
   - Elsewhere, GNOME Boxes or virt-manager. Windows 11 needs UEFI and a TPM: in
     virt-manager, tick "Customize configuration before install", choose a UEFI firmware and
     add a TPM (emulated, version 2.0). Windows 10 needs neither.
2. In the VM, install UA Connect, UAD and UAD Console (above). No Apollo needs to be connected.
3. Shut Windows down fully (above), so the disk isn't changing while setup reads it.
4. In setup, choose **Use a VM disk image…** and pick the VM's disk. Setup attaches it
   read-only and detaches it when you finish.

| VM | Where its disk usually is | Format |
|---|---|---|
| Omarchy | `~/.windows/data.img` | raw: use it as it is |
| GNOME Boxes | `~/.local/share/gnome-boxes/images/` | qcow2: convert it first |
| virt-manager | `/var/lib/libvirt/images/` (may need read access) | qcow2: convert it first |
| VirtualBox, VMware | the VM's folder | VDI or VMDK: convert it first, or use Option 4 |

Setup reads **raw** disk images. Convert a qcow2, VDI, VMDK or VHDX disk first, with the VM
shut down. This needs free space for the full disk size:

```bash
qemu-img convert -p -O raw windows.qcow2 windows.img
```

Once setup has finished, everything it needs has been copied, so you can delete the converted
image. Keep the VM if you want to run setup again later, for example after a UAD update.

**Short on disk space?** Skip the conversion: copy the folders listed under
[Option 4](#option-4-a-copied-folder) out of the running VM through a shared folder (GNOME
Boxes, virt-manager, VirtualBox and VMware all have one), then pick the copy in setup.

## Option 3: a Wine prefix

If UA's Windows software is installed in a Wine prefix, setup finds it at `$WINEPREFIX`,
`~/.wine` or a `~/wine…` folder with a `drive_c` inside. For a prefix elsewhere, pick it with
**Choose a folder**.

## Option 4: a copied folder

From any Windows PC or VM with UA's software installed, copy these folders, keeping their
names and their place under `C:\`:

| Copy | What it gives |
|---|---|
| `C:\Program Files\Universal Audio` | the Powered plug-ins: their DSP modules and panel art |
| `C:\Program Files (x86)\Universal Audio` | UAD Console's art, and the USB Apollos' boot images |
| `C:\Windows\System32\UAD2SDK.dll` and `UAD2DriverClient.dll` *(recommended)* | the DSP programs the driver loads, and a few plug-ins' DSP modules |
| `C:\ProgramData\Package Cache` *(optional)* | the plug-in icons (UA keeps them in its installer cache when no Apollo was connected) |

For example, copy them into a folder `uad-windows` on a USB stick so that it holds
`uad-windows/Program Files/Universal Audio`, `uad-windows/Program Files (x86)/Universal Audio`
and `uad-windows/Windows/System32/UAD2SDK.dll`. In setup, choose **Choose a folder** and pick
`uad-windows`.

## After a UAD update

Run `bash scripts/setup.sh` again with the updated install: setup reinstalls cleanly and
imports everything again. If it stops with "A DSP program in your install is a version
open-apollo doesn't know yet", UA has changed a DSP program that open-apollo hasn't checked.
Please [open an issue](https://github.com/rolotrealanis98/open-apollo/issues) with your UAD
version. Setup has already removed the earlier install by then, so keep a copy of the
previous UAD install (an unupdated VM, or an [Option 4](#option-4-a-copied-folder) folder)
to install from until the new version is supported.

## Not supported yet

- Reading UA's installer download directly, without installing it on Windows first.
- A Mac's UAD install. Its files are laid out differently; only UAD Console's `Resources`
  folder can be used, for the Console art alone
  (`bash scripts/install-uaconsole.sh --resources /path/to/Resources`).

## Checking what setup will find

`oa-assets` is the tool setup uses. From the repository, after `bash scripts/setup.sh` has built
it (or `cargo build --release --manifest-path console-rs/Cargo.toml -p oa-assets`):

```bash
console-rs/target/release/oa-assets scan                          # every UAD install it can see
console-rs/target/release/oa-assets scan --from /path/to/folder   # one folder, a Wine prefix or a mounted partition
console-rs/target/release/oa-assets status                        # what has been imported and built
```
