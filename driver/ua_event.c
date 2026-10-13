// SPDX-License-Identifier: GPL-2.0-only
/*
 * Universal Audio Apollo — event channel to user space
 *
 * read()/poll()/fasync on /dev/ua_apolloN deliver struct ua_event records
 * (see ua_ioctl.h for the codes and their meaning).  Every open file has
 * its own queue, so a reader never takes events from another one and a
 * file that does not read costs a few hundred bytes and nothing else.
 *
 * Producers (ua_event_post) may run in any context, interrupts included:
 * event_lock is taken with interrupts off, held only for the kfifo
 * operations and never nested with another lock.
 *
 * Lifetime: filp->private_data is a struct ua_file.  It points at the
 * ua_device, which the cdev keeps alive for as long as the file is open
 * (see ua_chardev_release), so the pointer needs no extra reference.
 */

#include <linux/slab.h>
#include <linux/poll.h>
#include <linux/ktime.h>
#include <linux/uaccess.h>

#include "ua_apollo.h"
#include "ua_plugin_host.h"
#include "ua_ioctl.h"

#define UA_EVENT_READ_BATCH	8

void ua_event_init(struct ua_device *ua)
{
	spin_lock_init(&ua->event_lock);
	INIT_LIST_HEAD(&ua->event_files);
	init_waitqueue_head(&ua->event_wq);
}

/* Queue one event on @uf; caller holds ua->event_lock */
static void ua_event_queue_locked(struct ua_file *uf,
				  const struct ua_event *ev)
{
	/* Full: the oldest event is the one to lose */
	if (kfifo_is_full(&uf->events)) {
		kfifo_skip(&uf->events);
		uf->lost++;
	}
	kfifo_put(&uf->events, *ev);
	kill_fasync(&uf->fasync, SIGIO, POLL_IN);
}

/*
 * Post an event to every open file.  Any context; does not sleep.
 */
void ua_event_post(struct ua_device *ua, u32 code, u32 arg)
{
	struct ua_event ev = {
		.code = code,
		.arg = arg,
		.time_ns = ktime_get_ns(),
	};
	struct ua_file *uf;
	unsigned long flags;

	spin_lock_irqsave(&ua->event_lock, flags);
	list_for_each_entry(uf, &ua->event_files, node)
		ua_event_queue_locked(uf, &ev);
	spin_unlock_irqrestore(&ua->event_lock, flags);

	wake_up_interruptible(&ua->event_wq);
}

/*
 * Count one error of @class (UA_ERR_*) and post UA_EVT_DEVICE_ERROR for it,
 * at most once per class per second.  Any context; does not sleep.
 */
void ua_err_count(struct ua_device *ua, unsigned int class)
{
	u64 now, last;

	if (class == 0 || class >= UA_ERR_NR)
		return;

	atomic_long_inc(&ua->err.count[class]);

	now = ktime_get_ns();
	last = atomic64_read(&ua->err.last_evt_ns[class]);
	/* 0 = never posted */
	if (last && now - last < NSEC_PER_SEC)
		return;
	if (atomic64_cmpxchg(&ua->err.last_evt_ns[class], last, now) == last)
		ua_event_post(ua, UA_EVT_DEVICE_ERROR, class);
}

/*
 * The notification status the driver just read and cleared: the raw value,
 * then the decoded events as the kext's _handleNotificationInterrupt posts
 * them.  Caller holds ua->lock (audio->rate_index is stable).
 */
void ua_event_post_notif(struct ua_device *ua, u32 notif)
{
	if (!notif || notif == 0xFFFFFFFF)
		return;

	ua_event_post(ua, UA_EVT_NOTIF_STATUS, notif);
	if (notif & UA_NOTIF_BIT_OUT_IO)
		ua_event_post(ua, UA_EVT_IODESC_OUT, notif);
	if (notif & UA_NOTIF_BIT_IN_IO)
		ua_event_post(ua, UA_EVT_IODESC_IN, notif);
	if (notif & (UA_NOTIF_BIT_TRANSPORT | UA_NOTIF_BIT_SRATE)) {
		ua_event_post(ua, UA_EVT_RATE_INFO, notif);
		ua_event_post(ua, UA_EVT_RATE_CHANGED, ua->audio.rate_index);
	}
}

/*
 * The device is going away: last event for every file, then read() and
 * poll() report it.  Called from ua_remove() once ioctls are refused.
 */
