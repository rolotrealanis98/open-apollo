# Contributing to Open Apollo

Thank you for your interest in Open Apollo! Whether you have an Apollo
interface collecting dust on Linux, or you want to dive into driver code,
there's a way for you to help.

## How You Can Help

- **Tier 1: Test on Linux** — load the driver on your hardware and submit a report
- **Tier 2: Capture device data** — help us build routing tables for untested models
- **Tier 3: Map and verify DSP plug-ins** — load a UAD plug-in you own on the Apollo's DSP and check every control
- **Code contributions** — fix bugs, add features, improve documentation

## What We Need Right Now

The [status map](docs/status/) is the full picture. These are the gaps a contributor can close
today, most valuable first:

1. **Device reports from models nobody has run.** 22 of the 25 Apollo models in `devices/` have
   per-model support in the code and no owner has tested it ([the list](docs/status/#apollo-models)).
   Every Gen 2, the x6, x8, x16 and x16D, the Arrow, the Solo and the Twin X USB are all open.
   Follow [Testing your Apollo](tools/TESTING.md) and file a
   [device report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml).
   Negative results count.
2. **Apollo Twin USB owners.** The USB bring-up sends the Solo USB's start-up stream, which stalls
   a Twin's DSP ([#128](https://github.com/rolotrealanis98/open-apollo/issues/128)). A USBPcap
   capture of the Windows driver bringing up a Twin USB is what unblocks it (Tier 2).
3. **Plug-in testers who own the plug-ins.** Dozens of plug-ins are mapped and wait for someone
   with a licence to run the live gate, and most of the plug-ins the Console offers still need
   their measured checks run (controls, text, meters). The tiers, with every name, are in
   [docs/status/PLUGINS.md](docs/status/PLUGINS.md); the steps are in Tier 3 below.
4. **Plug-in mapper work.** Discovery families for the partner frameworks
   ([#127](https://github.com/rolotrealanis98/open-apollo/issues/127)), latency taken from the
   plug-in itself ([#133](https://github.com/rolotrealanis98/open-apollo/issues/133)), and every
   failure listed under *What fails, by check* in PLUGINS.md.
5. **Plug-in meters through the daemon.** Every meter check that fails today fails the same way
   (`plugin_remote_readback_unavailable`): the daemon does not yet publish a plug-in's meter words.
   One feature unblocks them all (`mixer-engine/NATIVE-HOSTING.md`).
6. **Hardware the project does not have.** A second Apollo for multi-unit, S/PDIF, ADAT and
   external clock, suspend/resume: the *Needs hardware* list in [docs/status/](docs/status/).
   If you have the gear, open an issue and we will say what to run.
7. **Docs.** Install reports from distributions other than Ubuntu, and anything in these pages
   that did not match what you saw.

## Tier 1: Test on Linux

This is the most valuable contribution right now. Every device report helps
us understand what works and what needs fixing across the Apollo product line.

### Steps

1. Clone the repo and check dependencies (common to both connection types):
   ```bash
   git clone https://github.com/rolotrealanis98/open-apollo.git
   cd open-apollo
   ./scripts/check-deps.sh
   ```

   If you are unsure which connection type your Apollo uses, see
   [Installation](https://github.com/rolotrealanis98/open-apollo/wiki/Installation).

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

4. Submit the probe output as a [Device Report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml) issue.

### USB Apollo (Solo USB, Twin X USB)

2. Apollo plugged into a USB 3.0 port and powered **on**, run:
   ```bash
   sudo bash ./scripts/install-usb.sh
   ```
   The USB installer generates `/tmp/open-apollo-usb-install-report.json` and
   offers to upload it on completion.

3. Submit that report (plus any audio test notes — playback working? capture
   attempts? PipeWire output?) as a
   [Device Report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml) issue.
   Do **not** run `tools/contribute/device-probe.sh` on USB — it only supports
   the Thunderbolt driver path and will error out.

Even if nothing works, the probe output tells us what we need to know about
your hardware revision. Negative results are still valuable.

## Tier 2: Capture Device Data

For Apollo models we don't have routing tables for yet, we need register-level
captures from a working macOS system using DTrace. See the
[Device Capture (macOS) guide](https://github.com/rolotrealanis98/open-apollo/wiki/Device-Capture-macOS)
for instructions. The capture script is read-only — it observes driver behavior
without modifying anything.

For **USB** models, the equivalent is a USBPcap capture of the Windows driver
bringing the device up, which you then replay on Linux from your own machine.
See the [Device Capture (Windows, USB) guide](https://github.com/rolotrealanis98/open-apollo/wiki/Device-Capture-Windows).

> **Note:** Captures contain only hardware register values — no personal data,
> no audio content, no account information.

## Tier 3: Map and Verify DSP Plug-ins

If you own UAD plug-ins and a Thunderbolt Apollo, you can help grow the list of plug-ins that run
on its DSP from Linux: map a plug-in from your own UAD install, load it, turn every control, check
the hardware readback and the audio, and report the result. The
[plug-in mapping guide](docs/plugin-mapping.md) covers what you need, the tools for each stage,
and how loading with the plug-in emulator differs from loading without it. The maps you build
are made from UA's code and never go into a PR or an issue.

Where every plug-in stands, and what each one still needs, is
[docs/status/PLUGINS.md](docs/status/PLUGINS.md). The best report is a PR with the JSON that
`tools/plugin-qualify.py` writes; if you cannot run it, use the
[plug-in report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=plugin_report.yml)
issue form.

## Code Contributions

### Workflow

1. Fork the repository
2. Create a feature branch from `main`: `git checkout -b feat/my-feature`
3. Make your changes, one change per PR
4. Submit a pull request against `main`

CI (`.github/workflows/ci.yml`, described in [.github/CI.md](.github/CI.md)) runs the checks of
every component your change touches. The slow checks (the driver against every supported
kernel, the installer in each distro's container) run weekly; for a change that risks them, ask
for the `full-ci` label on your PR.

### Before You Open a PR

- **Run the checks for what you touched.** They are the same ones CI runs:

  | You changed | Run |
  |---|---|
  | anything | `python3 tools/check-clean-repo.py` |
  | `driver/` | `make -C driver KDIR=/lib/modules/$(uname -r)/build`, `python3 -m pytest driver/tests -q` |
  | `devices/` | the driver tests, then `python3 tools/gen-driver-models.py --check`, `python3 tools/check-models.py`, `python3 tools/gen-status-graph.py` (commit what it redraws) |
  | `mixer-engine/`, `tools/` | `ruff check --select E9,F63,F7,F82 mixer-engine tools`, `python3 -m pytest mixer-engine/tests -q`, `python3 -m pytest mixer-engine/test_console_*.py -q` |
  | `console-rs/` | `cargo build --workspace --all-targets --locked`, `cargo test --workspace --locked`, `cargo fmt --check -p oa-assets` |
  | `scripts/`, `configs/`, `tests/` | `shellcheck --severity=error` on the scripts you changed, and the `tests/test-*.sh` that covers them |
  | `docs/status/` inputs | `python3 tools/gen-status-graph.py`, then commit the regenerated files |

- **No UA bytes, no personal data.** The repository holds no Universal Audio files (DSP modules,
  firmware, art, screenshots, plug-in bundles) and no personal data (serial numbers, LAN
  addresses, home paths, e-mail addresses). `tools/check-clean-repo.py` refuses them in CI.
- **Fill in the PR template.** Say what the change does and why, tick the type, and under
  *Testing* say which Apollo model, sample rate and UAD version you ran it on. If you could not
  run it on hardware, say so: a maintainer will.
- **Driver changes** come with the register or DSP trace they follow (the driver never guesses:
  see [ARCHITECTURE.md](ARCHITECTURE.md)); **installer changes** come with a test in `tests/`;
  **daemon and tool changes** come with tests that use synthetic I/O, never a UA file.
- **User-visible changes** get a line under *Unreleased* in [CHANGELOG.md](CHANGELOG.md).

A maintainer reviews every PR; CI has to be green before it merges. Small PRs with one change
each merge fastest.

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

- **C (driver):** Linux kernel coding style (clean under the kernel tree's `scripts/checkpatch.pl`)
- **Python (mixer daemon, tools):** PEP 8; CI runs `ruff check --select E9,F63,F7,F82`
- **Rust (console-rs):** `rustfmt` with `console-rs/rustfmt.toml`; `cargo test --workspace` must pass
- **Shell (installers):** `bash`; `shellcheck --severity=error` must pass, and installer changes come with a test in `tests/`
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
