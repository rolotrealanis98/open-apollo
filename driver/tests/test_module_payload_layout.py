"""ua_dsp_configure_modules(): every module header and I/O descriptor survives
in the DMA page.

The payload builder is compiled as it is in ua_dsp.c (with its BUILD_BUG_ON
bounds) and the page it builds is compared with the templates.  With the old
128-byte slots module 1's header overwrote the last six words of module 0's
22-word descriptor.  From the parallel Codex work (test_module_payload_layout).
"""
import csrc

PREFIX = r'''
#include <assert.h>
#include <stdint.h>
#include <string.h>
typedef uint32_t u32;
typedef uint8_t u8;
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define BUILD_BUG_ON(c) _Static_assert(!(c), "layout bounds")
#define UA_PCIE_PAGE_SIZE 4096
'''

MAIN = r'''
int main(void)
{
	unsigned char page[UA_PCIE_PAGE_SIZE] = { 0 };
	void *dma_page = page;
	const struct ua_module_addrs *mod;
	unsigned int i;

	(void)ua_x4_routing; (void)ua_x4_synch;
BUILDER
	for (i = 0; i < UA_MODULE_COUNT; i++) {
		u32 want_io[22] = { 0 }, want_hdr[9];
		const u32 *tmpl;
		size_t size;

		mod = &ua_x4_modules[i];
		if (i == 0) {
			tmpl = ua_mod0_io_template; size = sizeof(ua_mod0_io_template);
		} else if (i <= 2) {
			tmpl = ua_mod_code_io_template; size = sizeof(ua_mod_code_io_template);
		} else if (i <= 4) {
			tmpl = ua_mod_passthru_io_template; size = sizeof(ua_mod_passthru_io_template);
		} else {
			tmpl = ua_mod5_io_template; size = sizeof(ua_mod5_io_template);
		}
		memcpy(want_io, tmpl, size);
		want_io[1] = mod->sram_io + ((i == 3 || i == 4) ? 8 : 0);
		assert(!memcmp(page + (i + 1) * MOD_SLOT_SIZE + MOD_IO_OFF,
			       want_io, size));

		memcpy(want_hdr, ua_mod_hdr_template, sizeof(want_hdr));
		want_hdr[1] = mod->sram_hdr;
		want_hdr[8] = i == 0 ? UA_MOD_EP_MIXER : UA_MOD_EP_ROUTING;
		if (i == 5) {
			want_hdr[4] = 2;
			want_hdr[5] = 2;
		}
		assert(!memcmp(page + (i + 1) * MOD_SLOT_SIZE + MOD_HDR_OFF,
			       want_hdr, sizeof(want_hdr)));
	}
	/* The zero slot the header-area clears read from stays zero */
	for (i = 0; i < MOD_SLOT_SIZE; i++)
		assert(page[i] == 0);
	/* The extended wiring block at the end of the page is not reached */
	for (i = (UA_MODULE_COUNT + 1) * MOD_SLOT_SIZE;
	     i < UA_PCIE_PAGE_SIZE; i++)
		assert(page[i] == 0);
	return 0;
}
'''


def test_headers_do_not_overwrite_descriptors():
    dsp = csrc.source('ua_dsp.c')
    builder = csrc.between(dsp, '#define MOD_SLOT_SIZE',
                           '\n\t/*\n\t * Verify ring space:')
    assert 'MOD_SLOT_SIZE\t256' in builder
    assert 'BUILD_BUG_ON(MOD_IO_OFF + sizeof(ua_mod0_io_template)' in builder
    header = csrc.source('ua_dsp_modules.h').replace(
        '#include <linux/types.h>', '')
    csrc.build_and_run(PREFIX + header + MAIN.replace('BUILDER', builder))


def test_old_slot_size_fails_the_bounds_check():
    """The BUILD_BUG_ON catches a 128-byte slot at compile time."""
    dsp = csrc.source('ua_dsp.c')
    builder = csrc.between(dsp, '#define MOD_SLOT_SIZE',
                           '\n\t/*\n\t * Verify ring space:')
    builder = builder.replace('MOD_SLOT_SIZE\t256', 'MOD_SLOT_SIZE\t128')
    header = csrc.source('ua_dsp_modules.h').replace(
        '#include <linux/types.h>', '')
    try:
        csrc.build_and_run(PREFIX + header +
                           MAIN.replace('BUILDER', builder))
    except AssertionError as e:
        assert 'layout bounds' in str(e)
    else:
        raise AssertionError('128-byte slots compiled')
