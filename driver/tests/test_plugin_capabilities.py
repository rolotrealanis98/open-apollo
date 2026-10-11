"""UA_IOCTL_GET_PLUGIN_CAPS: a read-only report that says no plugin
operation exists.

The struct and ioctl number come from ua_ioctl.h and the handler from
ua_core.c, compiled as they are.  Checks the fixed 64-byte layout, that the
number is not shared with another ioctl (Codex's version used 0x04, which is
GET_VERSION here), the feature bit and ABI minor, operations == 0 and the
state bits.  Adapted from the parallel Codex work (test_plugin_capabilities).
"""
import re
import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
/* The Linux asm-generic ioctl encoding, whatever the host's is */
#define _IOC_READ 2U
#define _IOC(d, t, n, s) (((d) << 30) | ((t) << 8) | (n) | ((s) << 16))
#define _IOR(t, n, type) _IOC(_IOC_READ, (t), (n), (unsigned)sizeof(type))
#define _IOC_NR(c) ((c) & 0xff)
#define _IOC_DIR(c) (((c) >> 30) & 3)
#define _IOC_SIZE(c) (((c) >> 16) & 0x3fff)
typedef uint32_t __u32;
typedef uint64_t __u64;
typedef uint16_t __u16;
#define UA_IOCTL_MAGIC 'U'
#define UA_MAX_DSPS 8
#define __user
#define BIT(n) (1UL << (n))
#define BUILD_BUG_ON(c) _Static_assert(!(c), "size")
static int locked, copies, fail_copy;
static void mutex_lock(int *l) { assert(!*l); *l = 1; locked = 1; }
static void mutex_unlock(int *l) { assert(*l); *l = 0; locked = 0; }
static unsigned long copy_to_user(void *to, const void *from, size_t n)
{ assert(!locked); copies++; if (fail_copy) return n; memcpy(to, from, n); return 0; }
struct ua_dsp_state { bool rings_allocated; };
struct ua_audio { bool connected; };
struct ua_device { int lock; bool fw_loaded, dsps_connected, plugins_activated;
		   struct ua_audio audio; unsigned int num_dsps;
		   struct ua_dsp_state dsp[UA_MAX_DSPS];
		   unsigned int dma_orphan_count; };
'''

MAIN = r'''
int main(void)
{
	struct ua_device ua = { .num_dsps = 4 };
	struct ua_plugin_caps c;

	assert(sizeof(c) == 64);
	assert(offsetof(struct ua_plugin_caps, operations) == 8);
	assert(offsetof(struct ua_plugin_caps, reserved) == 28);
	assert(_IOC_NR(UA_IOCTL_GET_PLUGIN_CAPS) == 7);
	assert(_IOC_DIR(UA_IOCTL_GET_PLUGIN_CAPS) == _IOC_READ);
	assert(_IOC_SIZE(UA_IOCTL_GET_PLUGIN_CAPS) == 64);
	assert(UA_ABI_MAJOR == 1 && UA_ABI_MINOR >= 3);
	assert(UA_FEAT_PLUGIN_CAPS == 1u << 13);

	/* Nothing set: no operations, no state */
	memset(&c, 0xff, sizeof(c));
	assert(ua_ioctl_get_plugin_caps(&ua, (unsigned long)&c) == 0);
	assert(c.struct_size == 64 && c.operations == 0 && c.state == 0);
	assert(c.num_dsps == 4 && c.dsp_rings == 0 && c.dma_orphans == 0);
	for (unsigned int i = 0; i < 9; i++)
		assert(c.reserved[i] == 0);

	/* Observations only; operations stays 0 whatever the state */
	ua.fw_loaded = ua.dsps_connected = ua.plugins_activated = true;
	ua.audio.connected = true;
	ua.dsp[0].rings_allocated = ua.dsp[3].rings_allocated = true;
	ua.dma_orphan_count = 2;
	assert(ua_ioctl_get_plugin_caps(&ua, (unsigned long)&c) == 0);
	assert(c.operations == 0);
	assert(c.state == (UA_PLUGIN_STATE_FW_LOADED |
			   UA_PLUGIN_STATE_DSPS_CONNECTED |
			   UA_PLUGIN_STATE_INIT_CHAIN |
			   UA_PLUGIN_STATE_AUDIO_CONNECTED));
	assert(c.dsp_rings == 0x9 && c.dma_orphans == 2);

	/* A num_dsps past the array never reads past it */
	ua.num_dsps = 100;
	assert(ua_ioctl_get_plugin_caps(&ua, (unsigned long)&c) == 0);
	assert(c.num_dsps == 100 && c.dsp_rings == 0x9);

	fail_copy = 1;
	assert(ua_ioctl_get_plugin_caps(&ua, (unsigned long)&c) == -EFAULT);
	assert(!locked);
	return 0;
}
'''


def header_abi():
    hdr = csrc.source('ua_ioctl.h')
    parts = [
        '\n'.join(re.findall(r'^#define UA_(?:ABI_M\w+|FEAT_PLUGIN_CAPS|'
                             r'PLUGIN_STATE_\w+)\s.*$', hdr, re.M)),
        csrc.between(hdr, 'struct ua_plugin_caps {', '#define UA_IOCTL_GET_PLUGIN_CAPS'),
        re.search(r'^#define UA_IOCTL_GET_PLUGIN_CAPS.*$', hdr, re.M).group(0),
    ]
    return '\n'.join(parts) + '\n'


def test_layout_number_and_handler():
    core = csrc.source('ua_core.c')
    handler = csrc.extract(core, 'static long ua_ioctl_get_plugin_caps(')
    csrc.build_and_run(PREFIX + header_abi() + handler + MAIN)


def test_number_is_unique_and_wired():
    hdr = csrc.source('ua_ioctl.h')
    numbers = re.findall(r'_IO[RW]*\(UA_IOCTL_MAGIC,\s*(0x[0-9a-fA-F]+)', hdr)
    assert numbers.count('0x07') == 1
    assert len(numbers) == len(set(n.lower() for n in numbers))
    core = csrc.source('ua_core.c')
    dispatch = csrc.extract(core, 'static long ua_ioctl_dispatch(')
    assert 'case UA_IOCTL_GET_PLUGIN_CAPS:' in dispatch
    version = csrc.extract(core, 'static long ua_ioctl_get_version(')
    assert 'UA_FEAT_PLUGIN_CAPS' in version
    # Read-only: open to whoever may open the node, like GET_SYSTEM_INFO
    rawio = csrc.extract(core, 'static bool ua_ioctl_needs_rawio(')
    assert 'GET_PLUGIN_CAPS' not in rawio
    # No operation bit exists to advertise
    assert 'UA_PLUGIN_OP_' not in re.sub(r'/\*.*?\*/', '', hdr, flags=re.S)
