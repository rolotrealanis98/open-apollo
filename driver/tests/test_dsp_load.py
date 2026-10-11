"""GET_DSP_INFO load words (ABI 1.6): the kext's GetDSPInfo figures for a
DSP with no plugins.

CDSPResourceManager::GetDSPInfo (kext 12.0.0 x86 0x21e1a) reports the
allocated load as the plugin loads plus _cpuLoadOverhead(rate) (0x22016),
whose constants CDSPResourceManager::Initialize sets per public type
(0x21c20-0x21c77).  With no plugins the allocated load is that overhead
alone; the expected values below are the kext's integer arithmetic.
ua_dsp_alloc_load() is compiled from ua_dsp.c as it is.
"""
import re
import csrc

PREFIX = r'''
#include <assert.h>
#include <stddef.h>
#include <stdint.h>
typedef uint32_t u32;
typedef uint32_t __u32;
typedef uint64_t u64;
struct ua_device { u32 public_type; };
'''

MAIN = r'''
static u32 load(u32 pt, u32 rate)
{
	struct ua_device ua = { .public_type = pt };
	return ua_dsp_alloc_load(&ua, rate);
}

int main(void)
{
	/* x4 (0x1A): 110 / 200, factor 1.0 */
	assert(load(0x1A, 44100) == 110);
	assert(load(0x1A, 48000) == 119);
	assert(load(0x1A, 88200) == 220);
	assert(load(0x1A, 96000) == 239);
	assert(load(0x1A, 176400) == 183);
	assert(load(0x1A, 192000) == 200);
	assert(load(0x1A, 0) == 0);
	/* x8p (0x16): 118 / 207, factor 1.017 */
	assert(load(0x16, 48000) == 130);	/* 128 * 0x1046a >> 16 */
	assert(load(0x16, 192000) == 210);	/* 207 * 0x1046a >> 16 */
	/* Gen 2 x16 (0x24): same set */
	assert(load(0x24, 48000) == 130);
	/* Rack (0xB): 90 / 190 */
	assert(load(0x0B, 48000) == 97);
	/* Twin X Gen 2 (0x25): the default set */
	assert(load(0x25, 48000) == 119);
	/* not a mixer device: no overhead */
	assert(load(0x04, 48000) == 0);		/* Satellite */
	assert(load(0x18, 48000) == 0);
	assert(load(0, 48000) == 0);
	assert(load(0x2a, 48000) == 0);
	assert(load(0xFFFFFFFFu, 48000) == 0);

	assert(sizeof(struct ua_dsp_info) == 32);
	assert(offsetof(struct ua_dsp_info, alloc_load) == 12);
	assert(offsetof(struct ua_dsp_info, flags) == 28);
	assert(UA_FEAT_DSP_LOAD == 1u << 16 && UA_DSP_INFO_F_LOADS == 1);
	assert(UA_ABI_MAJOR == 1 && UA_ABI_MINOR >= 6);
	return 0;
}
'''


def header():
    hdr = csrc.source('ua_ioctl.h')
    return '\n'.join([
        csrc.between(hdr, 'struct ua_dsp_info {', '#define UA_DSP_INFO_F_LOADS'),
        '\n'.join(re.findall(r'^#define UA_(?:ABI_M\w+|FEAT_DSP_LOAD|'
                             r'DSP_INFO_F_LOADS)\s.*$', hdr, re.M)),
    ]) + '\n'


def test_overhead_and_layout():
    dsp = csrc.source('ua_dsp.c')
    code = csrc.extract(dsp, 'u32 ua_dsp_alloc_load(')
    csrc.build_and_run(PREFIX + header() + code + MAIN)


def test_wired_into_get_dsp_info():
    core = csrc.source('ua_core.c')
    h = csrc.extract(core, 'static long ua_ioctl_get_dsp_info(')
    assert 'ua_dsp_alloc_load(' in h and 'ua_dsp_measured_load(' in h
    assert 'info.flags = UA_DSP_INFO_F_LOADS;' in h
    assert 'info.pgm_load = 0;' in h and 'info.mem_load = 0;' in h
    version = csrc.extract(core, 'static long ua_ioctl_get_version(')
    assert 'UA_FEAT_DSP_LOAD' in version
    dsp = csrc.source('ua_dsp.c')
    measured = csrc.extract(dsp, 'u32 ua_dsp_measured_load(')
    assert 'ua_dsp_window(dsp) + 0x0c' in measured
