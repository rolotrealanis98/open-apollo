// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — plug-in host: the DSP module catalog
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The modules come from the user's own UA install, written to /lib/firmware
 * at install time by oa-assets (no UA bytes ship with open-apollo).  The
 * driver reads them itself with request_firmware(); user space only ever
 * names one by its SHA-256, so there is no way to hand the driver code to
 * upload (no arbitrary uploader).  A record is accepted only when it is
 * what UAD2SDK registers and UAD2System's IsValid accepts (Mac kext 0x44da0,
 * Windows System 28c8/17070, dsp-loader README):
 *   "Bill" magic; an id of 24 bits (the on-disk form: the architecture byte
 *   is added in memory, never stored); word 2: kind <= 4, architecture <= 2,
 *   no flags for code; word 3 = the payload bytes; 1 <= w4 <= words - 13;
 *   code takes floor(2 * w4 / 3) pool words, which must be even (the
 *   constructor asserts it), the other kinds w4.
 */

#include <linux/errno.h>
#include <linux/firmware.h>
#include <linux/kernel.h>
#include <linux/mm.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "ua_apollo.h"
#include "ua_compat.h"
#include "ua_plugin_host.h"

/* UA_CATALOG_BEGIN: the host tests build everything up to the end mark */

/**
 * ua_bill_check - is @data a module record UAD2System would register?
 * @data: the record as stored (any alignment)
 * @len: its length
 * @out: what it says, filled on success
 *
 * Returns 0 or -EINVAL.
 */
int ua_bill_check(const u8 *data, size_t len, struct ua_bill_info *out)
{
	struct ua_bill_info b;
	u32 payload;

	if (!data || len < 13 * 4 + 4 || len > UA_BILL_MAX || len % 4)
		return -EINVAL;
	if (get_unaligned_le32(data) != UA_BILL_MAGIC)
		return -EINVAL;
	b.id = get_unaligned_le32(data + 4);
	b.w2 = get_unaligned_le32(data + 8);
	payload = get_unaligned_le32(data + 12);
	b.w4 = get_unaligned_le32(data + 16);
	b.kind = b.w2 & 0xff;
	b.flags = (b.w2 >> 8) & 0xffff;
	b.arch = b.w2 >> 24;
	if (b.id > 0xffffff || b.kind > 4 || b.arch > 2 ||
	    (!b.kind && b.flags) || payload != len - UA_BILL_HDR ||
	    !b.w4 || b.w4 > len / 4 - 13)
		return -EINVAL;
	if (!b.kind) {
		b.words = (u32)(((u64)b.w4 * 2) / 3);
		if (!b.words || (b.words & 1))
			return -EINVAL;
		b.pool = 0;
	} else {
		b.words = b.w4;
		b.pool = b.kind == 4 ? 1 : b.kind;
	}
	if (out)
		*out = b;
	return 0;
}

/* The entry holding exactly these bytes (ua->lock) */
static struct ua_cat_entry *ua_catalog_find(struct ua_plugin_host *h,
					    const u8 sha[32])
{
	struct ua_cat_entry *e;

	list_for_each_entry(e, &h->catalog, node)
		if (!memcmp(e->sha256, sha, 32))
			return e;
	return NULL;
}

static void ua_catalog_free(struct ua_cat_entry *e)
{
	kvfree(e->blob);
	kfree(e);
}

/*
 * A new entry from a store file's bytes: hashed, compared with the name it
 * was asked for (@want, NULL for a strip module asked for by id), checked.
 */
static int ua_catalog_build(const u8 *data, size_t len, const u8 *want,
			    struct ua_cat_entry **out)
{
	struct ua_cat_entry *e;
	int ret;

	*out = NULL;
	e = kzalloc(sizeof(*e), GFP_KERNEL);
	if (!e)
		return -ENOMEM;
	ret = ua_bill_check(data, len, &e->info);
	if (ret)
		goto fail;
	sha256(data, len, e->sha256);
	if (want && memcmp(e->sha256, want, 32)) {
		ret = -EBADMSG;
		goto fail;
	}
	e->blob = kvmalloc(len, GFP_KERNEL);
	if (!e->blob) {
		ret = -ENOMEM;
		goto fail;
	}
	memcpy(e->blob, data, len);
	e->len = len;
	INIT_LIST_HEAD(&e->node);
	*out = e;
	return 0;
fail:
	kfree(e);
	return ret;
}

/* Put @n in the catalog, or take the entry that already holds its bytes */
static int ua_catalog_insert(struct ua_device *ua, struct ua_cat_entry *n,
			     struct ua_cat_entry **out)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_cat_entry *e;
	size_t bytes = 0;
	int ret = 0;

	mutex_lock(&ua->lock);
	e = ua_catalog_find(h, n->sha256);
	if (e) {
		ua_catalog_free(n);
	} else {
		list_for_each_entry(e, &h->catalog, node)
			bytes += e->len;
		if (h->catalog_count >= UA_CATALOG_MAX ||
		    bytes + n->len > UA_CATALOG_BYTES) {
			ua_catalog_free(n);
			e = NULL;
			ret = -ENOSPC;
			goto out;
		}
		list_add_tail(&n->node, &h->catalog);
		h->catalog_count++;
		e = n;
	}
	e->users++;
