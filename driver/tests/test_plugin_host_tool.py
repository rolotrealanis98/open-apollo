"""tools/plugin-host-check.py (the owner's hardware-session helper) agrees
with ua_ioctl.h: struct sizes, field offsets and ioctl numbers, checked
against the header compiled with the host compiler.  The tool is imported,
never run: nothing opens a device here.
"""
import importlib.util
import pathlib
import re
import struct

import csrc

TOOL = pathlib.Path(csrc.DRIVER).parent / 'tools' / 'plugin-host-check.py'


def tool():
    spec = importlib.util.spec_from_file_location('plugin_host_check', TOOL)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_layouts_and_numbers_match_the_header():
    t = tool()
    hdr = re.sub(r'^#include .*$', '', csrc.source('ua_ioctl.h'), flags=re.M)
    out = csrc.build_and_run(r'''
#include <stdio.h>
#include <stddef.h>
#include <stdint.h>
typedef uint8_t __u8;
typedef uint16_t __u16;
typedef uint32_t __u32;
typedef uint64_t __u64;
typedef int32_t __s32;
#define _IOC(d, t, n, s) (((d) << 30) | ((t) << 8) | (n) | ((s) << 16))
#define _IO(t, n) _IOC(0U, (t), (n), 0U)
#define _IOR(t, n, type) _IOC(2U, (t), (n), (unsigned)sizeof(type))
#define _IOW(t, n, type) _IOC(1U, (t), (n), (unsigned)sizeof(type))
#define _IOWR(t, n, type) _IOC(3U, (t), (n), (unsigned)sizeof(type))
''' + hdr + r'''
int main(void)
{
	printf("%zu %zu %zu %zu\n", sizeof(struct ua_version),
	       sizeof(struct ua_plugin_caps), sizeof(struct ua_board_query),
	       sizeof(struct ua_plugin_instance));
	printf("%u %u %u %u\n", UA_IOCTL_GET_VERSION, UA_IOCTL_GET_PLUGIN_CAPS,
	       UA_IOCTL_BOARD_QUERY, UA_IOCTL_PLUGIN_INSTANCE);
	printf("%zu %zu %zu %zu\n", offsetof(struct ua_plugin_instance, epoch),
	       offsetof(struct ua_plugin_instance, license_state),
	       offsetof(struct ua_board_query, words),
	       offsetof(struct ua_plugin_caps, owned_dsps));
	return 0;
}
''')
    sizes, numbers, offsets = [list(map(int, line.split())) for line in out.split('\n')[:3]]
    assert sizes == [t.VERSION.size, t.CAPS.size, t.BOARD_QUERY.size, t.INSTANCE.size]
    assert numbers == [t.GET_VERSION, t.GET_PLUGIN_CAPS, t.BOARD_QUERY_IOCTL,
                       t.PLUGIN_INSTANCE]
    assert offsets == [48, 56, 24, 28]


def test_packing_round_trips():
    t = tool()
    q = t.pack_board_query()
    assert len(q) == 3096 and struct.unpack_from('<I', q)[0] == 3096
    words = list(range(768))
    buf = struct.pack('<4IQ768I', 3096, 0, 0x1234, 0, 7, *words)
    r = t.unpack_board_query(buf)
    assert r['date'] == 0x1234 and r['epoch'] == 7 and list(r['words']) == words
    c = t.pack_check(0x806)
    f = t.INSTANCE.unpack(bytes(c))
    assert f[:8] == (88, 1, 1, 0, 0x806, 0xffffffff, 0, 0)
    out = bytearray(c)
    struct.pack_into('<QII', out, 48, 9, 2, 0x80000000)
    r = t.unpack_instance(bytes(out))
    assert r['epoch'] == 9 and r['license_state'] == 2 and r['license_word'] == 0x80000000
    assert [t.state_of(w) for w in (0x81000000, 0x82000000, 0x80000000, 0,
                                    0x83000000, 0xd000)] == [0, 0, 2, 3, 4, 1]


def test_the_tool_never_prints_the_table():
    src = TOOL.read_text()
    query = src[src.index('def cmd_query('):src.index('def cmd_check(')]
    # only counts and the requested ids' states
    assert "q['words'])" not in query.replace("q['words']:", '')
    assert 'HARDWARE TOOL' in src
