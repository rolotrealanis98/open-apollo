// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — plug-in host: the device's authorization answer
 *
 * Copyright (c) 2026 apollo-linux contributors
 *
 * open-apollo hosts a plug-in only when the Apollo itself says the user may
 * run it.  The device's answer is its board table, which UA's own software
 * reads before it allocates anything (Windows PCIe refresh 108f8, records
 * f640, SDK CUADPlug::CreateUAD2Topology 1951b0; wine-ua-work
 * windows-license-contract/vendor-admission.md, windows-driver-wave24/
 * frontend-auth-predicate.md, windows-driver-wave12/snapshot-query-
 * producers.md).  This file only reads that answer:
 *   - the query is fresh every time, in the same operation as whatever it
 *     admits, and nothing is cached across queries, epochs or devices;
 *   - the date sent with it is the system clock's, encoded as the original
 *     encodes it; a clock before 2007 is refused, never replaced;
 *   - the table is passed through unmodified (UA_IOCTL_BOARD_QUERY) and
 *     decided on exactly as the SDK decides (ua_plugin_license_state());
 *     a failed query, a reply that is not the query's, an id outside the
 *     table or a changed epoch refuse;
 *   - nothing here starts a demo, writes a table, overrides a state or
 *     logs the table (only the counts and the decision).
 * Whether the DSP firmware itself refuses unauthorized code is unknown:
 * this gate is the only enforcement this driver can claim.
 */

#include <linux/errno.h>
#include <linux/math64.h>
#include <linux/mm.h>
#include <linux/slab.h>
#include <linux/timekeeping.h>
#include <linux/uaccess.h>

#include "ua_apollo.h"
#include "ua_plugin_host.h"

/* UA_LICENSE_BEGIN: the host tests build everything up to the end mark */

/**
 * ua_plugin_date - the query's date word for @unix_seconds
 * @unix_seconds: the system clock (UTC)
 * @date: (days since 2007-01-01 << 12) + minute of the day, mod 2^32
 *
 * PCIe 1086c over eb78: whole days since 2007-01-01 UTC shifted by 12,
 * plus (not or) the minute of the day.  A clock before 2007 has no
 * meaningful encoding: -ERANGE, the query is not sent.
 */
int ua_plugin_date(s64 unix_seconds, u32 *date)
{
	u64 elapsed, days;
	u32 secs;

	if (unix_seconds < UA_DATE_EPOCH)
		return -ERANGE;
	elapsed = (u64)(unix_seconds - UA_DATE_EPOCH);
	days = div_u64_rem(elapsed, 86400, &secs);
	*date = (u32)(days << 12) + secs / 60;
	return 0;
}

/**
 * ua_plugin_license_state - the record state of a table word
 * @word: the device's word for one plug-in
 *
 * PCIe f640: 0x81000000 and 0x82000000 are state 0 (authorized), 0 state
 * 3 (expired), 0x80000000 state 2 (demo not started), 0x83000000 state 4
 * (unavailable, also the unfilled-table sentinel), any other word state 1
 * (a demo is running; its low 31 bits are the countdown).
 */
u32 ua_plugin_license_state(u32 word)
{
	switch (word) {
	case 0x81000000:
	case 0x82000000:
		return UA_PLUGIN_LICENSE_AUTHORIZED;
	case 0x80000000:
		return UA_PLUGIN_LICENSE_NOT_STARTED;
	case 0x00000000:
		return UA_PLUGIN_LICENSE_EXPIRED;
	case 0x83000000:
		return UA_PLUGIN_LICENSE_UNAVAILABLE;
	default:
		return UA_PLUGIN_LICENSE_DEMO;
	}
}

/*
 * The SDK continues topology (allocation) for states 0 and 1 only; 2, 3,
 * 4 and anything else (the SDK logs an illegal state and exits) refuse.
 */
bool ua_plugin_license_admits(u32 state)
{
	return state == UA_PLUGIN_LICENSE_AUTHORIZED ||
	       state == UA_PLUGIN_LICENSE_DEMO;
}

/**
 * ua_plugin_board_query - the device's board table, fresh
 * @ua: device (host->mutex held, ua->lock NOT held)
 * @words: UA_BQ_WORDS words out (only written on success)
 * @date: the date word sent
 * @epoch: the epoch the answer belongs to
 *
 * One transaction on DSP 0: the reply descriptor first, then the date and
 * the query.  The reply is accepted only once the response ring retired it
 * (ua_xact_poll(): POSITION past it, then a read barrier), with the
 * header and the echo checked, and the epoch unchanged from before the
 * query to after the copy.  A query that times out is not a reason to
 * stop using DSP 0 (no quarantine): its descriptor stays the transaction's
 * until the ring passes it, and the next reply transaction clears it.
 * Returns 0, -EOPNOTSUPP, -ENODEV, -ERANGE (clock before 2007), -EBUSY,
 * -ETIMEDOUT, -EPROTO (not the query's reply), -ESTALE.
 */
