"""One clock-change attempt (ua_audio_clock_change_locked()) and the
SET_DRIVER_PARAM argument checks.

The clock change runs under ua->lock, which keeps the service loop from
taking the acknowledgement: before the doorbell it clears only the
acknowledgement bits of 0xC030, so a stale one cannot satisfy the wait and
the IO-descriptor bits stay for the service loop.  Codex's version keeps a
host sequence counter shared with the service loop and blocks further
clock writes after a timeout until a reprobe; driver-v2 retries once and
keeps the last acknowledged rate (14-change ratecheck on titan), so that is
not what is tested.  Both functions are compiled as they are.  Rewritten
from the parallel Codex work (test_audio_clock_serialization,
test_clock_ioctl_width).
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
typedef uint32_t u32;
typedef uint64_t u64;
#define BIT(n) (1UL << (n))
#define UA_REG_FPGA_REV 0x2218
#define UA_REG_AX_CONNECT 0x2260
#define UA_AX_CLOCK_DOORBELL 4
#define UA_REG_NOTIF_STATUS 0xC030
#define UA_REG_NOTIF_CLOCK_CFG 0xC074
#define UA_NOTIF_BIT_OUT_IO BIT(0)
#define UA_NOTIF_BIT_IN_IO BIT(1)
#define UA_NOTIF_BIT_TRANSPORT BIT(4)
#define UA_NOTIF_BIT_EVENT BIT(5)
#define UA_NOTIF_BIT_SRATE BIT(22)
#define UA_CLOCK_TIMEOUT_MS 2000
static bool clock_ack_bit5;
static void log_(int *d, const char *fmt, ...) { (void)d; (void)fmt; }
#define dev_info log_
#define dev_warn log_
struct pci_dev { int dev; };
struct ua_audio { unsigned int clock_ack_count[3]; u32 clock_ack_last; };
struct ua_device { struct ua_audio audio; struct pci_dev *pdev; };

/* Register model: the status register and a log of writes */
static u32 status, ack_after, ack_bits, fpga = 0x8a000000;
static int polls, offline;
static char trace[512];
static void note(const char *s) { strcat(trace, s); strcat(trace, " "); }
static u32 ua_read(struct ua_device *ua, u32 off)
{
	(void)ua;
	if (offline)
		return 0xFFFFFFFF;
	if (off == UA_REG_FPGA_REV)
		return fpga;
	assert(off == UA_REG_NOTIF_STATUS);
	return status;
}
static void ua_write(struct ua_device *ua, u32 off, u32 v)
{
	(void)ua;
	if (off == UA_REG_NOTIF_CLOCK_CFG) {
		note("cfg");
	} else if (off == UA_REG_NOTIF_STATUS) {
		note("clear");
		status = v;
	} else {
		assert(off == UA_REG_AX_CONNECT && v == UA_AX_CLOCK_DOORBELL);
		note("bell");
	}
}
static void msleep(unsigned int ms)
{
	assert(ms == 10);
	if (++polls == (int)ack_after)
		status |= ack_bits;
}
'''

MAIN = r'''
static void reset(u32 st, u32 after, u32 bits)
{
	status = st; ack_after = after; ack_bits = bits;
	polls = 0; offline = 0; trace[0] = 0;
}

int main(void)
{
	struct pci_dev pci = { 0 };
	struct ua_device ua = { .pdev = &pci };

	/* A stale ack and IO bits are pending: only the ack bits go */
	reset(UA_NOTIF_BIT_EVENT | UA_NOTIF_BIT_SRATE | UA_NOTIF_BIT_OUT_IO |
	      UA_NOTIF_BIT_IN_IO, 3, UA_NOTIF_BIT_EVENT);
	assert(ua_audio_clock_change_locked(&ua, 0x40c) == 0);
	assert(!strcmp(trace, "cfg clear bell "));
	assert(status & UA_NOTIF_BIT_OUT_IO && status & UA_NOTIF_BIT_IN_IO);
	assert(polls == 3 && ua.audio.clock_ack_count[0] == 1);

	/* Nothing pending: no clear write at all */
	reset(0, 1, UA_NOTIF_BIT_SRATE);
	assert(ua_audio_clock_change_locked(&ua, 0x20c) == 0);
	assert(!strcmp(trace, "cfg bell ") && ua.audio.clock_ack_count[1] == 1);

	/* No ack within 2 s: -ETIMEDOUT, the device still answers */
	reset(0, 0, 0);
	assert(ua_audio_clock_change_locked(&ua, 0x20c) == -ETIMEDOUT);
	assert(polls == UA_CLOCK_TIMEOUT_MS / 10);

	/* Gone: -ENODEV, no doorbell */
	reset(0, 0, 0);
	offline = 1;
	assert(ua_audio_clock_change_locked(&ua, 0x20c) == -ENODEV);
	assert(!strcmp(trace, "cfg "));

	/* clock_ack_bit5: bit 22 alone is no longer an ack */
	clock_ack_bit5 = true;
	reset(0, 1, UA_NOTIF_BIT_SRATE);
	assert(ua_audio_clock_change_locked(&ua, 0x20c) == -ETIMEDOUT);
	reset(0, 2, UA_NOTIF_BIT_EVENT);
	assert(ua_audio_clock_change_locked(&ua, 0x20c) == 0);
	return 0;
}
'''


def test_clock_change_attempt():
    audio = csrc.source('ua_audio.c')
    code = '\n'.join((
        csrc.between(audio, '#define UA_CLOCK_ACK_BITS',
                     '/* The bits that count as the ack'),
        csrc.extract(audio, 'static u32 ua_clock_ack_mask('),
        csrc.extract(audio, 'static int ua_audio_clock_change_locked('),
    ))
    csrc.build_and_run(PREFIX + code + MAIN)


def test_clock_runs_under_the_device_lock():
    audio = csrc.source('ua_audio.c')
    cfg = csrc.extract(audio, 'static int ua_audio_set_clock_cfg(')
    lock = cfg.index('mutex_lock(&ua->lock);')
    change = cfg.index('ua_audio_clock_change_locked(ua, clock_config)')
    unlock = cfg.rindex('mutex_unlock(&ua->lock);')
    assert lock < change < unlock
    # The rate commits only on an acknowledged change
    commit = cfg.index('audio->sample_rate = rate;', change)
    assert cfg.index('if (!ret) {', change) < commit
    # A rate an open stream does not run at is refused first
    assert cfg.index('ua_rate_busy_locked(audio, rate)') < change


PARAM_PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdint.h>
#include <string.h>
typedef uint32_t u32;
#define __user
struct ua_driver_param { u32 param_id; u32 reserved; uint64_t value; };
struct pci_dev { int dev; };
struct ua_audio { int transport_running; };
struct ua_device { struct ua_audio audio; struct pci_dev *pdev; };
static void log_(int *d, const char *fmt, ...) { (void)d; (void)fmt; }
#define dev_warn log_
static unsigned int rates, sources, notifies, last;
static int result;
static unsigned long copy_from_user(void *to, const void *from, unsigned long n)
{ memcpy(to, from, n); return 0; }
static int ua_audio_set_clock(struct ua_device *ua, unsigned int r)
{ (void)ua; rates++; last = r; return result; }
static int ua_audio_set_clock_source(struct ua_device *ua, unsigned int s)
{ (void)ua; sources++; last = s; return result; }
static void ua_notify_clock_controls(struct ua_device *ua) { (void)ua; notifies++; }
/* the kext ids 0x101-0x105 (test_kext_driver_params.py) */
static int ua_kext_param_set(struct ua_device *ua, u32 id, uint64_t v)
{ (void)ua; (void)v; return id >= 0x101 && id <= 0x105 ? 0 : -EINVAL; }
'''

PARAM_MAIN = r'''
static long set(u32 id, uint64_t v)
{
	struct ua_driver_param dp = { .param_id = id, .value = v };
	struct pci_dev pci = { 0 };
	struct ua_device ua = { .pdev = &pci };

	return ua_ioctl_set_driver_param(&ua, (unsigned long)&dp);
}

int main(void)
{
	/* Wide values never fold into a rate or a source */
	assert(set(0, (1ULL << 32) | 48000) == -EINVAL);
	assert(set(0, 192001) == -EINVAL);
	assert(set(1, (1ULL << 32) | 0x0C) == -EINVAL);
	assert(set(1, 0x100) == -EINVAL);
	assert(set(2, 1) == -EINVAL);
	assert(set(0x101, 1) == 0);	/* a kext id, not a rate or a source */
	assert(!rates && !sources && !notifies);

	assert(set(0, 96000) == 0 && rates == 1 && last == 96000 && notifies == 1);
	assert(set(1, 0x09) == 0 && sources == 1 && last == 0x09 && notifies == 2);
	result = -ETIMEDOUT;
	assert(set(1, 0x0C) == -ETIMEDOUT && notifies == 2);
	return 0;
}
'''


def test_driver_param_widths():
    core = csrc.source('ua_core.c')
    code = csrc.extract(core, 'static long ua_ioctl_set_driver_param(')
    csrc.build_and_run(PARAM_PREFIX + code + PARAM_MAIN)
