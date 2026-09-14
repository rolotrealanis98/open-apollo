# macOS Capture Script — What This Does

This document explains exactly what `capture.sh` does, line by line, so you can
make an informed decision before running it.

The macOS contribution tools have two entry points:

- `capture-nosip.sh` collects device identity and channel counts without DTrace.
- `capture.sh` runs `apollo-selectors.d` to capture routing and program payloads.

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
the standard macOS API for communicating with kernel drivers. The probe script
is `apollo-selectors.d`, next to `capture.sh` — you can read it before running,
and run it standalone if you prefer. It records the raw buffer for these
selectors:

| Selector | What it carries |
|---|---|
| 171 | Routing table — channel IDs, types and names, per direction |
| 130, 131 | Per-channel program burst, including the IO descriptor blocks |
| 113, 129 | Large payloads, contents not yet identified |
| 100 | DSP image list |
| 127 | Firmware block set |
| 189 | Stream format — channel count and type per direction |
| 102 | Transport commit — client name, sample rate, buffer size |

These are the same calls the UA software makes during normal operation. The
DTrace probes only observe — they do not inject, modify, or replay any calls.

Note that the routing table is sent when Console attaches to the device, not
continuously. If Console is already running and idle for the whole capture
window, the result will be empty — quit and relaunch Console while the capture
is running.

## What this does NOT do

- **ZERO writes to hardware** — The script only reads data that is already
  being exchanged between software components
- **No system modifications** — No files are installed, no settings changed
- **No persistent changes** — Once the script finishes, DTrace stops tracing

## Network

The capture itself makes no network calls, and every file is written locally
first. At the very end the script offers to upload the JSON report:

- **Run interactively** (the normal case) it asks first and defaults to **no**.
- **Run non-interactively** — piped, or over SSH with no TTY — it **uploads
  without asking**, because there is no way to prompt.

If you would rather nothing left the machine at all, answer `n` at the prompt,
or run with stdin attached to a terminal so the prompt appears. Only the JSON
report is ever uploaded; the raw selector dumps are never sent automatically.

## Output

The script produces two files:

- `apollo-macos-capture-<timestamp>.json` — the report: system info, driver
  version, IORegistry extract, and a per-selector summary (which selectors were
  seen, how many calls, how many bytes). This is the file the upload sends.
- `apollo-macos-capture-<timestamp>-selectors.txt` — the raw hexdumps. These
  stay local unless you attach them to an issue yourself. They are also the
  part that is actually useful for routing work, so please do attach them.

You can (and should) review both before submitting. They contain hardware
identifiers, driver configuration and the audio routing payloads — no personal
information, no audio content, no filesystem paths.

## Re-enabling SIP

After capturing, re-enable SIP immediately:

1. Restart your Mac and hold **Cmd+R** to enter Recovery Mode
2. Open **Terminal** from the Utilities menu
3. Run: `csrutil enable`
4. Restart normally

Verify with: `csrutil status` — it should say "enabled".
