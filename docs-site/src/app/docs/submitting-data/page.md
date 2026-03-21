---
title: Submitting Your Data
---

After capturing device data from your Apollo interface, this page explains how to review your capture, check for sensitive information, and submit it to the Open Apollo project.

---

## Before you submit

### Review your capture file

Open the JSON capture file in a text editor and verify:

1. **Device information is present** — the file should contain device type, serial prefix, channel counts, and firmware version
2. **No personal data** — check that the file does not include filesystem paths, usernames, or network addresses that you do not want to share
3. **File is valid JSON** — ensure the file parses correctly:

```bash
python3 -m json.tool your_capture.json > /dev/null
```

### What is included in a capture

Captures typically contain:

- **Device model and type** — serial prefix, device type ID, DSP variant
- **Register values** — BAR0 register snapshots (hardware configuration state)
- **Channel counts** — playback and record channel counts at each sample rate
- **Firmware version** — device firmware identification
- **State tree** — mixer control structure (names, types, ranges, defaults)

### What is NOT included

The capture tools do not collect:

- Audio data or recordings
- User account information or credentials
- Network configuration or IP addresses
- Operating system user profiles
- Plugin license or authorization data

---

## How to submit

### Option 1: GitHub Issue (recommended)

1. Go to the [Open Apollo GitHub repository](https://github.com/open-apollo/open-apollo/issues)
2. Click **New Issue**
3. Select the **Device Data Submission** template (if available), or create a blank issue
4. Title your issue: `Device data: Apollo <model name>`
5. In the description, include:
   - Your Apollo model (e.g., Apollo x8p, Arrow, Twin X)
   - The operating system you captured from (macOS or Windows)
   - Firmware version (if known)
   - Any notes about your configuration (sample rate, connected peripherals)
6. Drag and drop your capture file(s) into the issue, or attach them as a `.json` or `.zip` file

### Option 2: Pull request

If you are comfortable with Git:

1. Fork the repository
2. Add your capture file to the `device_maps/` directory
3. Name it descriptively: `device_map_apollo_<model>.json`
4. Open a pull request with a description of your device

---

## What happens after submission

1. **Review** — A maintainer reviews the submitted data for completeness and validity
2. **Device profile** — The data is used to create or update a device map for your Apollo model
3. **Testing** — If hardware access is available, the device map is tested against a real unit
4. **Integration** — The device profile is added to the project, enabling support for your model

Submissions for models not yet supported are especially valuable, as each Apollo variant has different channel counts, preamp configurations, and routing capabilities.

---

## Privacy

You are sharing hardware register data and mixer control structures. This data describes your Apollo's configuration, not your personal information.

If you have concerns about any data in the capture file, you are welcome to:

- Redact the serial number (replace with zeros)
- Remove any fields you are uncomfortable sharing
- Ask a maintainer to review the file privately before public posting

The project does not collect or store any personal information beyond what you voluntarily submit in the GitHub issue.

---

## How your contribution helps

Each Apollo model has a unique combination of:

- **Channel counts** — different numbers of analog, digital, and virtual channels
- **Preamp configuration** — number of preamps, Hi-Z inputs, phantom power channels
- **DSP capabilities** — SOLO, DUO, QUAD, or OCTO DSP variants
- **Routing topology** — how inputs and outputs are wired internally

A device capture provides the exact control tree structure the daemon needs to serve clients for that model. Without captures from real devices, adding support for new models requires guesswork.

Currently supported models have been developed and tested with captured data from real hardware. Your submission directly enables support for more Apollo interfaces.

---

## Further reading

- [Device Capture on macOS](/docs/device-capture-macos) — how to capture data from macOS
- [Device Capture on Windows](/docs/device-capture-windows) — how to capture data from Windows
- [Supported Devices](/docs/supported-devices) — current device support status
