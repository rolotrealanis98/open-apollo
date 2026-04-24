---
title: Device Capture (macOS)
---

This guide walks you through capturing device configuration data from your Apollo on macOS using DTrace. This data is essential for supporting Apollo models we don't have physical access to.

The capture script is **read-only** — it observes data the UA driver is already exchanging with its mixer software. It makes zero writes to hardware.

---

## Important: System Integrity Protection (SIP)

DTrace requires System Integrity Protection to be disabled. SIP is a macOS security feature that restricts certain system-level operations, including kernel tracing.

### What SIP does

SIP prevents modification of protected system files and restricts debugging tools like DTrace from attaching to system processes. It is an important security layer, and you should only disable it temporarily for this capture.

### Security implications

With SIP disabled, software running as root has fewer restrictions on what it can access. This is why you should:

- Only disable SIP on a machine you trust
- Run only the capture script while SIP is disabled
- Re-enable SIP immediately after capturing

Apple's official SIP documentation: [https://support.apple.com/en-us/102149](https://support.apple.com/en-us/102149)

---

## Step 1: Disable SIP

1. Shut down your Mac
2. Boot into Recovery Mode:
   - **Apple Silicon**: Hold the power button until "Loading startup options" appears, then select Options
   - **Intel**: Hold **Cmd+R** during startup
3. Open **Terminal** from the Utilities menu
4. Run:
   ```bash
   csrutil disable
   ```
5. Restart your Mac normally

Verify SIP is disabled:

```bash
csrutil status
# Should say: "System Integrity Protection status: disabled"
```

---

## Step 2: Prepare for capture

Make sure:

- Your Apollo is connected via Thunderbolt and powered on
- **UA Console** or **UA Connect** is running (the capture observes communication between the UA mixer software and the driver)
- UAD software is installed

Clone the repository if you haven't already:

```bash
git clone https://github.com/rolotrealanis98/open-apollo.git
cd open-apollo
```

---

## Step 3: Run the capture

> **Note:** The macOS capture script is a work-in-progress placeholder and may not produce complete capture data yet. We are actively improving it — partial captures are still useful, so please submit what you get.

```bash
sudo ./tools/contribute/macos/capture.sh
```

The script will:

1. Verify SIP is disabled
2. Check that the UA driver is loaded
3. Capture device configuration from IORegistry
4. Run DTrace probes for approximately 10 seconds to capture routing data
5. Save everything to a JSON file

The output file is saved in the current directory with a timestamp, for example: `apollo-macos-capture-YYYYMMDD-HHMMSS.json`

---

## Step 4: Review the capture

Before submitting, open the JSON file and review its contents. It contains:

- **System info**: macOS version and architecture
- **Device info**: Apollo model name, device type, channel counts
- **Routing data**: How audio channels are mapped between DMA and DSP

It does **not** contain: personal information, audio data, filesystem paths, network information, or any data unrelated to the Apollo hardware configuration.

---

## Step 5: Re-enable SIP immediately

This is critical. Do not leave SIP disabled longer than necessary.

1. Shut down your Mac
2. Boot into Recovery Mode (same method as Step 1)
3. Open **Terminal** from the Utilities menu
4. Run:
   ```bash
   csrutil enable
   ```
5. Restart your Mac normally

Verify SIP is re-enabled:

```bash
csrutil status
# Should say: "System Integrity Protection status: enabled"
```

---

## What the script captures (technical details)

The DTrace probes target `IOConnectCallStructMethod`, the standard macOS API for kernel driver communication. The probes intercept calls from the UA Mixer Engine process to read:

- **Selector 171 (routing table)**: Maps audio channels between DMA and DSP — this is the key data needed for Linux support
- **Device identification**: Model type and hardware capabilities

These are the same calls the UA software makes during normal operation. The probes only observe — they do not inject, modify, or replay any calls.

See `tools/contribute/macos/WHAT-THIS-DOES.md` in the repository for a line-by-line explanation of the capture script.

---

## Next steps

- [Submitting Your Data](/docs/submitting-data) — how to submit your capture
- [How to Contribute](/docs/how-to-contribute) — other ways to help
