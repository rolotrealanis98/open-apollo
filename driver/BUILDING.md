# Building and compile-checking the driver

Normal build and install: `make` in this directory (or `scripts/install.sh`).
The module builds out of tree against installed kernel headers:

```bash
make KDIR=/usr/src/linux-headers-$(uname -r)
```

## Compile-check matrix (no hardware)

Last verified clean (no errors) with `W=1`:

| Kernel | Compiler |
|--------|----------|
| 5.15.0-198 (Ubuntu 22.04) | gcc 11.4 |
| 6.1.0-53 (Debian 12) | gcc 12.2 |
| 6.6.158 (Ubuntu mainline) | gcc 15.2 |
| 6.8.0-146 (Ubuntu 24.04) | gcc 13.3 |
| 6.12.112 (Ubuntu mainline) | gcc 15.2 |
| 7.0.0-38 (Ubuntu 24.04 HWE) | gcc 13.3 |
| 7.2.6 (Ubuntu mainline) | gcc 15.2 |
| 7.3.0-rc3 (Ubuntu mainline) | gcc 15.2 |
| 6.12.88 (kernel.org, `LLVM=1` defconfig) | clang 18.1 |

Notes:

- Distro and mainline-PPA headers were configured with the distro's gcc. Use
  the same compiler major (`gcc-15` for the 6.12/7.2/7.3 mainline trees, for
  example) or the flags in the kernel's `.config` (`-fmin-function-alignment`
  and friends) are rejected.
- Distro headers cannot be used with `LLVM=1`: the saved flags are gcc-only.
  Build clang against a source tree configured with clang instead.
- gcc 11/12 need the `-O2` override on `get_io_map`; the `UA_GCC_OPTIMIZE()`
  macro at the top of `ua_apollo.h` provides it and expands to nothing under
  clang. Do not use `__attribute__((optimize))` directly.

## Commands

GCC with warnings:

```bash
make KDIR=/usr/src/linux-headers-<ver> W=1
```

Clang (needs `clang lld llvm` and a clang-configured kernel tree):

```bash
cd linux-6.12.x
make LLVM=1 defconfig
scripts/config -e SOUND -e SND -e SND_PCM -e PCI -e FW_LOADER
make LLVM=1 olddefconfig && make LLVM=1 -j"$(nproc)" modules_prepare
make -C /path/to/open-apollo/driver LLVM=1 KDIR=$PWD W=1
```

modpost reports undefined symbols there because no full kernel was built;
the object files compiling is the check.

Sparse (needs `sparse`; works with gcc-configured distro headers):

```bash
make KDIR=/usr/src/linux-headers-<ver> C=2 CHECK=sparse CF=-D__CHECK_ENDIAN__
```

Ring-entry buffers are `__le32 *`; keep `cpu_to_le32()`/`le32_to_cpu()` at
the access sites so this stays warning-free.

## Containers

The matrix runs in Proxmox LXCs via `apollo-linux/tools/oa-build.sh`
(`oa-build.sh <repo-or-worktree> <label>`; `OA_MAKE_ARGS="..."` passes extra
make arguments, `OA_CTS="106"` limits the containers). It builds against every
`/usr/src/linux-headers-*-generic` and `*-amd64` tree.

- CT 106, Ubuntu 24.04: 6.8, 7.0 HWE, plus mainline 6.6.158, 6.12.112, 7.2.6
  and 7.3-rc3 from `kernel.ubuntu.com/mainline` (headers `.deb`s installed
  with `dpkg -i --force-depends`), gcc 13 and gcc-15 (ppa:ubuntu-toolchain-r/test),
  clang 18, sparse.
- CT 113, Debian 12: 6.1.
- CT 114, Ubuntu 22.04: 5.15.
