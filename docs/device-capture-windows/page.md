---
title: Device Capture (Windows, USB)
---

This guide walks you through capturing the USB traffic the UA driver sends to a **USB Apollo** (Solo USB, Twin USB, Twin X USB) as it powers on under Windows. That traffic is the device's init sequence — firmware, DSP program, routing, preamp state and the UAD plugin chain — and once you have it you can replay it on Linux with `tools/usb-re/init-replay.py`, from your own machine's capture, matched to your own device and firmware.

This is how the Apollo Twin USB was brought up on Linux. It is the USB counterpart of the [macOS DTrace capture](/docs/device-capture-macos) for Thunderbolt models.

The capture is **read-only**: USBPcap observes the traffic between the UA driver and the device. It sends nothing to the hardware.

---

## What you need

- A Windows machine where UA Console already works with the Apollo (any Console version that works; note which one you used)
- [Wireshark](https://www.wireshark.org/) — tick the **USBPcap** component in the installer
- **Reboot after installing.** USBPcap does not attach to the USB root hubs until you do
- An **administrator** PowerShell — USBPcap needs it

{% callout type="warning" %}
**USB 3.0 SuperSpeed capture.** USB Apollos need a SuperSpeed port to expose their audio interfaces, and USBPcap 1.5.x captures SuperSpeed links reliably only after its SuperSpeed support is enabled. If your capture verifies as containing no `2b5a` traffic, run `& "C:\Program Files\USBPcap\USBPcapCMD.exe" -I`, reboot, and try again.
{% /callout %}

---

## The rules that make a capture usable

1. **Start the capture before the Apollo is powered on.** The init sequence happens exactly once, when the device first talks to the driver. If the Apollo is already enumerated when you start capturing, you record nothing useful. Power the Apollo off — not just USB; its own power — so it comes back through the firmware loader.
2. **Capture the whole thing in one file.** Loader enumeration, firmware download, re-enumeration, Console loading its session. Do not stop and restart.
3. **Do not load, unload or adjust plugins while capturing.** Set up Console the way you want the device to come up *before* you start; then capture it coming up. The DSP allocates plugin instances per session, so a sequence captured while things were being rearranged replays a state that never existed.

---

## Step 1: Find the USB root hubs

USBPcap exposes one capture interface per USB root hub. List them with `tshark`, **not** `dumpcap -D` — USBPcap is an *extcap* interface and `dumpcap -D` does not enumerate those at all, so it reports zero even when elevated:

```powershell
& "C:\Program Files\Wireshark\tshark.exe" -D
```

You will see `\\.\USBPcap1`, `\\.\USBPcap2`, … If you do not know which hub the Apollo is on, capture all of them (Step 2 does) and let the verification step tell you.

---

## Step 2: Start the capture (Apollo powered off)

Drive `USBPcapCMD.exe` directly rather than the Wireshark GUI: the GUI dissects every packet as it arrives and drops packets once the Apollo starts streaming isochronous audio. One process per hub, with a large kernel buffer and `--capture-from-new-devices` so a device that enumerates *after* the capture starts is included:

```powershell
mkdir C:\apollo-capture; cd C:\apollo-capture
$hubs = (& "C:\Program Files\Wireshark\tshark.exe" -D) | Select-String 'USBPcap\d+' | ForEach-Object { $_.Matches.Value }
foreach ($h in $hubs) {
  Start-Process "C:\Program Files\USBPcap\USBPcapCMD.exe" -WindowStyle Hidden -ArgumentList @(
    '-d', "\\.\$h", '-o', "$h.pcap", '-A', '--inject-descriptors', '--capture-from-new-devices',
    '-b', '134217728', '-s', '65535')
}
```

(If you already know the hub, one `dumpcap -i \\.\USBPcapN -w apollo-init.pcapng` from an administrator prompt does the same job for that hub.)

---

## Step 3: Power the Apollo on and let it settle

Now power the Apollo on. Wait for UA Console to fully load and the front panel to settle — 30 to 60 seconds; the DSP load meter in Console stops changing. Then let it idle another 10 seconds.

---

## Step 4: Stop, merge, verify

```powershell
Get-Process USBPcapCMD | Stop-Process
& "C:\Program Files\Wireshark\mergecap.exe" -w apollo-init.pcapng (Get-ChildItem *.pcap | Where-Object Length -gt 24)
```

Verify before you leave Windows — it is much cheaper to redo now:

```powershell
$t = "C:\Program Files\Wireshark\tshark.exe"
# Which Apollo PIDs appeared? You want BOTH the loader PID and the live PID.
& $t -r apollo-init.pcapng -Y 'usb.idVendor == 0x2b5a' -T fields -e usb.idProduct | Sort-Object -Unique
# The firmware download (a few hundred frames) — proves it was a cold start
& $t -r apollo-init.pcapng -Y 'usb.setup.bRequest == 160 && usb.bmRequestType == 0x40' | Measure-Object
# DSP command packets — thousands for a full init
& $t -r apollo-init.pcapng -Y 'usb.endpoint_address == 0x01 && usb.capdata' | Measure-Object
```

| Loader PID | Live PID | Model |
|---|---|---|
| `0x000c` | `0x000d` | Apollo Solo USB |
| `0x0001` | `0x0002` | Apollo Twin USB |
| `0x000e` | `0x000f` | Apollo Twin X USB |

If only the live PID appears, the Apollo was not actually powered down — its firmware survives as long as it has power. If nothing appears, see the SuperSpeed note above.

A full Twin init capture is about 3 MB. Note your Console version and the device's serial prefix alongside it.

---

## Step 5: What is in the file, and what to do with it

The capture contains everything the driver sent: the **FX3 firmware image** and the **DSP program data** that UA ships with its driver, your routing and preamp settings, and the plugin chain that was loaded. It contains no audio (nothing was streaming) and nothing about your account or authorisations.

Because it carries UA's firmware and DSP program, **keep the capture on your own machine**, the same way the project asks you to obtain `ApolloTwin.bin` from UA yourself rather than from the repo. The project needs the *recipe* and the *findings* — not the bytes. If you want to contribute, the useful things to share in a [Device Report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml) are: your model, Console version, the packet counts from Step 4, the phase table from Step 6, and whether the replay brought the device up.

---

## Step 6: Build and replay the init sequence on Linux

Copy the `.pcapng` to your Linux machine (`tshark` must be installed there too), then:

```bash
# Extract every DSP command packet and vendor control transfer, in order, with timing.
# The firmware download is excluded — tools/fx3-load.py handles firmware separately.
python3 tools/usb-re/pcap-init-extract.py apollo-init.pcapng -o apollo-init.seq
```

It finds the live Apollo in the capture by PID, prints the number of bulk and control events, and a phase table with the registers each phase writes. The last line names the phase that carries the plugin-chain upload (register `0x000C`) — note it, because phase numbers are a property of *your* capture (they come from where the driver paused), not of the protocol. Two warnings matter: *no firmware download* means it was not a cold start; *no control transfers* means the sequence is roughly half the init.

Replay it with the Apollo connected, firmware loaded, and the device **idle for at least 30 seconds** after it re-enumerated with its live PID — started earlier, a cold DSP acknowledges the whole sequence and silently skips the plugin chain:

```bash
sudo python3 tools/usb-re/init-replay.py apollo-init.seq --dry-run   # look first
sudo python3 tools/usb-re/init-replay.py apollo-init.seq             # replay every phase
```

A good run reports `0 errors` and several hundred responses, most of them in the plugin-chain phase. Then check capture with `arecord -l` / PipeWire as usual. To bring the device up **without** the plugin chain (converters, routing, monitoring only), replay every phase before the one the tool named — e.g. `--phases 0-9` if it said the chain is phase 10.

What comes up is exactly what Console had loaded when you captured: the same Unison preamp, inserts and settings, frozen. To change the chain, change it in Console and capture again.

The sequence is specific to your model and firmware build; replaying a Twin's sequence on a Solo, or across a firmware update, is not expected to work.

---

## Optional: capture the mixer controls too

With the device up, `tools/usb-re/mixer-sweep.py` drives every preamp and monitor control over Console's TCP port 4710 while a second capture runs, and `tools/usb-re/pcap-mixer-decode.py` correlates the two afterwards. That is how the vendor-control mixer protocol on this page's parent was mapped. `pcap-mixer-decode.py` reads `.pcap` only; convert a merged file with `editcap -F pcap in.pcapng out.pcap`.

---

## Next steps

- [USB reverse-engineering findings](/docs/usb-apollo-re) — what the captured protocol means
- [Submitting Your Data](/docs/submitting-data) — how to report what you found
- [How to Contribute](/docs/how-to-contribute) — other ways to help
