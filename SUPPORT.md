# Getting help

open-apollo is a community project; nobody at Universal Audio supports it, so please don't
ask UA for help with it.

## Check these first

| Problem | Where to look |
|---|---|
| Installing, the setup wizard, the driver | [Install guide › Troubleshooting](docs/install.md#troubleshooting) |
| Setup finds no UAD install, BitLocker, Fast Startup, VM disks, boot disks | [Getting a UAD install](docs/uad-install.md) |
| The Console: no art, a control does nothing | [console-rs/README.md](console-rs/README.md#troubleshooting), [what is wired](console-rs/WIRING.md) |
| Plug-in windows | [Install guide › Plug-ins](docs/install.md#plug-ins) |
| Audio stops after a JACK app, browsers, DAWs | [README › Known Issues](README.md#known-issues) and [App Compatibility](README.md#app-compatibility) |
| Hardware details | the [wiki](https://github.com/rolotrealanis98/open-apollo/wiki/Troubleshooting) |

These commands show where things stand. None of them changes anything:

```bash
console-rs/target/release/oa-assets scan       # the UAD installs setup can see
console-rs/target/release/oa-assets status     # what has been imported and built
bash scripts/uninstall.sh --list               # what open-apollo has installed
bash scripts/install-uaconsole.sh --check      # the Console's art, fonts and GPU driver
systemctl --user status ua-mixer-daemon        # the mixer daemon
```

## Opening an issue

Search the [issues](https://github.com/rolotrealanis98/open-apollo/issues) first. Then open a
[bug report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=bug_report.yml)
with:
- your Apollo model, distribution and kernel (`uname -r`);
- where your UAD install came from (dual boot, boot disk, VM, Wine, copied folder) and its
  UAD and UAD Console versions;
- what the wizard showed: the page, and the step that failed with its message;
- the run's log from `~/.local/state/open-apollo/` (the wizard's Done page, or the page where
  a step failed, shows its name; the scripts print it at the end). It leaves out your user
  name, host name, machine id and serial numbers; read it before you attach it. See
  [Install logs](docs/install.md#install-logs-and-the-install-report);
- the report from `bash tools/apollo-report.sh` (it leaves out the host name, user name, serial
  numbers and network details; read it before you attach it).

For an Apollo model nobody has tested yet, a
[device report](https://github.com/rolotrealanis98/open-apollo/issues/new?template=device-report.yml)
helps most: see [Testing your Apollo](tools/TESTING.md).

**Never attach files from your UAD install** (DLLs, `.bill` modules, firmware, the Console's
art) or anything setup extracted from it. They are UA's, and an issue is public.

## Security problems

Don't open a public issue for a security problem: see [SECURITY.md](SECURITY.md).
