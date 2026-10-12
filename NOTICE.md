# Notice

## About This Project

This project is an independent, community-driven effort to bring Linux support
to Universal Audio Apollo Thunderbolt audio interfaces. It is not affiliated
with, endorsed by, or sponsored by Universal Audio.

This work exists because we love Universal Audio products. Apollo interfaces are
exceptional pieces of audio hardware, and we believe their owners should be able
to use them on any operating system they choose.

## Intellectual Property

All code in this repository is original work based on clean-room reverse
engineering of hardware interfaces — observing publicly accessible register
behavior, wire protocols, and device responses. No proprietary source code,
firmware, or copyrighted materials from Universal Audio are included.

We respect Universal Audio's intellectual property. If any content in this
repository is believed to infringe on UA's rights, we welcome direct
communication and will promptly make adjustments.

**Contact:** dev@rolo.work

## UAD Console Assets (console-rs)

The native mixer console in `console-rs/` (`uaconsole`) is GPL-2.0 code written for
this project. To look like UA's UAD Console, it reads UA's images, layouts, appearance
data, and fonts **at run time from the user's own installation** of UAD Console (for
example under a Wine prefix, or a copy of its `Resources` folder the user made).

None of those assets are included in this repository, in packages built from it (such
as the Nix package), or in CI artifacts, and the software never downloads them. They
remain the property of Universal Audio and are not covered by this project's license.
Do not commit them or redistribute copies made with `scripts/install-uaconsole.sh`.

The generic mixer icon in `console-rs/packaging/` is original to this project and is
not a Universal Audio mark.

## Trademarks

Universal Audio, Apollo, UAD, UAD Console, and related names and logos are
trademarks of Universal Audio, Inc. Their use here is purely descriptive —
identifying the hardware this driver supports and the software the console is
compatible with.
