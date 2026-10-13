# CI

Everything runs from one workflow, [`workflows/ci.yml`](workflows/ci.yml), on pull requests
to `main`, on pushes to `main` and by hand (`workflow_dispatch`, which runs every job). A
newer push to the same PR or branch cancels the run before it.

The `changes` job reads which files the PR (or push) touches and starts only the jobs those
files can affect. A job skipped this way reports success, so required checks never wait on a
job that had nothing to check. Any change to `ci.yml` itself runs every job.

| Job (status check) | Runs when these change | What it does |
|---|---|---|
| `changes` | always | Path filters (dorny/paths-filter) for the jobs below |
| `clean-repo` | always | `tools/test-check-clean-repo.py`, then `tools/check-clean-repo.py`: no UA bytes and no personal data in any tracked file |
| `driver` | `driver/`, `mixer-engine/hardware.py` | Builds the module against Ubuntu's generic kernel headers (the runner's Azure kernel has no `CONFIG_SND`), then `pytest driver/tests` |
| `daemon` | `mixer-engine/`, `tools/`, `devices/`, `docs/`, `driver/` | ruff (E9, F63, F7, F82) and compileall; `pytest mixer-engine/tests` and `mixer-engine/test_console_*.py` with `HOME` in `$RUNNER_TEMP`; `gen-driver-models.py --check`, `check-models.py`, `gen-status-graph.py --check`; the tool unit tests |
| `console-rs` | `console-rs/`, `mixer-engine/device_maps/` | `cargo build`/`cargo test --workspace --locked` (cached with Swatinem/rust-cache, saved from `main` only); `cargo fmt --check` for the crates that follow `console-rs/rustfmt.toml` (`oa-assets`); `clippy -D warnings` for `oa-assets` and `oa-setup`; `cargo check` with the `webview` feature when `plugin-ui`, `uaconsole` or `Cargo.lock` change |
| `installer` | `scripts/`, `tests/`, `configs/`, `tools/**/*.sh` | `shellcheck --severity=error` over every shell script in `scripts/ tools/ configs/ tests/`; every `tests/test-install-*.sh` (except the docker matrix), `tests/test-host-checks.sh`, `tests/test-uninstall.sh` and `tests/test-setup-root.sh` |
| `install (ubuntu, fedora, debian, arch, opensuse)` | `driver/`, `scripts/`, `configs/`, `tests/docker/` | `scripts/install.sh` end to end in each distro's container |
| `nix` | PRs: `flake.nix`, `nix/`, the Cargo manifests and lock file; `main`: also any `console-rs/` change | `nix flake check` (builds `uaconsole` and runs its tests in the sandbox) |

## Branch protection for `main`

Require a pull request, and require these status checks to pass:

- `changes`
- `clean-repo`
- `driver`
- `daemon`
- `console-rs`
- `installer`

`changes` is required because the other jobs start from it: if it fails they are skipped,
and a skipped job counts as passing. GitHub lists a check for selection only after it has
run once in the repository.

Leave these advisory (visible on every PR, not required):

- `install (…)`: the containers install from live distro mirrors, and Arch and openSUSE
  Tumbleweed are rolling releases, so they can break without any change of ours. Make
  `install (ubuntu)` and `install (debian)` required if installer regressions slip through.
- `nix`: it follows `nixos-unstable` (there is no `flake.lock`) and is not run for every
  console-rs PR.

The last file `clean-repo` flagged, `tools/usb-re/init-bulk-sequence.bin` (a Windows USB
capture that held UA DSP modules), is gone from the tree: the USB init now builds that stream
from the user's own extracted modules and `tools/uausb/solo-usb-init.json`. Older commits still
carry it until the history rewrite; turn the check on as required once `main` passes
`python3 tools/check-clean-repo.py`.

## The clean-repo guard

`tools/check-clean-repo.py` (Python standard library only) checks every tracked file. Run it
before you push; `--rev <commit>` checks a commit without checking it out.

| Rule | Fails on |
|---|---|
| `ua-bill` | a UA DSP module ("Bill" record header: `Bill`, id with arch byte 0 or 2, `0x02000000` or 0, payload size, w4), as raw bytes or written out as C bytes, hex words, decimal arrays, hex strings or base64 |
| `ua-pinned` | a file, a Bill record or an embedded image whose SHA-256 is pinned in `console-rs/crates/oa-assets/data/firmware-pins.json` (only the hashes are read) |
| `raster-image` | PNG, JPEG, GIF, BMP, WebP, ICO, TIFF files (by name or content) and `data:image/…;base64` URIs, unless reviewed and pinned by SHA-256 in the allowlist. Screenshots of console-rs show UA's art: never commit them |
| `private-ipv4` | 10/8, 172.16/12 and 192.168/16 addresses. Use 192.0.2.x (RFC 5737) in examples |
| `home-path` | `/home/<name>`, `/Users/<name>`, `C:\Users\<name>`, `drive_c/users/<name>`. Use `/home/you` |
| `email` | addresses other than `*@users.noreply.github.com`, the RFC 2606 example domains and the project contact |
| `serial` | Apollo-style serial numbers (14 digits, or 12 to 16 from a 2) other than the synthetic test serials |
| `host-name` | machines named after a person (`<Name>'s Mac Studio`) |
| `large-file` | binaries over 256 KiB, any file over 2 MiB |

Findings print as `path:line: [rule] message` (binary files give the byte offset). Reviewed
exceptions go in `tools/check-clean-repo.allow`, each with its reason. Never put UA bytes, UA
text or a real person's data there.

## GitHub Actions minutes

The private hub's minutes come from the account's monthly allowance (2,000 on GitHub Free,
3,000 on Pro), billed per job and rounded up to the minute; the public repository's are free.
Rough figures for a 2-core `ubuntu-24.04` runner:

| Change | Jobs | Minutes |
|---|---|---|
| Docs only (`README.md`, `CHANGELOG.md`) | `changes`, `clean-repo` | 2 |
| Daemon or tools (Python) | + `daemon` | 5 |
| console-rs | + `console-rs` | 8 to 12 with a warm cache, 20 to 25 cold (first run after a Rust release or a `Cargo.lock` change on `main`) |
| Driver | + `driver`, `daemon`, the 5 `install` jobs | about 25 |
| Installer scripts | + `installer`, the 5 `install` jobs | about 22 |
| `ci.yml` itself, or `workflow_dispatch` | everything | about 60 to 70 |

The push to `main` after a merge runs the same jobs again (it keeps the Rust cache warm for
the next PRs), plus `nix` (about 25 minutes) when console-rs changed. A typical PR therefore
costs its row above twice. The `install` matrix and `nix` are the expensive jobs; trim the
matrix to `ubuntu` and `arch` on PRs if the allowance runs short.
