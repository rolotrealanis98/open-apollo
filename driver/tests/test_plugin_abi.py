"""Plug-in hosting ABI 1.8 (task A1) and the licensing boundary scan.

- Numbers: BOARD_QUERY 0x90 and PLUGIN_INSTANCE 0x94 (0x91/0x92 never
  assigned, 0x09-0x0e still free), GET_PLUGIN_CAPS stays 0x07 and
  GET_VERSION 0x04 (the Codex foundation commit 199bb21 had caps at 0x04).
- Both ioctls go through ua_ioctl_dispatch() with the open file and are not
  raw: the driver builds every word that reaches the device.
- GET_VERSION: with plugin_host=0 the features word is the old one; with
  plugin_host=1 bit 31 says reserved[0] holds UA_FEAT2_PLUGIN_HOST.
- Overlay review finding 11: the withdrawn prototype's leftovers never come
  back (registration preflight, the serial-type gate, pinned 4K hashes, the
  Windows notification block, PREPARE sessions, the authorization snapshot
  ioctl, the global untracked-orphan latch, host-audio exclusivity), and no
  date override or demo start exists anywhere.
"""
import re
import pathlib
import csrc

ABI_PREFIX = r'''
#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#define _IOC_READ 2U
#define _IOC_WRITE 1U
#define _IOC(d, t, n, s) (((d) << 30) | ((t) << 8) | (n) | ((s) << 16))
#define _IO(t, n) _IOC(0U, (t), (n), 0U)
#define _IOR(t, n, type) _IOC(_IOC_READ, (t), (n), (unsigned)sizeof(type))
#define _IOW(t, n, type) _IOC(_IOC_WRITE, (t), (n), (unsigned)sizeof(type))
#define _IOWR(t, n, type) _IOC(_IOC_READ | _IOC_WRITE, (t), (n), (unsigned)sizeof(type))
#define _IOC_NR(c) ((c) & 0xff)
#define _IOC_DIR(c) (((c) >> 30) & 3)
#define _IOC_SIZE(c) (((c) >> 16) & 0x3fff)
typedef uint8_t __u8;
typedef uint16_t __u16;
typedef uint32_t __u32;
typedef uint64_t __u64;
typedef int32_t __s32;
#define __aligned_u64 __u64 __attribute__((aligned(8)))
'''


def ioctl_header():
    return re.sub(r'^#include .*$', '', csrc.source('ua_ioctl.h'), flags=re.M)


def test_structs_and_numbers():
    csrc.build_and_run(ABI_PREFIX + ioctl_header() + r'''
int main(void)
{
	assert(_IOC_NR(UA_IOCTL_BOARD_QUERY) == 0x90);
	assert(_IOC_DIR(UA_IOCTL_BOARD_QUERY) == (_IOC_READ | _IOC_WRITE));
	assert(sizeof(struct ua_board_query) == 24 + 4 * 768);
	assert(_IOC_SIZE(UA_IOCTL_BOARD_QUERY) == sizeof(struct ua_board_query));
	assert(offsetof(struct ua_board_query, epoch) == 16);
	assert(offsetof(struct ua_board_query, words) == 24);
	assert(UA_BOARD_QUERY_WORDS == 768 && UA_BOARD_QUERY_BASE == 0x800);

	assert(_IOC_NR(UA_IOCTL_PLUGIN_INSTANCE) == 0x94);
	assert(sizeof(struct ua_plugin_instance) == 88);
	assert(offsetof(struct ua_plugin_instance, resources) == 32);
	assert(offsetof(struct ua_plugin_instance, epoch) == 48);
	assert(offsetof(struct ua_plugin_instance, license_state) == 56);
	assert(sizeof(struct ua_plugin_resource) == 32);
	assert(sizeof(struct ua_plugin_private) == 16);
	assert(UA_PLUGIN_LICENSE_AUTHORIZED == 0 && UA_PLUGIN_LICENSE_DEMO == 1 &&
	       UA_PLUGIN_LICENSE_NOT_STARTED == 2 && UA_PLUGIN_LICENSE_EXPIRED == 3 &&
	       UA_PLUGIN_LICENSE_UNAVAILABLE == 4);

	assert(_IOC_NR(UA_IOCTL_GET_VERSION) == 0x04);
	assert(_IOC_NR(UA_IOCTL_GET_PLUGIN_CAPS) == 0x07);
	assert(UA_ABI_MAJOR == 1 && UA_ABI_MINOR == 8);
	assert(UA_FEAT_FEATURES2 == 1u << 31 && UA_FEAT2_PLUGIN_HOST == 1);
	return 0;
}
''')


def test_kept_free_numbers_and_events():
    hdr = csrc.source('ua_ioctl.h')
    numbers = [int(n, 16) for n in
               re.findall(r'_IO[RW]*\(UA_IOCTL_MAGIC,\s*(0x[0-9a-fA-F]+)', hdr)]
    assert len(numbers) == len(set(numbers))
    for n in list(range(0x09, 0x0f)) + [0x91, 0x92, 0x93]:
        assert n not in numbers, hex(n)
    assert {0x90, 0x94} <= set(numbers)
    events = [int(n, 16) for n in
              re.findall(r'#define UA_EVT_\w+\s+(0x[0-9a-fA-F]+)', hdr)]
    assert 0x1008 not in events and 0x1009 not in events


