# CI

Two workflows. [`workflows/ci.yml`](workflows/ci.yml) is the per-change check: it runs on pull
requests to `main`, on pushes to `main` and by hand (`workflow_dispatch`, which runs every job
in it), and starts at most six jobs. [`workflows/deep.yml`](workflows/deep.yml) holds the slow
cross-checks (11 jobs) and runs weekly, by hand, or on a PR that gets the `full-ci` label. In
both, a newer push or run for the same PR or branch cancels the one before it.

## ci.yml: every PR and every push to `main`

The `changes` job reads which files the PR (or push) touches and starts only the jobs those
files can affect. A job skipped this way reports success, so required checks never wait on a
job that had nothing to check. Any change to `ci.yml` itself runs every job.

| Job (status check) | Runs when these change | What it does |
|---|---|---|
| `changes` | always | Path filters (dorny/paths-filter) for the jobs below |
| `clean-repo` | always | `tools/test-check-clean-repo.py`, then `tools/check-clean-repo.py`: no UA bytes and no personal data in any tracked file; the panel sprites fit their specs (`tools/check-sprite-fit.py`) |
| `driver` | `driver/`, `devices/`, `mixer-engine/hardware.py`, `tools/gen-driver-models.py`, `tools/check-models.py` | Builds the module against Ubuntu's generic kernel headers (the runner's Azure kernel has no `CONFIG_SND`), `pytest driver/tests`, then the model checks: `gen-driver-models.py --check` and `check-models.py` (`devices/*.json` against the driver sources) |
| `daemon` | `mixer-engine/`, `tools/`, `devices/`, `docs/status/` | ruff (E9, F63, F7, F82) and compileall; `pytest mixer-engine/tests` and `mixer-engine/test_console_*.py` with `HOME` in `$RUNNER_TEMP`; `gen-status-graph.py --check`; the tool unit tests |
| `console-rs` | `console-rs/`, `mixer-engine/device_maps/` | `cargo build`/`cargo test --workspace --locked` (cached with Swatinem/rust-cache, saved from `main` only); `cargo fmt --check` for the crates that follow `console-rs/rustfmt.toml` (`oa-assets`); `clippy -D warnings` for `oa-assets` and `oa-setup`; `cargo check` with the `webview` feature when `plugin-ui`, `uaconsole` or `Cargo.lock` change |
| `installer` | `scripts/`, `tests/`, `configs/`, `tools/**/*.sh` | `shellcheck --severity=error` over every shell script in `scripts/ tools/ configs/ tests/`; every `tests/test-install-*.sh` (except the docker matrix), `tests/test-host-checks.sh`, `tests/test-uninstall.sh` and `tests/test-setup-root.sh` |

The push to `main` after a merge runs the same filtered set again: `main` has no "branch must
be up to date" rule, so a merge can combine changes no PR run saw together, and that push is
what saves the Rust cache the next PRs restore.

## deep.yml: weekly, by hand, or with the `full-ci` label

| Job (status check) | What it does |
|---|---|
| `driver (5.15, 6.1, 6.6, 6.8, 6.12, 7.2)` | The module against each kernel line the driver supports: kernel.org sources, defconfig plus the sound/PCI options the driver needs, `modules_prepare`, `make W=1` with `KBUILD_MODPOST_WARN`; fails on any warning from the plug-in host files. Compile test only |
| `install (ubuntu, fedora, debian, arch, opensuse)` | `scripts/install.sh` end to end in each distro's container (module build against that distro's headers, WirePlumber and UCM2 configs) |

It runs every Monday at 05:23 UTC on `main`, and from the Actions tab or
`gh workflow run deep.yml --ref <branch>` on any branch that has it. On a PR, add the `full-ci`
label when the change is risky for other kernels or distros: a driver change that touches
kernel APIs, `scripts/install.sh`, `configs/`, `tests/docker/`. The label starts one run on the
PR as it is; after a newer push, remove and re-add it (or use `gh workflow run`). Labels other
than `full-ci` start a run whose jobs all skip.

GitHub turns a scheduled workflow off after 60 days without activity in a public repository;
re-enable it on the Actions tab if the weekly run stops.

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

Never require the `deep.yml` checks: they do not run on every PR, so a required one would
block every PR without the label.

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
| Docs only (`README.md`, `CHANGELOG.md`, `docs/` outside `docs/status/`) | `changes`, `clean-repo` | 2 |
| Daemon or tools (Python), `docs/status/` | + `daemon` | 5 |
| console-rs | + `console-rs` | 8 to 12 with a warm cache, 20 to 25 cold (first run after a Rust release or a `Cargo.lock` change on `main`) |
| Driver | + `driver` | 4 |
| `devices/` | + `driver`, `daemon` | 8 |
| Installer scripts | + `installer` | 3 |
| `ci.yml` itself, or `workflow_dispatch` of `ci.yml` | all six | 15 to 20 |
| `deep.yml` (weekly, by hand, `full-ci`) | the 11 deep jobs | about 20 |

A typical PR costs its row twice (the PR, then the push to `main` after the merge), plus about
20 minutes a week for `deep.yml`.
