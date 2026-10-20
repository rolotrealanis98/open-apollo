# open-apollo documentation

## Using open-apollo

| Document | What it covers |
|---|---|
| [Install guide](install.md) | Requirements, the setup wizard page by page, the command-line install, USB Apollos, updating and uninstalling, where every file goes, troubleshooting |
| [open-apollo on Asahi Linux](asahi-linux.md) | Apple Silicon Macs: building and booting a kernel with Thunderbolt PCIe tunneling, then installing open-apollo on it |
| [Getting a UAD install](uad-install.md) | The UAD install setup reads from: Windows on another partition or a boot disk, a Windows VM, a Wine prefix, or a copied folder; what setup takes from it |
| [Getting help](../SUPPORT.md) | What to check first, and what to attach to an issue |
| [Mixer console](../console-rs/README.md) | `uaconsole`: installing, running, where its art comes from, options, troubleshooting |
| [Console wiring](../console-rs/WIRING.md) | Which Console controls reach the Apollo, and a hardware checklist |
| [Testing your Apollo](../tools/TESTING.md) | Device reports and self-tests for models nobody has verified yet |
| [NOTICE](../NOTICE.md) | What open-apollo contains, what it reads from your UAD install, trademarks |

## Developing open-apollo

| Document | What it covers |
|---|---|
| [Architecture](../ARCHITECTURE.md) | The repository's layout and how the driver, daemon, console and setup fit together |
| [Contributing](../CONTRIBUTING.md) | Workflow, commit messages, code style, and the rule that no UA files or personal data are committed |
| [Plug-in mapping](plugin-mapping.md) | Mapping, loading and verifying a DSP plug-in: what you need, the tools for each stage, the emulator's role, what to report |
| [CI](../.github/CI.md) | What CI checks for each component |
| [console-rs architecture](../console-rs/ARCHITECTURE.md) | The console's crates and frame pipeline |
| [Plug-in panel runtime](../console-rs/crates/plugin-ui/RUNTIME-V2.md) | The schema-2 panel specs and the elements plugin-ui draws itself |
| [Status map](status/) | Every behaviour of UA's drivers, every task of the work plan and every model, by status |
| [Changelog](../CHANGELOG.md) | What changed |

Plug-in panel specs are kept in their own repository,
[open-apollo-panels](https://github.com/rolotrealanis98/open-apollo-panels).

## Hardware reference (wiki)

The [GitHub wiki](https://github.com/rolotrealanis98/open-apollo/wiki) holds the reverse-engineering
notes: the [register map](https://github.com/rolotrealanis98/open-apollo/wiki/Register-Map), the
[DSP protocol](https://github.com/rolotrealanis98/open-apollo/wiki/DSP-Protocol), the
[initialization sequence](https://github.com/rolotrealanis98/open-apollo/wiki/Initialization-Sequence),
[sample rates](https://github.com/rolotrealanis98/open-apollo/wiki/Sample-Rates), and how to
capture device data on [Windows](https://github.com/rolotrealanis98/open-apollo/wiki/Device-Capture-Windows)
or [macOS](https://github.com/rolotrealanis98/open-apollo/wiki/Device-Capture-macOS).
