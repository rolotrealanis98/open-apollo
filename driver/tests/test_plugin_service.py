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
            reply_on(d, r, e[2] + 2);
'''


def run(main):
    old = license.fake.FPGA
    license.fake.FPGA = (
        'static u32 service_addr; static int service_mode;\n' + old.replace(
            '\t\t} else if (e[0] == 0x00100002) {',
            READ_FAKE + '\t\t} else if (e[0] == 0x00100002) {'))
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
    for (service_mode = 1; service_mode <= 2; service_mode++) {
        values[0] = 0xdeadbeef;
        assert(service(&file_a, NULL, 0, 0, 1, instance.epoch) == -EPROTO);
        assert(values[0] == 0xdeadbeef);
    }
    service_mode = 0;
    assert(service(&file_a, NULL, 0, 0, 1, instance.epoch) == 0);
    ua_plugin_file_release(&file_a);
    cleanup();
    assert(!dma_live);
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