out:
	mutex_unlock(&ua->lock);
	*out = e;
	return ret;
}

/* Read one store file; -ENOENT when it is not there */
static int ua_catalog_read(struct ua_device *ua, const char *name,
			   const u8 *want, struct ua_cat_entry **out)
{
	const struct firmware *fw;
	int ret;

	ret = firmware_request_nowarn(&fw, name, &ua->pdev->dev);
	if (ret)
		return ret == -ENOENT ? -ENOENT : ret;
	ret = ua_catalog_build(fw->data, fw->size, want, out);
	release_firmware(fw);
	if (ret)
		dev_warn(&ua->pdev->dev,
			 "plug-in host: %s refused (%d): %s\n", name, ret,
			 ret == -EBADMSG ? "its SHA-256 is not its name" :
			 "not a module record UAD2System would register");
	return ret;
}

/**
 * ua_catalog_get - a plug-in module by SHA-256
 * @ua: device (no lock held: reads a file)
 * @sha256: the module's hash
 * @out: the entry, with a user reference taken
 *
 * Returns 0, -ENOENT (not in the module store), -EBADMSG (a store file
 * whose content is not its name), -EINVAL (not a valid record), -ENOSPC
 * (the catalog is full), -ENOMEM.
 */
int ua_catalog_get(struct ua_device *ua, const u8 sha256[32],
		   struct ua_cat_entry **out)
{
	struct ua_cat_entry *n;
	char name[128], hex[65];
	int ret;

	*out = NULL;
	mutex_lock(&ua->lock);
	n = ua_catalog_find(ua->host, sha256);
	if (n)
		n->users++;
	mutex_unlock(&ua->lock);
	if (n) {
		*out = n;
		return 0;
	}
	*bin2hex(hex, sha256, 32) = '\0';
	snprintf(name, sizeof(name), "universal-audio/plugins/modules/%s.bill",
		 hex);
	ret = ua_catalog_read(ua, name, sha256, &n);
	if (ret)
		return ret;
	return ua_catalog_insert(ua, n, out);
}

/**
 * ua_catalog_get_system - a Console strip module by id
 * @ua: device (no lock held)
 * @id: module id (the strips decide which ids they ask for)
 * @out: the entry, with a user reference taken
 *
 * universal-audio/dsp/<id>.bill (oa-assets firmware), else the older
 * ua_apollo/dsp/<id>.bill (tools/extract-ua-dsp-modules.py).  The record
 * must carry @id.
 */
int ua_catalog_get_system(struct ua_device *ua, u32 id,
			  struct ua_cat_entry **out)
{
	static const char * const dirs[] = { "universal-audio/dsp",
					      "ua_apollo/dsp" };
	struct ua_cat_entry *n = NULL;
	char name[64];
	unsigned int i;
	int ret = -ENOENT;

	*out = NULL;
	if (id > 0xfff)
		return -EINVAL;
	for (i = 0; i < ARRAY_SIZE(dirs) && ret == -ENOENT; i++) {
		snprintf(name, sizeof(name), "%s/%03x.bill", dirs[i], id);
		ret = ua_catalog_read(ua, name, NULL, &n);
	}
	if (ret)
		return ret;
	if (n->info.id != id) {
		ua_catalog_free(n);
		return -EINVAL;
	}
	return ua_catalog_insert(ua, n, out);
}

/* Is @e uploaded on @dsp in @epoch (ua->lock)? */
bool ua_catalog_resident(const struct ua_cat_entry *e, unsigned int dsp,
			 u64 epoch)
{
	return dsp < UA_HOST_DSPS && e->res[dsp].epoch == epoch &&
	       e->res[dsp].state == UA_RES_RESIDENT;
}

/**
 * ua_catalog_put - drop a user reference
 * @ua: device (ua->lock held)
 * @e: entry (NULL is fine)
 *
 * The last user frees the host copy.  Where it was resident is the
 * caller's business before this (the release on the DSP is part of the
 * instance's or the strips' teardown).
 */
void ua_catalog_put(struct ua_device *ua, struct ua_cat_entry *e)
{
	struct ua_plugin_host *h = ua->host;

	lockdep_assert_held(&ua->lock);
	if (!e || WARN_ON_ONCE(!e->users))
		return;
	if (--e->users)
		return;
	list_del(&e->node);
	h->catalog_count--;
	ua_catalog_free(e);
}

/* Host teardown: nothing references the entries any more */
void ua_catalog_free_all(struct ua_device *ua)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_cat_entry *e, *tmp;

	list_for_each_entry_safe(e, tmp, &h->catalog, node) {
		list_del(&e->node);
		ua_catalog_free(e);
	}
	h->catalog_count = 0;
}

/* UA_CATALOG_END */
