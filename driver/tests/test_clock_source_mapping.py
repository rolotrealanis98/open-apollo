"""Clock source values: the driver's UA_CLOCK_* (the kext's UAD2ClockSource,
clock_config low byte), the ALSA control's list, what SET_DRIVER_PARAM
accepts, and the mixer daemon's CLOCK_SOURCE_MAP agree.

ua_clock_source_valid() is compiled as it is.  Adapted from the parallel
Codex work (test_clock_source_mapping); driver-v2 already had the native
values, Dante (9) was missing from the accepted set.
"""
import re
import csrc

ROOT = csrc.DRIVER.parent


def header_clock_values():
    hdr = csrc.source('ua_apollo.h')
    return {m[0]: int(m[1], 16) for m in re.findall(
        r'^#define UA_CLOCK_(SPDIF|ADAT|WORDCLOCK|DANTE|INTERNAL|PEER)\s+'
        r'(0x[0-9A-Fa-f]+)', hdr, re.M)}


def test_values_match_the_daemon_map():
    v = header_clock_values()
    assert v == {'SPDIF': 0x00, 'ADAT': 0x05, 'WORDCLOCK': 0x07,
                 'DANTE': 0x09, 'INTERNAL': 0x0C, 'PEER': 0x0A}
    hw = (ROOT / 'mixer-engine' / 'hardware.py').read_text()
    block = hw[hw.index('CLOCK_SOURCE_MAP = {'):]
    block = block[:block.index('\n}')]
    daemon = {k: int(x, 16) for k, x in
              re.findall(r'"([^"]+)":\s*(0x[0-9A-Fa-f]+)', block)}
    assert daemon == {'Internal': v['INTERNAL'], 'S/PDIF': v['SPDIF'],
                      'ADAT': v['ADAT'], 'Word Clock': v['WORDCLOCK'],
                      'Dante': v['DANTE']}
    # The fixed ALSA list carries the native values, not indices
    audio = csrc.source('ua_audio.c')
    names = csrc.between(audio, 'static const char * const ua_clock_source_names[]',
                         '};')
    values = csrc.between(audio, 'static const u8 ua_clock_source_values[]',
                          '};')
    assert '"Internal", "S/PDIF", "ADAT", "Word Clock"' in names
    assert ('UA_CLOCK_INTERNAL, UA_CLOCK_SPDIF, UA_CLOCK_ADAT, '
            'UA_CLOCK_WORDCLOCK') in values


PREFIX = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
typedef uint8_t u8;
typedef uint16_t u16;
#define BIT(n) (1UL << (n))
#define READ_ONCE(x) (x)
#define UA_CLOCK_LIST_MAX 16
static bool clock_src_from_device;
struct ua_audio { u16 clock_src_mask; u8 clock_list_n;
		  u8 clock_list_val[UA_CLOCK_LIST_MAX]; };
struct ua_device { struct ua_audio audio; };
'''

MAIN = r'''
int main(void)
{
	struct ua_device ua = { 0 };
	unsigned int s;

	/* Not connected (no mask yet): the known values, Dante included */
	for (s = 0; s < 256; s++)
		assert(ua_clock_source_valid(&ua, s) ==
		       (s == UA_CLOCK_INTERNAL || s == UA_CLOCK_SPDIF ||
			s == UA_CLOCK_ADAT || s == UA_CLOCK_WORDCLOCK ||
			s == UA_CLOCK_DANTE));
	assert(!ua_clock_source_valid(&ua, UA_CLOCK_PEER));

	/* An x4's mask (S/PDIF, ADAT, peer, Internal): no Dante, no WC */
	ua.audio.clock_src_mask = BIT(0) | BIT(5) | BIT(10) | BIT(12);
	assert(ua_clock_source_valid(&ua, UA_CLOCK_INTERNAL));
	assert(ua_clock_source_valid(&ua, UA_CLOCK_SPDIF));
	assert(!ua_clock_source_valid(&ua, UA_CLOCK_DANTE));
	assert(!ua_clock_source_valid(&ua, UA_CLOCK_WORDCLOCK));
	assert(!ua_clock_source_valid(&ua, UA_CLOCK_PEER));

	/* An x16D offers Dante */
	ua.audio.clock_src_mask = BIT(7) | BIT(9) | BIT(12);
	assert(ua_clock_source_valid(&ua, UA_CLOCK_DANTE));
	assert(!ua_clock_source_valid(&ua, UA_CLOCK_ADAT));

	/* clock_src_from_device: exactly the device's menu */
	clock_src_from_device = true;
	ua.audio.clock_list_n = 2;
	ua.audio.clock_list_val[0] = UA_CLOCK_INTERNAL;
	ua.audio.clock_list_val[1] = UA_CLOCK_DANTE;
	assert(ua_clock_source_valid(&ua, UA_CLOCK_DANTE));
	assert(!ua_clock_source_valid(&ua, UA_CLOCK_WORDCLOCK));
	return 0;
}
'''


def test_accepted_sources():
    hdr = csrc.source('ua_apollo.h')
    defines = '\n'.join(re.findall(r'^#define UA_CLOCK_(?:SPDIF|ADAT|'
                                   r'WORDCLOCK|DANTE|INTERNAL|PEER)\s.*$',
                                   hdr, re.M))
    audio = csrc.source('ua_audio.c')
    code = (csrc.extract(audio, 'static bool ua_clock_list_active(') +
            '\n' + csrc.extract(audio, 'bool ua_clock_source_valid('))
    csrc.build_and_run(PREFIX + defines + '\n' + code + MAIN)
