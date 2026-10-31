"""GET_SYSTEM_INFO carries the firmware version word (BAR0 0x0008) and the
device info word (BAR0 0x01AC) as the macOS driver reports them (ABI 1.4).

The version word is a release, not a model: 0x0b0fc9b3 (11.15.51635) is
shared by the x4 and the Twin X built from the same drop.  The per-model
words are dev_info (x4: 0x0322007a) and the FirmwareUpdate*.bin header's
board_info selector (x4: 0x1f), which UA's Meter app compares with the
unit before flashing (mixer-engine/firmware_header.py).

CPcieDevice::GetDeviceInfo (kext 12.0.0 x86 0x4f44) returns BAR0 0x08 as
UAD2DeviceInfo+0x24, the word UA's Mixer Engine prints "%d.%02d.%03d" and
whose major it checks, and BAR0 0x1AC as +0x80 only for the public types in
0x3ff16fcf9a0 (0xFFFFFFFF for the rest).  The struct, macros and the gating
helper are compiled from the driver sources as they are.
"""
import re
import csrc

PREFIX = r'''
#include <assert.h>
#include <stddef.h>
#include <stdint.h>
typedef uint32_t __u32;
typedef uint32_t u32;
typedef uint64_t __u64;
static u32 regs[0x200];
static unsigned int reads;
struct ua_device { u32 public_type; };
static u32 ua_read(struct ua_device *ua, u32 off)
{ (void)ua; reads++; return regs[off / 4]; }
'''

MAIN = r'''
int main(void)
{
	struct ua_device ua = { .public_type = 0x1A };

	assert(sizeof(struct ua_system_info) == 148);
	assert(offsetof(struct ua_system_info, fw_rate_mask) == 128);
	assert(offsetof(struct ua_system_info, fw_release) == 132);
	assert(offsetof(struct ua_system_info, dev_info) == 136);
	assert(UA_ABI_MAJOR == 1 && UA_ABI_MINOR >= 4);
	assert(UA_FEAT_FW_RELEASE == 1u << 14);

	/* titan's x4: 11.15.51635 (a release, shared with the Twin X) */
	assert(UA_FW_MAJOR(0x0b0fc9b3u) == 11);
	assert(UA_FW_MINOR(0x0b0fc9b3u) == 15);
	assert(UA_FW_BUILD(0x0b0fc9b3u) == 51635);
	assert(UA_REG_FW_RELEASE == 0x0008 && UA_REG_DEV_INFO == 0x01AC);
	assert(UA_REG_FW_BOOT == 0x0004);	/* FirmwareUpdate +0x14 */

	regs[UA_REG_DEV_INFO / 4] = 0x0322007a;
	assert(ua_read_dev_info(&ua) == 0x0322007a);	/* x4: in the mask */
	ua.public_type = 0x14;				/* x6 */
	assert(ua_read_dev_info(&ua) == 0x0322007a);
	ua.public_type = 0x04;				/* Satellite: not read */
	reads = 0;
	assert(ua_read_dev_info(&ua) == 0xFFFFFFFFu && reads == 0);
	ua.public_type = 0x18;
	assert(ua_read_dev_info(&ua) == 0xFFFFFFFFu);
	ua.public_type = 0x2a;				/* past the mask */
	assert(ua_read_dev_info(&ua) == 0xFFFFFFFFu);
	ua.public_type = 0xFFFFFFFFu;			/* unresolved */
	assert(ua_read_dev_info(&ua) == 0xFFFFFFFFu && reads == 0);
	ua.public_type = 0;
	assert(ua_read_dev_info(&ua) == 0xFFFFFFFFu);
	return 0;
}
'''


def header_parts():
    hdr = csrc.source('ua_ioctl.h')
    apollo = csrc.source('ua_apollo.h')
    return '\n'.join([
        '\n'.join(re.findall(r'^#define UA_(?:ABI_M\w+|FEAT_FW_RELEASE|'
                             r'FW_(?:MAJOR|MINOR|BUILD)\(v\))\s.*$',
                             hdr, re.M)),
        csrc.between(hdr, 'struct ua_system_info {',
                     '#define UA_FW_MAJOR'),
        '\n'.join(re.findall(r'^#define UA_(?:REG_FW_RELEASE|REG_FW_BOOT|'
                             r'REG_DEV_INFO|DEVINFO_TYPES)\s.*$', apollo, re.M)),
    ]) + '\n'


def test_layout_and_gating():
    core = csrc.source('ua_core.c')
    helper = csrc.extract(core, 'static u32 ua_read_dev_info(')
    csrc.build_and_run(PREFIX + header_parts() + helper + MAIN)


def test_wired_into_system_info_and_version():
    core = csrc.source('ua_core.c')
    sysinfo = csrc.extract(core, 'static long ua_ioctl_get_system_info(')
    assert 'si.fw_release = ua_read(ua, UA_REG_FW_RELEASE);' in sysinfo
    assert 'si.dev_info = ua_read_dev_info(ua);' in sysinfo
    version = csrc.extract(core, 'static long ua_ioctl_get_version(')
    assert 'UA_FEAT_FW_RELEASE' in version