def test_dispatch_and_privilege():
    core = csrc.source('ua_core.c')
    dispatch = csrc.extract(core, 'static long ua_ioctl_dispatch(')
    assert dispatch.startswith('static long ua_ioctl_dispatch(struct ua_device *ua, struct ua_file *uf,')
    assert 'case UA_IOCTL_BOARD_QUERY:\n\t\treturn ua_plugin_ioctl_board_query(ua, arg);' in dispatch
    assert 'case UA_IOCTL_PLUGIN_INSTANCE:\n\t\treturn ua_plugin_ioctl_instance(ua, uf, arg);' in dispatch
    rawio = csrc.extract(core, 'static bool ua_ioctl_needs_rawio(')
    assert 'BOARD_QUERY' not in rawio and 'PLUGIN_INSTANCE' not in rawio
    ioctl = csrc.extract(core, 'static long ua_ioctl(struct file *filp')
    # nothing bypasses the dispatch (the withdrawn overlay's ioctls did)
    assert ioctl.count('ua_ioctl_dispatch(ua, uf, cmd, arg)') == 1
    assert 'PLUGIN' not in ioctl and 'BOARD' not in ioctl


VERSION_PREFIX = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
typedef uint32_t __u32;
#define __user
#define DRIVER_VERSION "0.2.0"
struct ua_device { int unused; };
static bool solo, host;
static bool ua_dsp_solo_flag_honoured(void) { return solo; }
static bool ua_plugin_host_enabled(void) { return host; }
static struct ua_version out;
static unsigned long copy_to_user(void *to, const void *from, unsigned long n)
{ (void)to; memcpy(&out, from, n); return 0; }
static void strscpy(char *d, const char *s, unsigned long n)
{ strncpy(d, s, n - 1); d[n - 1] = 0; }
'''


def test_version_features2_only_with_plugin_host():
    hdr = csrc.source('ua_ioctl.h')
    abi = '\n'.join(re.findall(r'^#define UA_(?:ABI_M\w+|FEAT_\w+|FEAT2_\w+)\s.*$',
                               hdr, re.M))
    struct = csrc.between(hdr, 'struct ua_version {', '#define UA_IOCTL_GET_VERSION')
    fn = csrc.extract(csrc.source('ua_core.c'), 'static long ua_ioctl_get_version(')
    csrc.build_and_run(VERSION_PREFIX + abi + "\n" + struct + fn + r'''
int main(void)
{
	struct ua_device ua;
	__u32 base;

	assert(ua_ioctl_get_version(&ua, 1) == 0);
	base = out.features;
	assert(!(base & UA_FEAT_FEATURES2) && out.reserved[0] == 0);
	assert(out.abi_minor == 8);
	host = true;
	assert(ua_ioctl_get_version(&ua, 1) == 0);
	assert(out.features == (base | UA_FEAT_FEATURES2));
	assert(out.reserved[0] == (UA_FEAT2_PLUGIN_HOST |
	       UA_FEAT2_PLUGIN_MULTI_INPUT | UA_FEAT2_PLUGIN_STEREO | UA_FEAT2_PLUGIN_SWAP | UA_FEAT2_PLUGIN_CHAIN | UA_FEAT2_PLUGIN_DELAY | UA_FEAT2_PLUGIN_UNISON | UA_FEAT2_PLUGIN_ANALOG_LEASE | UA_FEAT2_PLUGIN_START_STAGED | UA_FEAT2_PLUGIN_VIRTUAL_INPUTS));
	assert(UA_FEAT2_PLUGIN_MULTI_INPUT == 2 && UA_FEAT2_PLUGIN_STEREO == 4);
	assert(!out.reserved[1] && !out.reserved[2] && !out.reserved[3]);
	return 0;
}
''', extra=('-Wno-unused-parameter',))


def test_plugin_host_parameter_defaults_off():
    src = csrc.source('ua_plugin_host.c')
    assert 'static bool plugin_host;' in src
    assert 'module_param(plugin_host, bool, 0444);' in src
    assert 'ua_plugin_host.o' in csrc.source('Makefile')


FORBIDDEN = [
    # overlay leftovers (claude-review-20261011.md, finding 11 and 12)
    'ua_dsp_registration_preflight', 'ua_cpu_type26',
    'ua_plugin_session_prepare_4k', 'ua_plugin_session_initial_cache',
    'plugin_prepare_prerequisite', 'EXPERIMENTAL_PREPARE',
    'UA_IOCTL_EXPERIMENTAL', 'WINDOWS_NOTIF', 'WINDOWS_IDENTITY',
    'SYSINFO_RAW_ID', 'REGISTRATION_EFFECT', 'AUTH_SNAPSHOT',
    'plugin_host_audio_exclusive', 'dma_orphan_untracked',
    'ua_plugin_session_prepare', 'quiescence_only',
    # authorization must only ever be read, never produced or overridden
    'ignore_dsp_version_auth', 'PluginStartDemo', 'start_demo',
    'demo_start', 'authorize_override', 'license_override',
]


def driver_sources():
    root = pathlib.Path(csrc.DRIVER)
    return [p for p in root.iterdir() if p.suffix in ('.c', '.h')]


def test_no_withdrawn_symbols_or_authorization_writers():
    for path in driver_sources():
        text = path.read_text()
        for name in FORBIDDEN:
            assert name not in text, (path.name, name)
        # no module parameter can set a date, a table or a license state
        for param in re.findall(r'module_param(?:_named|_cb)?\((\w+)', text):
            assert not re.search(r'date|auth|licen|demo|table', param, re.I), \
                (path.name, param)
        # selector 107 / vendor op 211 (demo activation) are never sent
        assert not re.search(r'\b(?:0x6b|107|0xd3|211)\b.*(?:selector|demo)',
                             text, re.I), path.name
