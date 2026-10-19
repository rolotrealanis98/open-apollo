"""Kind3 resources use the original pool3 DATA path, without payload changes.

Synthetic Bill matches the API dependency's shape:3072 words,12340 bytes,
architecture0,flags0. No original resource bytes are included.
"""
import test_plugin_catalog as catalog
import test_plugin_upload as upload
import test_plugin_place as place


def test_data3_full_record_dma_framing_reply_and_release():
    blob = catalog.fixture(0xf00023, 3, 0, 3072, 12340)
    extra = catalog.c_bytes('data3', blob)
    extra += catalog.c_bytes('expected3', upload.sdk_command(blob, 0x08ffe000))
    upload.build_upload_test(r"""
int main(void)
{
    struct ua_cat_entry e;
    struct ua_xact *x;
    static u8 out[16384];
    __le32 reply[4] = {0x80070004, 0, 0xf00023, 0x40c0f};
    bool echo = false;
    size_t n;
    mk(&e, data3, sizeof(data3));
    assert(e.info.pool == 3 && e.info.words == 3072 && !e.info.arch && !e.info.flags);
    assert(ua_upload_header(&e.info, e.len) == 0x40c0f);
    x = ua_xact_alloc(0, 16384);
    assert(ua_upload_build(x, &e, 0x08ffe000, UA_BILL_SDK) == 0);
    n = stream(x, out); /* also checks every DMA segment's page boundary */
    assert(n == sizeof(expected3) && !memcmp(out, expected3, n));
    assert(!memcmp(e.blob, data3, e.len));
    assert(!ua_upload_reply_check(reply, &e, UA_BILL_SDK, 0x40c0f, &echo) && echo);
    reply[2]++;
    assert(ua_upload_reply_check(reply, &e, UA_BILL_SDK, 0x40c0f, &echo) == -EPROTO);
    ua_xact_discard(&dev, x);
    x = ua_xact_alloc(0, 0);
    ua_release_build(x, &e.info);
    assert(x->n == 1 && x->ent[0][0] == 0x60003);
    assert(x->ent[0][1] == 0xf00023 && x->ent[0][2] == 3);
    ua_xact_discard(&dev, x);
    return 0;
}
""", extra)


def test_data3_shared_resource_private_rows_scratch_and_late_rollback():
    place.run(r"""
int main(void)
{
    struct ua_pool_request rq[3];
    struct ua_pool_lease leases[3], existing, zero[3] = {0};
    struct ua_pool_usage before[4], after[4];
    unsigned int dsp = 99;
    int code, data, row, objects;
    setup(true); start(true); connect_all();
    assert(!ua_pool_set_scratch(host.dsp[1].pools, 3, 65536));
    assert(!ua_pool_reserve(host.dsp[1].pools, 3, 3072, &data, true, &existing));
    assert(!ua_pool_read_usage(host.dsp[1].pools, before));
    objects = k_objects;
    rq[0] = (struct ua_pool_request){0, 128, &code, true};
    rq[1] = (struct ua_pool_request){3, 3072, &data, true};
    rq[2] = (struct ua_pool_request){3, before[3].total_words, &row, false};
    mutex_lock(&dev.lock);
    assert(ua_plugin_place(&dev, 1, rq, 3, &dsp, leases) == -ENOSPC);
    assert(!memcmp(leases, zero, sizeof(leases)) && dsp == 99 && k_objects == objects);
    assert(!ua_pool_read_usage(host.dsp[1].pools, after) &&
           !memcmp(before, after, sizeof(before)));
    rq[2].words = 64;
    assert(!ua_plugin_place(&dev, 1, rq, 3, &dsp, leases) && dsp == 1);
    assert(leases[1].base == existing.base && leases[1].cookie != existing.cookie);
    assert(leases[2].base >= before[3].base + 65536);
    assert(leases[2].base + 64 <= leases[1].base ||
           leases[1].base + 3072 <= leases[2].base);
    assert(!ua_pool_read_usage(host.dsp[1].pools, after));
    assert(after[3].free_words == before[3].free_words - 64 && after[3].holds == 3);
    assert(!ua_plugin_unplace(&dev, 1, leases, 3));
    assert(!ua_pool_read_usage(host.dsp[1].pools, after) &&
           !memcmp(before, after, sizeof(before)));
    assert(!ua_pool_release(host.dsp[1].pools, &existing));
    mutex_unlock(&dev.lock);
    setup(true);
    return 0;
}
""")
