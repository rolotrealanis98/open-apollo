// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — plug-in host: upload, zero-fill, memspec, release
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * The commands that put a resource set on a DSP, as UAD2System builds them
 * (see ua_plugin_host.h for the words and their sources).  Every upload is
 * one transaction with one reply descriptor, waited for and checked; the
 * rest goes out without a reply and is done when the command ring has
 * passed it.  Nothing here decides placement or authorization.
 */

#include <linux/errno.h>
#include <linux/string.h>

#include "ua_apollo.h"
#include "ua_compat.h"
#include "ua_plugin_host.h"

/* UA_UPLOAD_BEGIN: the host tests build everything up to the end mark */

/* Word 0 of a resource upload: pool and length (record + two words) */
u32 ua_upload_header(const struct ua_bill_info *b, size_t len)
{
	return (b->pool ? UA_CMD_UPLOAD_DATA : UA_CMD_UPLOAD_CODE) |
	       (u32)(len / 4 + 2);
}

/**
 * ua_upload_build - one resource upload into @x
 * @x: transaction (a reply descriptor and one DMA command are added)
 * @e: the catalog entry
 * @dest: the DSP address the ledger gave
 * @form: how the record is normalized (enum ua_bill_form)
 *
 * The record is copied whole; only the copy is normalized.
 */
int ua_upload_build(struct ua_xact *x, const struct ua_cat_entry *e,
		    u32 dest, enum ua_bill_form form)
{
	unsigned int dwords = (unsigned int)(e->len / 4 + 2);
	__le32 *w;
	u8 *b;

	if (e->len % 4 || dwords > 0xffff || dest == 0xffffffff)
		return -EINVAL;
	if (ua_xact_rx(x, 4) < 0)
		return x->err;
	w = ua_xact_cmd(x, dwords);
	if (!w)
		return x->err;
	w[0] = cpu_to_le32(ua_upload_header(&e->info, e->len));
	w[1] = cpu_to_le32(dest);
	b = (u8 *)(w + 2);
	memcpy(b, e->blob, e->len);
	if (form == UA_BILL_STRIP) {
		b[7] = (u8)e->info.arch;	/* id |= arch << 24 */
		memset(b + 8, 0, 4);		/* word 2 = 0 */
	} else {
		b[10] = 0;			/* word 2 & 0xffff */
		b[11] = 0;
	}
	return 0;
}

/**
 * ua_upload_reply_check - is @r the DSP's acceptance of this upload?
 * @r: the reply (4 words, little endian)
 * @e: the entry that was uploaded
 * @form: how it was sent
 * @header: word 0 of the upload
 * @echo: set when word 3 repeats @header (seen on titan; not checked)
 *
 * Opcode 0x80070004, status 0 and the resource id, as the kext and the
 * Windows driver check it.  The id is accepted with or without the
 * architecture byte: the genuine uploads (sent without it) were answered
 * with the plain id; what the DSP answers to the strips' form (sent with
 * it) has not been seen yet.  -EPROTO otherwise.
 */
int ua_upload_reply_check(const __le32 *r, const struct ua_cat_entry *e,
			  enum ua_bill_form form, u32 header, bool *echo)
{
	u32 id = e->info.id, sent = id;

	if (form == UA_BILL_STRIP)
		sent |= e->info.arch << 24;
	if (echo)
		*echo = le32_to_cpu(r[3]) == header;
	if (le32_to_cpu(r[0]) != UA_REPLY_UPLOAD || le32_to_cpu(r[1]) != 0)
		return -EPROTO;
	if (le32_to_cpu(r[2]) != id && le32_to_cpu(r[2]) != sent)
		return -EPROTO;
	return 0;
}

/**
 * ua_memspec_compile - the private-memory link table of a chain
 * @rows: private rows of every descriptor, in descriptor order
 * @n: how many (1..UA_MEMSPEC_ROWS_MAX)
 * @desc: index of each descriptor's first row; desc[0] is 0
 * @n_desc: how many descriptors
 * @addr: the address the ledger gave each row
 * @words: 2n - 1 words out: {0x0015nnnn, (address, value)...}
 *
 * _transmitMemSpecUpdates (UAD2System 0x36bbc..0x36fa0, tools-wine
 * compile_chain_memspec.py): only the chain head's first row has no link
 * (0xffffffff); every other row stores its own address at word `offset`
 * of row `target`, where target counts from the HEAD's rows for a
 * descriptor's first row and from the descriptor's own first row after
 * that.  Offsets must fall inside the target row.  Returns 2n - 1 or
 * -EINVAL.
 */