int ua_plugin_board_query(struct ua_device *ua, u32 *words, u32 *date,
			  u64 *epoch)
{
	struct ua_plugin_host *h = ua->host;
	const __le32 *r;
	struct ua_xact *x;
	u32 cookie;
	u64 e;
	unsigned int i;
	int ret;

	lockdep_assert_held(&h->mutex);
	if (!ua_plugin_host_enabled())
		return -EOPNOTSUPP;
	ret = ua_plugin_date(ktime_get_real_seconds(), date);
	if (ret)
		return ret;

	x = ua_xact_alloc(0, 0);
	if (!x)
		return -ENOMEM;
	mutex_lock(&ua->lock);
	e = h->epoch;
	h->board_queries++;
	mutex_unlock(&ua->lock);
	x->epoch = e;
	x->rx_fill = UA_BQ_SENTINEL;
	/* The owned request's low 32 bits, as the original sends its own */
	cookie = lower_32_bits((uintptr_t)x);
	if (ua_xact_rx(x, UA_BQ_RX_WORDS) < 0) {
		ret = x->err;
		goto fail_unpublished;
	}
	ua_xact_inline(x, UA_BQ_CMD_DATE, *date, 0, 0);
	ua_xact_inline(x, UA_BQ_CMD_QUERY, cookie, 0, 0);
	ret = ua_xact_map(ua, x);
	if (!ret)
		ret = ua_xact_publish(ua, x);
	if (ret)
		goto fail_unpublished;
	ret = ua_xact_wait(ua, x, UA_BQ_WAIT_MS);

	mutex_lock(&ua->lock);
	if (!ret) {
		r = ua_xact_reply(x, 0);
		if (le32_to_cpu(r[0]) != UA_BQ_REPLY ||
		    le32_to_cpu(r[1]) != cookie) {
			ret = -EPROTO;
		} else {
			for (i = 0; i < UA_BQ_WORDS; i++)
				words[i] = le32_to_cpu(r[2 + i]);
			/* accepted only if nothing moved on meanwhile */
			if (h->epoch != e || atomic_read(&ua->shutdown))
				ret = -ESTALE;
		}
	}
	if (ret)
		h->board_query_failures++;
	ua_xact_done(ua, x);
	mutex_unlock(&ua->lock);
	*epoch = e;
	if (ret)
		dev_info(&ua->pdev->dev,
			 "plug-in host: board query failed (%d)\n", ret);
	return ret;

fail_unpublished:
	ua_xact_discard(ua, x);
	mutex_lock(&ua->lock);
	h->board_query_failures++;
	mutex_unlock(&ua->lock);
	return ret;
}

/**
 * ua_plugin_admit - may plug-in @render_id be placed, says the device?
 * @ua: device (host->mutex held, ua->lock NOT held)
 * @render_id: absolute plug-in id
 * @state: UA_PLUGIN_LICENSE_* out (UNKNOWN when there is no answer)
 * @word: the device's word out (0 when there is no answer)
 * @epoch: the epoch the answer belongs to; the caller places nothing in
 *         any other
 *
 * Fail closed: 0 only for an answer that admits.  -EACCES for one that
 * refuses or an id outside the table; the query's error when there is no
 * answer.
 */
int ua_plugin_admit(struct ua_device *ua, u32 render_id, u32 *state,
		    u32 *word, u64 *epoch)
{
	struct ua_plugin_host *h = ua->host;
	u32 date, *words;
	int ret;

	*state = UA_PLUGIN_LICENSE_UNKNOWN;
	*word = 0;
	*epoch = 0;
	if (render_id < UA_BQ_BASE || render_id >= UA_BQ_BASE + UA_BQ_WORDS) {
		ret = -EACCES;
		goto refused;
	}
	words = kvmalloc(UA_BQ_WORDS * sizeof(*words), GFP_KERNEL);
	if (!words)
		return -ENOMEM;
	ret = ua_plugin_board_query(ua, words, &date, epoch);
	if (!ret) {
		*word = words[render_id - UA_BQ_BASE];
		*state = ua_plugin_license_state(*word);
		if (!ua_plugin_license_admits(*state))
			ret = -EACCES;
	}
	kvfree(words);
	if (ret)
		goto refused;
	mutex_lock(&ua->lock);
	h->admitted++;
	mutex_unlock(&ua->lock);
	dev_info(&ua->pdev->dev,
		 "plug-in host: plug-in 0x%03x admitted by the device (state %u)\n",
		 render_id, *state);
	return 0;
refused:
	mutex_lock(&ua->lock);
	h->refused++;
	mutex_unlock(&ua->lock);
	dev_info(&ua->pdev->dev,
		 "plug-in host: plug-in 0x%03x refused (state %d, %d): nothing placed\n",
		 render_id, (int)*state, ret);
	return ret;
}

/* UA_LICENSE_END */

/*
 * UA_IOCTL_BOARD_QUERY: the table as the device returned it.  The struct
 * (3 KiB) lives on the heap; nothing is copied back on an error.
 */
long ua_plugin_ioctl_board_query(struct ua_device *ua, unsigned long arg)
{
	struct ua_plugin_host *h = ua->host;
	struct ua_board_query *q;
	long ret;

	if (!ua_plugin_host_enabled())
		return -EOPNOTSUPP;
	q = kvzalloc(sizeof(*q), GFP_KERNEL);
	if (!q)
		return -ENOMEM;
	if (copy_from_user(q, (void __user *)arg, offsetof(struct ua_board_query, words))) {
		ret = -EFAULT;
		goto out;
	}
	if (q->struct_size != sizeof(*q) || q->flags || q->reserved ||
	    q->date || q->epoch) {
		ret = -EINVAL;
		goto out;
	}
	if (mutex_lock_killable(&h->mutex)) {
		ret = -EINTR;
		goto out;
	}
	ret = ua_plugin_board_query(ua, q->words, &q->date, &q->epoch);
	mutex_unlock(&h->mutex);
	if (!ret && copy_to_user((void __user *)arg, q, sizeof(*q)))
		ret = -EFAULT;
out:
	kvfree(q);
	return ret;
}
