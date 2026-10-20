"""Live instance SERVICE over the fake FPGA, using synthetic memory/replies."""
import test_plugin_license as license


READ_FAKE = r'''
        } else if (e[0] == 0x000c0004) {
            u32 r[66];
            assert(e[2] <= 64);
            service_addr = e[1];
            r[0] = 0x80010000u | (e[2] + 2);
            r[1] = e[3];
            for (unsigned int k = 0; k < e[2]; k++)
                r[k + 2] = 0x12340000u + k;
            if (service_mode == 1) r[0] ^= 1;
            if (service_mode == 2) r[1] ^= 1;
            if (service_mode == 3) {
                /* one reply in two pieces, the first with its own length:
                 * the second is owed to the next descriptor */
                unsigned int half = (e[2] + 2) / 2;
                r[0] = 0x80010000u | half;
                deliver(d, r, half);
                deliver(d, r + half, e[2] + 2 - half);
            } else {
                static const u32 junk[3] = { 0x8001ffff, 0xffffffff, 0 };
                for (unsigned int k = 0; service_mode == 4 && k < 4; k++)
                    deliver(d, junk, 3);	/* always four behind */
                deliver(d, r, e[2] + 2);
            }
            if (service_mode < 4)
                service_mode = 0;	/* one reply only */
'''

# Replies the fake DSP owes: each takes the next descriptor (up to its room)
OWED = r'''
static u32 owed[16][66];
static unsigned int owed_len[16], n_owed;
static void deliver(unsigned int d, const u32 *w, unsigned int n)
{
    unsigned int r = 2 * d + 1;

    assert(n_owed < 16 && n <= 66);
    memcpy(owed[n_owed], w, n * 4);
    owed_len[n_owed++] = n;
    while (n_owed && pos[r] != target[r]) {
        u32 *e = slots[r][pos[r]];
        unsigned int room = e[0] & 0xffff;

        memcpy(bus(e[2], e[3]), owed[0], (owed_len[0] < room ? owed_len[0] : room) * 4);
        pos[r] = (pos[r] + 1) & 1023;
        replies_made++;
        memmove(owed, owed + 1, sizeof(owed[0]) * --n_owed);
        memmove(owed_len, owed_len + 1, sizeof(owed_len[0]) * n_owed);
    }
}
'''


def with_reads(fpga):
    """The fake FPGA answering SERVICE reads (test_plugin_swap uses it too)."""
    return ('static u32 service_addr; static int service_mode;\n' + fpga.replace(
        '\t\t} else if (e[0] == 0x00100002) {',
        READ_FAKE + '\t\t} else if (e[0] == 0x00100002) {').replace(
        '/* DSP @d consumes command entries', OWED + '/* DSP @d consumes command entries'))


def run(main):
    old = license.fake.FPGA
    license.fake.FPGA = with_reads(old)
    try:
        license.build_license(HELPERS + main)
    finally:
        license.fake.FPGA = old


HELPERS = r'''
static struct ua_plugin_instance instance;
static u32 values[64];
static void live(void)
{
    setup();
    bq_table[6] = 0x81000000;
    assert(create(&file_a, 0x806, 0, &instance) == 0);
    /* SERVICE's live-state fixture; F_START itself is tested separately. */
    mutex_lock(&dev.lock);
    ua_inst_find(&host, &file_a, instance.handle)->started = true;
    mutex_unlock(&dev.lock);
}
static long service(struct ua_file *f, u32 *cmd, unsigned int nw,
                    u32 ref, unsigned int nr, u64 epoch)
{
    struct ua_plugin_instance q = { 0 };
    uintptr_t p = (uintptr_t)cmd;
    q.struct_size = sizeof(q);
    q.op = UA_PLUGIN_INST_SERVICE;
    q.handle = instance.handle;
    q.epoch = epoch;
    q.reserved[0] = ref;
    q.reserved[1] = nw;
    q.reserved[2] = (u32)p;
    q.reserved[3] = (u64)p >> 32;
    q.reserved[4] = nr;
    q.privates = nr ? (uintptr_t)values : 0;
    return ua_plugin_ioctl_instance(&dev, f, (unsigned long)&q);
}
'''


