/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Universal Audio Apollo — kernel version differences the plug-in host meets
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The driver builds on 5.15 and later (CI: 5.15, 6.1, 6.6, 6.8, 6.12, 7.x).
 * Only what the plug-in host files need is here.
 */
#ifndef UA_COMPAT_H
#define UA_COMPAT_H

#include <linux/version.h>

/* get_unaligned_le32(): <linux/unaligned.h> from 6.12, <asm/...> before */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 12, 0)
#include <linux/unaligned.h>
#else
#include <asm/unaligned.h>
#endif

/* bin2hex(): <linux/hex.h> once it was split out of <linux/kernel.h> */
#include <linux/kernel.h>
#if __has_include(<linux/hex.h>)
#include <linux/hex.h>
#endif

/* sha256(): <crypto/sha2.h> since 5.11 */
#include <crypto/sha2.h>

#endif /* UA_COMPAT_H */
