"""The kext's driver parameters 1-5 as UA_DRIVER_PARAM_KEXT(n) aliases.

CDeviceManager::SetDriverParameter / GetDriverParameter (kext 12.0.0 x86
0x1dd38 / 0x1de68) only store them: 1 at mgr+0x5100 (initial 0), 2 through
SetDiagnosticFlags (bit 0 clears 3 and is dropped), 3 is cleared by any
Set, 4 is a u64, 5 at mgr+0x5104 (initial 1); Get sign-extends 1 and 5.
The Linux ids 0-2 keep their meaning.  Helpers compiled from ua_core.c.
"""
import re
import csrc

PREFIX = r'''
#include <assert.h>
#include <errno.h>
#include <stdint.h>
typedef uint32_t u32;
typedef uint32_t __u32;
typedef uint64_t u64;
typedef uint64_t __u64;
typedef int32_t s32;
typedef int64_t s64;
#define BIT(n) (1UL << (n))
static int locked;
static void mutex_lock(int *l) { assert(!*l); *l = 1; locked = 1; }
static void mutex_unlock(int *l) { assert(*l); *l = 0; locked = 0; }
struct ua_device { int lock; u64 kext_param[6]; u32 kext_param_set; };
'''

MAIN = r'''
static u64 get(struct ua_device *ua, u32 id)
{
	u64 v = 0xdeadbeef;

	assert(ua_kext_param_get(ua, id, &v) == 0);
	return v;
}

int main(void)
{
	struct ua_device ua = { 0 };
	u64 v;

	assert(UA_DRIVER_PARAM_KEXT(1) == 0x101 && UA_DRIVER_PARAM_KEXT(5) == 0x105);
	assert(UA_DRIVER_PARAM_RATE == 0 && UA_DRIVER_PARAM_CLOCK_SRC == 1 &&
	       UA_DRIVER_PARAM_TRANSPORT == 2);
	assert(UA_FEAT_KEXT_DRIVER_PARAMS == 1u << 15);
	assert(UA_ABI_MAJOR == 1 && UA_ABI_MINOR >= 5);

	/* initial values */
	assert(get(&ua, 0x101) == 0 && get(&ua, 0x102) == 0);
	assert(get(&ua, 0x103) == 0 && get(&ua, 0x104) == 0);
	assert(get(&ua, 0x105) == 1);

	/* out of range, including the Linux ids and the kext's raw ids */
	for (u32 id = 0; id < 0x110; id++) {
		if (id >= 0x101 && id <= 0x105)
			continue;
		assert(ua_kext_param_set(&ua, id, 1) == -EINVAL);
		assert(ua_kext_param_get(&ua, id, &v) == -EINVAL);
	}

	/* 1: 32-bit, sign-extended on Get */
	assert(ua_kext_param_set(&ua, 0x101, 1) == 0 && get(&ua, 0x101) == 1);
	assert(ua_kext_param_set(&ua, 0x101, 0x1ffffffffULL) == 0);
	assert(get(&ua, 0x101) == 0xffffffffffffffffULL);

	/* 2: bit 0 is dropped and clears 3 */
	assert(ua_kext_param_set(&ua, 0x102, 0x7) == 0);
	assert(get(&ua, 0x102) == 0x6 && get(&ua, 0x103) == 0);

	/* 3: any Set clears */
	assert(ua_kext_param_set(&ua, 0x103, 42) == 0 && get(&ua, 0x103) == 0);

	/* 4: full 64 bits */
	assert(ua_kext_param_set(&ua, 0x104, 0x123456789abcULL) == 0);
	assert(get(&ua, 0x104) == 0x123456789abcULL);

	/* 5: the Mixer Engine writes 1; 0 sticks */
	assert(ua_kext_param_set(&ua, 0x105, 0) == 0 && get(&ua, 0x105) == 0);
	assert(ua_kext_param_set(&ua, 0x105, 1) == 0 && get(&ua, 0x105) == 1);
	assert(!locked);
	return 0;
}
'''


def header():
    hdr = csrc.source('ua_ioctl.h')
    return '\n'.join(re.findall(
        r'^#define UA_(?:ABI_M\w+|FEAT_KEXT_DRIVER_PARAMS|'
        r'DRIVER_PARAM_\w+(?:\(n\))?)\s.*$', hdr, re.M)) + '\n'


def test_aliases():
    core = csrc.source('ua_core.c')
    code = (csrc.extract(core, 'static int ua_kext_param_set(') + '\n' +
            csrc.extract(core, 'static int ua_kext_param_get('))
    csrc.build_and_run(PREFIX + header() + code + MAIN)


def test_wired_and_linux_ids_kept():
    core = csrc.source('ua_core.c')
    set_ = csrc.extract(core, 'static long ua_ioctl_set_driver_param(')
    get_ = csrc.extract(core, 'static long ua_ioctl_get_driver_param(')
    assert 'case 0:' in set_ and 'case 1:' in set_
    assert 'ua_kext_param_set(ua, dp.param_id, dp.value)' in set_
    assert 'case 0:' in get_ and 'case 1:' in get_ and 'case 2:' in get_
    assert 'ua_kext_param_get(ua, dp.param_id, &dp.value)' in get_
    version = csrc.extract(core, 'static long ua_ioctl_get_version(')
    assert 'UA_FEAT_KEXT_DRIVER_PARAMS' in version


def test_inherited_dsp_keeps_its_input_strips():
    """A mixer DSP found running at probe holds another session's state,
    Console's input strips among it; the per-DSP ring connect makes the
    DSPs drop them (titan 2026-10-11: a mic recorded only with it off), so
    ua_kext_ring_connect() returns before connecting on such a device."""
    core = csrc.source('ua_core.c')
    fn = csrc.extract(core, 'void ua_kext_ring_connect(')
    assert fn.index('ua->dsp_inherited') < fn.index('ua_dsp_connect_retry(')
    init = csrc.extract(core, 'static int ua_dsp_init_and_load(')
    alive = init.index('mixer DSP alive!')
    window = init[alive:alive + 600]
    # Only for a model whose strips this driver knows: on the Twin X the
    # skipped ring connect kept nothing and left playback silent (#91)
    assert 'ua->dsp_inherited =' in window
    assert 'ua_strips_model_known(ua->device_type)' in window
    assert 'ua->dsp_inherited = true' not in window
    # A cold re-init (resume after the Apollo lost power) has nothing to
    # keep: the fresh DSPs get the ring connect again
    cold = init.index('ret = ua_program_registers(ua);')
    assert 'ua->dsp_inherited = false' in init[cold:cold + 600]
