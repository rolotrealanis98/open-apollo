---
title: How to Contribute
---

Open Apollo is a community-driven project. We need help from Apollo owners to test on different models, capture device data from working systems, and contribute code improvements.

---

## Tier 1: Test on Linux (easiest)

If you have an Apollo connected to a Linux machine, this is the most straightforward way to help.

### What to do

1. Build and load the driver ([Installation guide](/docs/installation))
2. Run the device probe script:
   ```bash
   sudo ./tools/contribute/device-probe.sh
   ```
3. Test basic functionality:
   - Does `aplay -l` show your Apollo?
   - Does audio playback work?
   - Does recording work?
   - Do preamp controls respond?
4. [Submit a device report](https://github.com/open-apollo/open-apollo/issues/new?template=device-report.yml) with your probe output and test results

### What we learn

Even a simple "driver loaded, audio plays" report on a model we haven't tested is enormously helpful. It lets us mark that model as verified and gives other users confidence.

---

## Tier 2: Capture device data (advanced)

The most valuable contribution is capturing device configuration data from a working system (macOS). This data tells us exactly how each Apollo model configures its audio routing, which is essential for supporting models we don't have physical access to.

### macOS capture

Requires temporarily disabling System Integrity Protection (SIP) to use DTrace. The capture script is read-only — it observes driver behavior without modifying anything.

See the full guide: [Device Capture (macOS)](/docs/device-capture-macos)

### After capturing

See [Submitting Your Data](/docs/submitting-data) for how to review and submit your capture.

---

## Tier 2b: Run the install matrix

The repository includes Docker-based install matrix tests that validate the driver builds and all configs deploy correctly across supported distros. Running these helps catch regressions:

```bash
bash tests/test-install-matrix.sh
```

This runs Dockerfiles in `tests/docker/` for Ubuntu, Fedora, Arch, Debian, openSUSE, Mint, Pop!_OS, and Manjaro. Requires Docker installed locally.

---

## Tier 3: Code contributions

We welcome pull requests for bug fixes, new features, and documentation improvements.

### Getting started

1. Fork the repository on GitHub
2. Create a feature branch:
   ```bash
   git checkout -b feat/your-feature-name
   ```
3. Make your changes
4. Test your changes (build the driver, run the daemon)
5. Commit using [conventional commits](https://www.conventionalcommits.org/):
   ```bash
   git commit -m "feat: add support for Apollo Twin X preamp routing"
   ```
6. Push and open a pull request

### Commit message format

We use conventional commit prefixes:

| Prefix | Use for |
|---|---|
| `feat:` | New features |
| `fix:` | Bug fixes |
| `docs:` | Documentation changes |
| `refactor:` | Code restructuring without behavior change |
| `test:` | Adding or updating tests |
| `chore:` | Build, CI, or tooling changes |

### What we need most

- **USB device testing** — People with Apollo Twin USB or Twin X USB willing to test `install-usb.sh` and report results
- **Thunderbolt device testing** — People with non-x4 Apollo models willing to test and report
- **Routing table captures** — DTrace or BAR0 captures from untested Thunderbolt models
- **Mixer daemon improvements** — Protocol edge cases, error handling
- **Kernel patch** — Submit the `snd-usb-audio` GET_RANGE quirk for VID `0x2B5A` to alsa-devel
- **Documentation** — Corrections, clarifications, additional examples

---

## Reporting issues

If something doesn't work, please [open an issue](https://github.com/open-apollo/open-apollo/issues/new) with:

- Your Apollo model
- Linux distribution and kernel version (`uname -r`)
- Output of `dmesg | grep ua_apollo`
- What you expected vs. what happened
- Steps to reproduce

---

## Code of conduct

Be respectful, constructive, and patient. This is a reverse-engineering project — things break, behavior is surprising, and progress is incremental. Every contribution, no matter how small, moves the project forward.
