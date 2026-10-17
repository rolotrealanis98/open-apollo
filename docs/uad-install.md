# Getting a UAD install for setup

open-apollo includes no Universal Audio software, firmware or artwork. What your Apollo
needs from UA's software is read from **a UAD install you already have**:

- the DSP modules and USB boot images go to `/lib/firmware/universal-audio/`;
- UAD Console's art and fonts go to `~/.local/share/open-apollo/console-resources/`;
- the plug-in panels' art is made from the Powered plug-in files, into
  `~/.local/share/open-apollo/generated/`.

Setup reads your install read-only, and nothing leaves your computer. The copies are for your
own use on this machine; don't share them. See [NOTICE.md](../NOTICE.md).

## What you need on the Windows side

Install UA's Windows software as you would on any Windows PC:

1. Install **UA Connect** from [uaudio.com](https://www.uaudio.com) and sign in with your UA
   account.
2. From UA Connect, install **UAD** (the plug-ins and their firmware) and **UAD Console**.

You don't need an Apollo connected to Windows, and you don't need any plug-in licences:
setup only reads files the installer puts on disk. Tested with UAD 12.0 and UAD Console
1.3.1. setup checks every DSP module against a list of known SHA-256 values and stops on a
version it doesn't know.

Then pick whichever of these fits your computer.

## Option 1: Windows on another partition (dual boot)

The easiest case. Install UA's software in Windows, then:

1. **Shut Windows down fully**: Start, Power, Shut down. Not sleep or hibernate. If Windows
   has Fast Startup on, hold **Shift** while you click Shut down; otherwise Windows leaves the
   partition in a state Linux won't open.
2. Boot Linux and run setup. "Find your UAD install" lists the Windows partition whether or
   not it is mounted. setup opens it read-only for the install and closes it afterwards.

**BitLocker.** If the partition is encrypted with BitLocker, Linux can't read it as it is.
Either unlock it in your file manager with its 48-digit recovery key (it is at
account.microsoft.com/devices/recoverykey) and press **Rescan**, or copy the folders listed
under [Option 4](#option-4-a-copied-folder) to a USB stick.

## Option 2: a Windows virtual machine

For computers with only Linux on them.

1. Create a Windows VM: on Omarchy run `omarchy-windows-vm`; elsewhere GNOME Boxes or
   virt-manager work.
2. In the VM, install UA Connect, UAD and UAD Console (above). No Apollo needs to be connected.
3. Shut Windows down fully (above), so the disk isn't changing while setup reads it.
4. In setup, choose **Use a VM disk image…** and pick the VM's disk. setup attaches it
   read-only and detaches it when you finish.

| VM | Where its disk usually is |
|---|---|
| Omarchy | `~/.windows/data.img` |
| GNOME Boxes | `~/.local/share/gnome-boxes/images/` |
| virt-manager | `/var/lib/libvirt/images/` (may need read access) |

setup reads **raw** disk images. Convert a qcow2, VDI, VMDK or VHDX disk first; this needs
free space for the full disk size:

```bash
qemu-img convert -O raw windows.qcow2 windows.img
```

Once setup has finished, everything it needs has been copied, so you can delete the converted
image. Keep the VM if you want to run setup again later, for example after a UAD update.

## Option 3: a Wine prefix

If UA's Windows software is installed in a Wine prefix, setup finds it at `$WINEPREFIX`,
`~/.wine` or a `~/wine…` folder with a `drive_c` inside. For a prefix elsewhere, pick it with
**Choose a folder**.

## Option 4: a copied folder

From any Windows PC with UA's software installed, copy these folders, keeping their names and
their place under `C:\`:

| Copy | What it gives |
|---|---|
| `C:\Program Files\Universal Audio` | the Powered plug-ins: their DSP modules and panel art |
| `C:\Program Files (x86)\Universal Audio` | UAD Console's art, and the USB Apollos' boot images |
| `C:\ProgramData\Package Cache` *(optional)* | the plug-in icons (UA keeps them in its installer cache when no Apollo was connected) |

For example, copy them into a folder `uad-windows` on a USB stick so that it holds
`uad-windows/Program Files/Universal Audio` and `uad-windows/Program Files (x86)/Universal Audio`.
In setup, choose **Choose a folder** and pick `uad-windows`.

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