void ua_event_offline(struct ua_device *ua, bool surprise)
{
	unsigned long flags;

	ua_event_post(ua, UA_EVT_DEVICE_OFFLINE, surprise ? 1 : 0);

	spin_lock_irqsave(&ua->event_lock, flags);
	ua->event_offline = true;
	spin_unlock_irqrestore(&ua->event_lock, flags);
	wake_up_interruptible_all(&ua->event_wq);
}

int ua_event_open(struct inode *inode, struct file *filp)
{
	struct ua_device *ua = container_of(inode->i_cdev, struct ua_device,
					    cdev);
	struct ua_file *uf;
	unsigned long flags;

	uf = kzalloc(sizeof(*uf), GFP_KERNEL);
	if (!uf)
		return -ENOMEM;
	uf->ua = ua;
	INIT_KFIFO(uf->events);

	spin_lock_irqsave(&ua->event_lock, flags);
	list_add_tail(&uf->node, &ua->event_files);
	spin_unlock_irqrestore(&ua->event_lock, flags);

	filp->private_data = uf;
	return nonseekable_open(inode, filp);
}

int ua_event_release(struct inode *inode, struct file *filp)
{
	struct ua_file *uf = filp->private_data;
	struct ua_device *ua = uf->ua;
	unsigned long flags;

	/* the plug-in instances this file created come down with it */
	ua_plugin_file_release(uf);
	fasync_helper(-1, filp, 0, &uf->fasync);

	spin_lock_irqsave(&ua->event_lock, flags);
	list_del(&uf->node);
	spin_unlock_irqrestore(&ua->event_lock, flags);

	kfree(uf);
	return 0;
}

/* Pop up to @max events (a LOST record first, if any); returns the count */
static unsigned int ua_event_pop(struct ua_file *uf, struct ua_event *out,
				 unsigned int max, bool *offline)
{
	struct ua_device *ua = uf->ua;
	unsigned long flags;
	unsigned int n = 0;

	spin_lock_irqsave(&ua->event_lock, flags);
	if (uf->lost && !kfifo_is_empty(&uf->events)) {
		out[n].code = UA_EVT_EVENTS_LOST;
		out[n].arg = uf->lost;
		out[n].time_ns = ktime_get_ns();
		uf->lost = 0;
		n++;
	}
	while (n < max && kfifo_get(&uf->events, &out[n]))
		n++;
	*offline = ua->event_offline;
	spin_unlock_irqrestore(&ua->event_lock, flags);
	return n;
}

ssize_t ua_event_read(struct file *filp, char __user *buf, size_t len,
		      loff_t *ppos)
{
	struct ua_file *uf = filp->private_data;
	struct ua_device *ua = uf->ua;
	struct ua_event ev[UA_EVENT_READ_BATCH];
	size_t done = 0;

	if (len < sizeof(struct ua_event))
		return -EINVAL;

	while (done + sizeof(ev[0]) <= len) {
		unsigned int max = min_t(size_t, UA_EVENT_READ_BATCH,
					 (len - done) / sizeof(ev[0]));
		bool offline;
		unsigned int n = ua_event_pop(uf, ev, max, &offline);

		if (!n) {
			int ret;

			if (done)
				break;
			if (offline)
				return -ENODEV;
			if (filp->f_flags & O_NONBLOCK)
				return -EAGAIN;
			ret = wait_event_interruptible(ua->event_wq,
				!kfifo_is_empty(&uf->events) ||
				READ_ONCE(ua->event_offline));
			if (ret)
				return ret;
			continue;
		}

		if (copy_to_user(buf + done, ev, n * sizeof(ev[0])))
			return done ? done : -EFAULT;
		done += n * sizeof(ev[0]);
	}
	return done;
}

__poll_t ua_event_poll(struct file *filp, struct poll_table_struct *wait)
{
	struct ua_file *uf = filp->private_data;
	struct ua_device *ua = uf->ua;
	__poll_t mask = 0;
	unsigned long flags;

	poll_wait(filp, &ua->event_wq, wait);

	spin_lock_irqsave(&ua->event_lock, flags);
	if (!kfifo_is_empty(&uf->events))
		mask |= EPOLLIN | EPOLLRDNORM;
	else if (ua->event_offline)
		mask |= EPOLLHUP;
	spin_unlock_irqrestore(&ua->event_lock, flags);
	return mask;
}

int ua_event_fasync(int fd, struct file *filp, int on)
{
	struct ua_file *uf = filp->private_data;

	return fasync_helper(fd, filp, on, &uf->fasync);
}