int ua_memspec_compile(const struct ua_memspec_row *rows, unsigned int n,
		       const unsigned int *desc, unsigned int n_desc,
		       const u32 *addr, u32 *words)
{
	unsigned int c, j, m = 0, first, last, tgt_first, tgt_count, t;
	u32 link, off;

	if (!rows || !desc || !addr || !words || !n ||
	    n > UA_MEMSPEC_ROWS_MAX || !n_desc || n_desc > n || desc[0])
		return -EINVAL;
	for (j = 0; j < n; j++)
		if (rows[j].pool < 1 || rows[j].pool > 3 || !rows[j].dwords ||
		    (rows[j].flags != 0 && rows[j].flags != 2) ||
		    addr[j] == 0xffffffff || (addr[j] & 1) ||
		    (u64)addr[j] + rows[j].dwords > 0xffffffffULL)
			return -EINVAL;
	for (c = 1; c < n_desc; c++)
		if (desc[c] <= desc[c - 1] || desc[c] >= n)
			return -EINVAL;
	if (rows[0].link != 0xffffffff)
		return -EINVAL;

	words[m++] = UA_CMD_MEMSPEC | (2 * n - 1);
	for (c = 0; c < n_desc; c++) {
		first = desc[c];
		last = c + 1 < n_desc ? desc[c + 1] : n;
		for (j = first; j < last; j++) {
			link = rows[j].link;
			if (link == 0xffffffff) {
				if (j)
					return -EINVAL;
				continue;
			}
			/* the head's rows for a first row, else this descriptor's */
			tgt_first = j == first ? 0 : first;
			tgt_count = j == first ? (n_desc > 1 ? desc[1] : n) :
						 last - first;
			t = link >> 24;
			off = link & 0xffffff;
			if (t >= tgt_count || off >= rows[tgt_first + t].dwords)
				return -EINVAL;
			words[m++] = addr[tgt_first + t] + off;
			words[m++] = addr[j];
		}
	}
	return m == 2 * n - 1 ? (int)m : -EINVAL;
}

/* The release of one resource (CResourcePool::UnmapResource): no reply */
void ua_release_build(struct ua_xact *x, const struct ua_bill_info *b)
{
	if (!b->kind)
		ua_xact_inline(x, UA_CMD_RELEASE_CODE, b->id, 0, 0);
	else
		ua_xact_inline(x, UA_CMD_RELEASE_DATA, b->id,
			       b->kind == 4 ? 1 : b->kind, 0);
}

/**
 * ua_plugin_upload - put one resource on DSP @dsp and wait for its reply
 * @ua: device (host->mutex held, ua->lock NOT held)
 * @dsp: DSP the ledger placed it on
 * @e: catalog entry
 * @dest: its address there
 * @form: how the record is normalized
 * @epoch: the epoch of the decision that placed it
 *
 * On success the entry is resident on @dsp in @epoch.  A reply that is not
 * the acceptance, or none in time, leaves its state unknown and
 * quarantines the DSP (nothing more is placed there until a cold start).
 */
int ua_plugin_upload(struct ua_device *ua, unsigned int dsp,
		     struct ua_cat_entry *e, u32 dest, enum ua_bill_form form,
		     u64 epoch)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_xact *x;
	bool echo = false;
	int ret;

	x = ua_xact_alloc(dsp, PAGE_ALIGN(e->len + 8));
	if (!x)
		return -ENOMEM;
	x->epoch = epoch;
	ret = ua_upload_build(x, e, dest, form);
	if (ret) {
		ua_xact_discard(ua, x);
		return ret;
	}
	ret = ua_xact_run(ua, x, UA_XACT_WAIT_MS);
	mutex_lock(&ua->lock);
	if (x->state >= UA_XACT_PUBLISHED)
		h->dsp[dsp].uploads++;
	if (!ret) {
		ret = ua_upload_reply_check(ua_xact_reply(x, 0), e, form,
					    ua_upload_header(&e->info, e->len),
					    &echo);
		if (ret) {
			const __le32 *r = ua_xact_reply(x, 0);

			h->dsp[dsp].replies_bad++;
			dev_warn(&ua->pdev->dev,
				 "plug-in host: DSP %u refused module 0x%03x: reply %08x %08x %08x %08x\n",
				 dsp, e->info.id, le32_to_cpu(r[0]),
				 le32_to_cpu(r[1]), le32_to_cpu(r[2]),
				 le32_to_cpu(r[3]));
			ua_plugin_quarantine(ua, dsp, "an upload was not accepted",
					     ret);
		} else {
			h->dsp[dsp].replies_ok++;
		}
	}
	if (!ret)
		e->res[dsp] = (struct ua_cat_res){ .epoch = epoch,
			.state = UA_RES_RESIDENT, .base = dest };
	else if (x->state >= UA_XACT_PUBLISHED)
		e->res[dsp] = (struct ua_cat_res){ .epoch = epoch,
			.state = UA_RES_UNKNOWN, .base = dest };
	dev_dbg(&ua->pdev->dev,
		"plug-in host: module 0x%03x to DSP %u @0x%05x: %d%s\n",
		e->info.id, dsp, dest, ret, echo ? " (reply echoes the header)" : "");
	ua_xact_done(ua, x);
	mutex_unlock(&ua->lock);
	return ret;
}

/* UA_UPLOAD_END */
