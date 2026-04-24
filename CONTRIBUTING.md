# Contributing to Open Apollo

Thank you for your interest in Open Apollo! Whether you have an Apollo
interface collecting dust on Linux, or you want to dive into driver code,
there's a way for you to help.

## How You Can Help

- **Tier 1: Test on Linux** — load the driver on your hardware and submit a report
- **Tier 2: Capture device data** — help us build routing tables for untested models
- **Code contributions** — fix bugs, add features, improve documentation

## Tier 1: Test on Linux

This is the most valuable contribution right now. Every device report helps
us understand what works and what needs fixing across the Apollo product line.

### Steps

1. Clone the repo and check dependencies (common to both connection types):
   ```bash
   git clone https://github.com/open-apollo/open-apollo.git
   cd open-apollo
   ./scripts/check-deps.sh
   ```

   If you are unsure which connection type your Apollo uses, see
   [Installation](https://open-apollo.org/docs/installation).

### Thunderbolt Apollo (x-series, Twin/Arrow Thunderbolt)

2. Apollo powered **off**, run the installer from the repo root; power the
   Apollo on when the installer prompts:
   ```bash
   sudo bash ./scripts/install.sh
   ```

3. From the repo root, run the device probe script:
   ```bash
   sudo ./tools/contribute/device-probe.sh
   ```
   The probe writes a JSON report and offers to upload it. The probe currently
   expects the Thunderbolt `ua_apollo` kernel module and uses `lspci` — it is
   **not** compatible with USB Apollos.

4. Submit the probe output as a [Device Report](https://github.com/open-apollo/open-apollo/issues/new?template=device-report.yml) issue.

### USB Apollo (Solo USB, Twin X USB)

2. Apollo plugged into a USB 3.0 port and powered **on**, run:
   ```bash
   sudo bash ./scripts/install-usb.sh
   ```
   The USB installer generates `/tmp/open-apollo-usb-install-report.json` and
   offers to upload it on completion.

3. Submit that report (plus any audio test notes — playback working? capture
   attempts? PipeWire output?) as a
   [Device Report](https://github.com/open-apollo/open-apollo/issues/new?template=device-report.yml) issue.
   Do **not** run `tools/contribute/device-probe.sh` on USB — it only supports
   the Thunderbolt driver path and will error out.

Even if nothing works, the probe output tells us what we need to know about
your hardware revision. Negative results are still valuable.

## Tier 2: Capture Device Data

For Apollo models we don't have routing tables for yet, we need register-level
captures from a working macOS system using DTrace. See the
[Device Capture (macOS) guide](https://open-apollo.org/docs/device-capture-macos)
for instructions. The capture script is read-only — it observes driver behavior
without modifying anything.

> **Note:** Captures contain only hardware register values — no personal data,
> no audio content, no account information.

## Code Contributions

### Workflow

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/my-feature`
3. Make your changes
4. Submit a pull request

### Commit Messages

We use [conventional commits](https://www.conventionalcommits.org/):

- `feat:` — new feature
- `fix:` — bug fix
- `docs:` — documentation changes
- `refactor:` — code restructuring without behavior change
- `test:` — adding or updating tests
- `chore:` — maintenance tasks

### Examples

```
feat: add Apollo x8 routing table
fix: correct DMA buffer alignment for 96kHz sample rates
docs: update supported devices table
```

## Code Style

- **C (driver):** Linux kernel coding style (`scripts/checkpatch.pl` compatible)
- **Python (mixer daemon):** PEP 8
- **TypeScript (console UI):** Standard ES6+ conventions
- **File size:** Keep files under 200 lines where practical — split into focused modules

## Reporting Issues

### Bug Reports

Please include:
- Apollo model and generation
- Linux distribution and kernel version (`uname -r`)
- Driver version / commit hash
- Steps to reproduce
- Relevant `dmesg` output

### Feature Requests

Open an issue describing what you'd like to see and why. If it involves
hardware behavior, include any references or observations that might help
with implementation.
