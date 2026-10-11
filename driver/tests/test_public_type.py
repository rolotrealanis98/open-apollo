"""The kext's public device type (CPcieDevice::GetCapabilities, kext
12.0.0 x86 0x464c).

Checked against the disassembly:
  * the family jump table at 0x4d5c has 22 entries: families 20 and 21
    give 0, family 22 the x16D's 0x20 (the old 20-entry table put 0x20 on
    family 20);
  * QUAD and HEXA fall back on the FPGA_REV type
    (CUAD2Device::_deviceTypeFromFPGAVersion 0x14528) when the serial does
    not decide; HEXA serial 0x35 gives 0x21 (the kext's fall-through at
    0x4a30 / arm64e 0xcda8).
The functions are compiled from ua_core.c as they are.
"""
import re
import csrc

PREFIX = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
typedef uint32_t u32;
typedef uint8_t u8;
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define dev_info(dev, ...) ((void)(dev))
struct pci_dev { int dev; };
struct ua_device { bool fw_v2; u32 ext_caps, fpga_rev, device_type,
		   public_type; struct pci_dev *pdev; };
static u32 serial_type;
static u32 ua_serial_lookup(struct ua_device *ua, bool verbose)
{ (void)ua; (void)verbose; return serial_type; }
'''

MAIN = r'''
static struct pci_dev pd;

static u32 pub(u32 family, u32 st, u32 fpga)
{
	struct ua_device ua = { .fw_v2 = true, .ext_caps = family << 20,
				.fpga_rev = fpga, .pdev = &pd };
	serial_type = st;
	return ua_compute_public_type(&ua);
}

int main(void)
{
	const u32 old = 0x90000000;	/* old-format FPGA_REV: no type byte */
	const u32 x4_fpga = 0x9f0ea929;	/* titan */

	assert(ua_type_from_fpga_rev(x4_fpga) == 0x1F);
	assert(ua_type_from_fpga_rev(old) == 9);

	/* plain families */
	assert(pub(1, 0, old) == 0x05);
	assert(pub(5, 0, old) == 0x08);
	assert(pub(10, 0, old) == 0x0F);
	assert(pub(11, 0, old) == 0x0E);
	assert(pub(14, 0, old) == 0x12);
	assert(pub(18, 0, old) == 0x18);
	assert(pub(20, 0, old) == 0);	/* was 0x20 */
	assert(pub(21, 0, old) == 0);
	assert(pub(22, 0, old) == 0x20);	/* x16D family */
	assert(pub(23, 0, old) == 0);
	assert(pub(0, 0, old) == 0);
	assert(pub(0x3f, 0, old) == UA_PUBTYPE_NONE);

	/* QUAD: serial, then the FPGA type */
	assert(pub(16, UA_DEV_APOLLO_X4, x4_fpga) == 0x1A);
	assert(pub(16, 0, x4_fpga) == 0x1A);
	assert(pub(16, 0, 0xb60ea929) == 0x26);
	assert(pub(16, 0, 0xbf0ea929) == 0x28);
	assert(pub(16, 0, old) == UA_PUBTYPE_NONE);

	/* HEXA */
	assert(pub(17, UA_DEV_APOLLO_X8P, old) == 0x16);
	assert(pub(17, UA_DEV_APOLLO_X16D, old) == 0x21);	/* fall-through */
	assert(pub(17, UA_DEV_APOLLO_X16_GEN2, old) == 0x24);
	assert(pub(17, 0, 0xb50ea929) == 0x20);		/* FPGA says x16D */
	assert(pub(17, UA_DEV_APOLLO_X4_GEN2, 0xb50ea929) == 0x20);
	assert(pub(17, 0, old) == 0x16);

	/* Conrad family and Twin X family */
	assert(pub(15, UA_DEV_SERIAL_2016, old) == 0x13);
	assert(pub(15, UA_DEV_SERIAL_2032, old) == 0x1C);
	assert(pub(15, UA_DEV_SERIAL_2110, old) == 0x27);
	assert(pub(19, UA_DEV_APOLLO_SOLO, old) == 0x19);
	assert(pub(19, UA_DEV_APOLLO_TWIN_X_GEN2, old) == 0x25);

	/* v1 */
	{
		struct ua_device ua = { .fw_v2 = false, .pdev = &pd };
		assert(ua_compute_public_type(&ua) == 0);
	}
	return 0;
}
'''


def defines():
    hdr = csrc.source('ua_apollo.h')
    return '\n'.join(re.findall(
        r'^#define (?:UA_DEV_\w+(?:\(pt\))?|UA_PUBLIC_DEVTYPE_BASE|'
        r'UA_PUBTYPE_NONE)\s.*$', hdr, re.M)) + '\n'


def test_public_type():
    core = csrc.source('ua_core.c')
    code = '\n'.join(csrc.extract(core, sig) for sig in (
        'static u32 ua_type_from_fpga_rev(',
        'static u32 ua_compute_public_type('))
    csrc.build_and_run(PREFIX + defines() + code + MAIN)
