# Notice

## About This Project

This project is an independent, community-driven effort to bring Linux support
to Universal Audio Apollo audio interfaces. It is not affiliated with, endorsed
by, or sponsored by Universal Audio, Inc.

This work exists because we love Universal Audio products. Apollo interfaces are
exceptional pieces of audio hardware, and we believe their owners should be able
to use them on any operating system they choose.

## Intellectual Property

Open Apollo was developed by observing and reverse engineering the behaviour of
UA hardware and drivers, for interoperability. The code in this repository is
written for this project and licensed under the GPL-2.0. It contains no
Universal Audio software, firmware, DSP code, artwork or text.

What an Apollo needs from UA's software is read on your own computer, from the
UAD install you have (a Windows partition, a Windows VM's disk or a Wine
prefix), by the setup wizard and `oa-assets`:

- the DSP modules and USB boot images go to `/lib/firmware/universal-audio/`;
- UAD Console's art, layouts, appearance data and fonts go to
  `~/.local/share/open-apollo/`;
- the plug-in panels' art is regenerated from the plug-in files and checked
  against a SHA-256 before it is used.

These copies are for your own use on your own machine; don't share them.
Universal Audio's license terms may restrict copying or extracting from its
software, and you are responsible for complying with the licenses you accepted.
None of these files are in this repository, in packages built from it or in CI
artifacts. `tools/check-clean-repo.py` refuses commits
that would add them.

The plug-in panel specs (where each control of a plug-in's editor sits) are
published separately, in open-apollo-panels, under CC0. They hold measurements
only: their art and longer texts are references that the importer resolves from
your install.

We respect Universal Audio's intellectual property. If any content in this
repository is believed to infringe on UA's rights, we welcome direct
communication and will promptly make adjustments.

**Contact:** dev@rolo.work

## UAD Console Assets (console-rs)

The native mixer console in `console-rs/` (`uaconsole`) is GPL-2.0 code written for
this project. To look like UA's UAD Console, it reads UA's images, layouts, appearance
data, and fonts **at run time from the copy the setup wizard imported from your own
installation** of UAD Console (`oa-assets import`), or from a `Resources` folder you
point it at.

The software never downloads them. They remain the property of Universal Audio and
are not covered by this project's license. Do not commit them or redistribute the
copies.

The generic mixer icon in `console-rs/packaging/` is original to this project and is
not a Universal Audio mark.

`tools/plugin-mapper/plugin_mapper/executor/win32-ctype.json` is a character-type table
generated from Wine 11.19's `sortdefault.nls` (as read by `kernelbase/locale.c`), so the
plug-in mapper's Windows emulation classifies characters as Windows does. Wine is licensed
under the GNU LGPL 2.1 or later (https://www.winehq.org); the source and its SHA-256 are
recorded in the file.

## Trademarks

Universal Audio, Apollo, UAD, UAD Console, and related names and logos are
trademarks of Universal Audio, Inc. Plug-in and product names are trademarks of
their owners. Their use here is purely descriptive — identifying the hardware this
driver supports, the software the console is compatible with, and the plug-in a
panel spec describes.
