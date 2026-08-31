# macOS Capture Script — What This Does

This document explains exactly what `capture.sh` does, line by line, so you can
make an informed decision before running it.

The macOS contribution tools have three entry points:

- `capture-nosip.sh` collects device identity and channel counts without DTrace.
- `apollo-selector-smoke.d` checks whether the selectors fire before a full capture.
- `apollo-capture.d` captures routing and program payloads. `capture.sh` runs this file.

## What is SIP?

**System Integrity Protection (SIP)** is a macOS security feature that prevents
even the root user from modifying certain system files and using certain
debugging tools. DTrace is one of the tools restricted by SIP.

SIP must be temporarily disabled to run DTrace. You should **re-enable SIP
immediately after capturing**. The script prints re-enable instructions at the
end.

Apple's official documentation:
https://support.apple.com/en-us/102149

## What is DTrace?

DTrace is a kernel-level tracing framework built into macOS (and other Unix
systems). It allows observing function calls and data flowing through the
system — without modifying anything. Think of it as a read-only wiretap on
software calls.

DTrace is a standard system tool, not third-party software.

## What data is captured?

The script captures information that the Universal Audio driver exchanges with
its mixer software:

1. **Device configuration** — Model name, device type ID, channel counts, and
   sample rate. This tells us what kind of Apollo you have and how many
   audio channels it supports.

2. **Routing tables (SEL171)** — How audio channels are mapped between DMA
   (the data transfer mechanism) and the DSP (the audio processor). This is
   the key data needed to support new Apollo models on Linux.

3. **IORegistry info** — Basic device registration data that macOS maintains
   for connected audio devices.

## What probes are used?

The DTrace probes target the `IOConnectCallStructMethod` function, which is
the standard macOS API for communicating with kernel drivers. The probes
intercept calls from the UA Mixer Engine process to read:

- **Selector 171 (routing)**: The routing table that maps audio channels
- **Device identification**: Model type and capabilities

These are the same calls the UA software makes during normal operation. The
DTrace probes only observe — they do not inject, modify, or replay any calls.

## What this does NOT do

- **ZERO writes to hardware** — The script only reads data that is already
  being exchanged between software components
- **No network calls** — All output is saved to a local file. Nothing is
  sent anywhere
- **No system modifications** — No files are installed, no settings changed
- **No persistent changes** — Once the script finishes, DTrace stops tracing

## Output

The script produces a single JSON file containing the captured data. You can
(and should) review this file before submitting it. It contains only hardware
identifiers and configuration data — no personal information, no audio data,
no filesystem paths.

## Re-enabling SIP

After capturing, re-enable SIP immediately:

1. Restart your Mac and hold **Cmd+R** to enter Recovery Mode
2. Open **Terminal** from the Utilities menu
3. Run: `csrutil enable`
4. Restart normally

Verify with: `csrutil status` — it should say "enabled".
