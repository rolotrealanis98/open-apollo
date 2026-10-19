"""UA_PLUGIN_CREATE_F_START parameter commands: UA's WRITE framing.

The commands are UA's parameter-block WRITEs {0x0014LLLL, row << 24 | word,
skip mask, values}: the run spans L - 3 + popcount(mask) words and bit i of
the mask leaves word + i + 1 untouched, which is how UA's masks keep the
words the memspec links.  ua_inst_check_cmds() takes only such commands,
inside their rows and clear of every linked word; ua_inst_cmd_copy() sends
them with word 1 relocated to the row's DSP address.  The blocks here are
synthetic (no captured bytes).
"""
import csrc
import khost

SRC = csrc.source('ua_plugin_instance.c')


def run(main):
    code = (khost.PREFIX + khost.header('ua_resource_pool.h') +
            'struct ua_memspec_row { u32 pool, dwords, link, flags; };\n' +
            '#define UA_PLUGIN_CMD_WRITE 0x0014\n' +
            csrc.extract(SRC, 'static bool ua_inst_link_word(') + '\n' +
            csrc.extract(SRC, 'static int ua_inst_check_cmds(') + '\n' +
            csrc.extract(SRC, 'static unsigned int ua_inst_cmd_copy(') + '\n' + main)
    csrc.build_and_run(code, extra=('-Wno-unused-parameter',))


ROWS = r'''
/* a two-sub chain like UA's: head 10 words, sub A row 0 -> head+6, sub B row 0
 * -> head+8, B's second row -> B+20 */
static const struct ua_memspec_row rows[4] = {
	{ 1, 10, 0xffffffff, 0 },
	{ 1, 56, 0x00000006, 0 },
	{ 1, 66, 0x00000008, 0 },
	{ 2, 4, 0x02000014, 2 },
};
#define N 4
'''


def test_masks_keep_the_linked_words_and_values_fill_the_rest():
    run(ROWS + r'''
int main(void)
{
	/* head: 7 values, mask bits 2, 5, 7 = words 3, 6, 8 kept */
	static const u32 head[] = { 0x0014000a, 0x00000000, 0xa4,
		0x40000000, 1, 1, 0x22, 2, 0x111, 0x222 };
	/* sub B from word 4: bit 15 keeps word 20 (B's link target) */
	static const u32 b[] = { 0x00140014, 0x02000004, 1u << 15,
		1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17 };
	u32 two[sizeof(head) / 4 + sizeof(b) / 4];

	assert(ua_inst_check_cmds(rows, N, head, 10) == 1);
	assert(ua_inst_check_cmds(rows, N, b, 20) == 1);
	memcpy(two, head, sizeof(head));
	memcpy(two + 10, b, sizeof(b));
	assert(ua_inst_check_cmds(rows, N, two, 30) == 2);
	return 0;
}
''')


def test_refusals():
    run(ROWS + r'''
int main(void)
{
	/* the old one-word form: mask only, no value */
	static const u32 w1[] = { 0x00140003, 0x00000000, 0x40000000 };
	/* a value on head word 6 (sub A's link): no mask bit keeps it */
	static const u32 on6[] = { 0x0014000a, 0x00000000, 0x00,
		1, 2, 3, 4, 5, 6, 7 };
	/* past the row: 9 values + 2 kept words from word 0 of a 10-word row */
	static const u32 past[] = { 0x0014000c, 0x00000000, 0x24,
		1, 2, 3, 4, 5, 6, 7, 8, 9 };
	/* no such row; another opcode; a length past the buffer */
	static const u32 norow[] = { 0x00140004, 0x04000000, 0, 1 };
	static const u32 op[] = { 0x00150004, 0x00000000, 0, 1 };
	static const u32 longer[] = { 0x00140008, 0x01000000, 0, 1 };
	/* the head's form with sub A's link word written */
	static const u32 head_bad[] = { 0x0014000a, 0x00000000, 0x84,
		0x40000000, 1, 1, 0x22, 2, 0x111, 0x222 };

	assert(ua_inst_check_cmds(rows, N, w1, 3) == -EINVAL);
	assert(ua_inst_check_cmds(rows, N, on6, 10) == -EINVAL);
	assert(ua_inst_check_cmds(rows, N, past, 12) == -EINVAL);
	assert(ua_inst_check_cmds(rows, N, norow, 4) == -EINVAL);
	assert(ua_inst_check_cmds(rows, N, op, 4) == -EINVAL);
	assert(ua_inst_check_cmds(rows, N, longer, 4) == -EINVAL);
	assert(ua_inst_check_cmds(rows, N, head_bad, 10) == -EINVAL);
	return 0;
}
''')


def test_copy_relocates_word_1_only():
    run(ROWS + r'''
int main(void)
{
	static const u32 cmds[] = {
		0x0014000a, 0x00000000, 0xa4, 0x40000000, 1, 1, 0x22, 2, 0x111, 0x222,
		0x00140006, 0x02000000, 0, 0x40000000, 1, 1,
		0x00140006, 0x0200003a, 0, 7, 8, 9,
	};
	struct ua_pool_lease l[4];
	__le32 d[sizeof(cmds) / 4];

	memset(l, 0, sizeof(l));
	l[0].base = 0x9d194;
	l[2].base = 0x9d106;
	assert(ua_inst_check_cmds(rows, N, cmds, 22) == 3);
	assert(ua_inst_cmd_copy(d, cmds, 22, l) == 3);
	for (unsigned int i = 0; i < 22; i++)
		if (i != 1 && i != 11 && i != 17)
			assert(d[i] == cmds[i]);
	assert(d[1] == 0x9d194 && d[11] == 0x9d106 && d[17] == 0x9d106 + 0x3a);
	return 0;
}
''')


def test_ioctl_takes_the_commands_and_the_scratch():
    abi = csrc.source('ua_ioctl.h')
    assert '#define UA_PLUGIN_CMD_WORDS_MAX' in abi and 'struct ua_plugin_word' not in abi
    ioctl = csrc.extract(SRC, 'long ua_plugin_ioctl_instance(')
    # checked against the rows before the device is asked
    assert ioctl.index('ua_inst_check_rows(') < ioctl.index('ua_inst_check_cmds(') < \
        ioctl.index('ua_plugin_create(')
    # scratch only with an explicit DSP
    assert 'req.dsp == UA_PLUGIN_DSP_ANY' in ioctl
    create = csrc.extract(SRC, 'static int ua_plugin_create(')
    assert create.index('ua_inst_scratch_raise(') < create.index('ua_plugin_place(')
    unplace = csrc.extract(SRC, 'static int ua_inst_unplace(')
    assert unplace.index('ua_plugin_unplace(') < unplace.index('ua_inst_scratch_restore(')
    # zero-fills cover the even extent the ledger hands out
    assert 'rows[i].dwords + (rows[i].dwords & 1)' in SRC