def test_live_write_read_owner_and_bounds():
    run(r'''
int main(void)
{
    u32 cmd[] = { 0x00140004, 0x01000000, 0, 0x1234 };
    unsigned int sent;
    u32 base;
    live();
    base = ua_inst_find(&host, &file_a, instance.handle)->lease[3].base;
    sent = nputs;
    assert(service(&file_b, cmd, 4, 0, 0, instance.epoch) == -ENOENT);
    assert(service(&file_a, cmd, 4, 0, 0, instance.epoch + 1) == -ESTALE);
    cmd[1] = 6; /* head pointer owned by the memspec */
    assert(service(&file_a, cmd, 4, 0, 0, instance.epoch) == -EINVAL);
    cmd[1] = 0x01000004; /* row length is four */
    assert(service(&file_a, cmd, 4, 0, 0, instance.epoch) == -EINVAL);
    assert(service(&file_a, NULL, 0, 0x01000003, 2, instance.epoch) == -EINVAL);
    assert(service(&file_a, NULL, 0, 0x02000000, 1, instance.epoch) == -EINVAL);
    assert(service(&file_a, NULL, 0, 0, 65, instance.epoch) == -EINVAL);
    assert(nputs == sent);
    cmd[1] = 0x01000000;
    assert(service(&file_a, cmd, 4, 0, 0, instance.epoch) == 0);
    assert(service(&file_a, cmd, 4, 0x01000001, 3, instance.epoch) == 0);
    assert(service_addr == base + 1);
    assert(values[0] == 0x12340000 && values[2] == 0x12340002);
    assert(!sleeps_locked);
    ua_plugin_file_release(&file_a);
    assert(service(&file_a, cmd, 4, 0, 0, instance.epoch) == -ENOENT);
    cleanup();
    assert(!dma_live);
    return 0;
}
''')


def test_malformed_replies_do_not_escape():
    run(r'''
int main(void)
{
    live();
    for (int mode = 1; mode <= 2; mode++) {
        values[0] = 0xdeadbeef;
        service_mode = mode;
        k_log_reset();
        assert(service(&file_a, NULL, 0, 0, 1, instance.epoch) == -EPROTO);
        assert(values[0] == 0xdeadbeef);
        /* logged, then a probe found nothing stray */
        assert(K_HAS("got a reply that is not its own"));
        assert(K_HAS("SERVICE read: reply 0 of 3 words"));
        assert(K_HAS("realigned after 0 stray replies"));
        assert(!host.dsp[instance.dsp].quarantined);
        /* the probe's three spare descriptors go with the next reads */
        for (int i = 0; i < 3; i++)
            assert(service(&file_a, NULL, 0, 0, 1, instance.epoch) == 0);
    }
    ua_plugin_file_release(&file_a);
    cleanup();
    assert(!dma_live);
    return 0;
}
''')


def test_split_reply_is_absorbed_and_the_next_read_is_clean():
    run(r'''
int main(void)
{
    live();
    service_mode = 3;
    k_log_reset();
    assert(service(&file_a, NULL, 0, 0x01000000, 3, instance.epoch) == -EPROTO);
    assert(K_HAS("SERVICE read: reply 0 of 5 words: 80010002 00000000 data ffcccccc ffcccccc ffcccccc 00000000 .. ffcccccc ffcccccc, 2 words written"));
    assert(K_HAS("stray reply: reply 0 of 3 words: 12340000 12340001 data 12340002 00000000 00000000 00000000 .. 12340001 12340002, 3 words written"));
    assert(K_HAS("realigned after 1 stray reply"));
    assert(!host.dsp[instance.dsp].quarantined);
    for (int i = 0; i < 3; i++) {
        values[2] = 0;
        assert(service(&file_a, NULL, 0, 0x01000000, 3, instance.epoch) == 0);
        assert(values[0] == 0x12340000 && values[2] == 0x12340002);
    }
    ua_plugin_file_release(&file_a);
    cleanup();
    assert(!dma_live);
    return 0;
}
''')


def test_replies_that_stay_behind_quarantine():
    run(r'''
int main(void)
{
    live();
    service_mode = 4;
    k_log_reset();
    assert(service(&file_a, NULL, 0, 0, 1, instance.epoch) == -EPROTO);
    assert(K_HAS("stray reply: reply 3 of 4 words"));
    assert(host.dsp[instance.dsp].quarantined);
    assert(K_HAS("replies could not be realigned"));
    assert(service(&file_a, NULL, 0, 0, 1, instance.epoch) == -ESTALE);
    service_mode = 0;
    ua_plugin_file_release(&file_a);
    cleanup();
    return 0;
}
''')


def test_timeout_quarantines_and_refuses_further_updates():
    run(r'''
int main(void)
{
    u32 cmd[] = { 0x00140004, 0x01000000, 0, 42 };
    live();
    stalled = true;
    assert(service(&file_a, cmd, 4, 0, 0, instance.epoch) == -ETIMEDOUT);
    assert(host.dsp[instance.dsp].quarantined);
    assert(service(&file_a, cmd, 4, 0, 0, instance.epoch) == -ESTALE);
    stalled = false;
    ua_plugin_file_release(&file_a);
    cleanup();
    assert(!dma_live);
    return 0;
}
''')


def test_epoch_change_during_read_copies_nothing():
    run(r'''
static void invalidate(void)
{
    mutex_lock(&dev.lock);
    ua_plugin_epoch_bump(&dev, "test");
    mutex_unlock(&dev.lock);
    on_tick = NULL;
}
int main(void)
{
    live();
    values[0] = 0xdeadbeef;
    on_tick = invalidate;
    assert(service(&file_a, NULL, 0, 0, 1, instance.epoch) == -ESTALE);
    assert(values[0] == 0xdeadbeef);
    ua_plugin_file_release(&file_a);
    cleanup();
    assert(!dma_live);
    return 0;
}
''')
