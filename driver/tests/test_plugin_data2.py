"""Architecture-neutral pool-2 resources use the original generic DATA path.

Original kext LoadPlugin 0x3711c maps kind 2 to pool 2; transmit 0x34154
uses DATA for every nonzero pool; UnmapResource 0x34cd8 releases by pool.
All records here are synthetic; no proprietary payload is included.
"""
import csrc
import khost
import test_plugin_catalog as catalog
import test_plugin_upload as upload
import test_plugin_place as place


def test_instance_admits_data2_without_relaxing_other_guards():
    fn = csrc.extract(csrc.source('ua_plugin_instance.c'),
                      'static int ua_inst_check_set(')
    csrc.build_and_run(khost.PREFIX + r'''
struct ua_bill_info { unsigned int id, kind, arch, flags; };
struct ua_cat_entry { struct ua_bill_info info; };
struct ua_plugin_inst { unsigned int n_res; struct ua_cat_entry *res[4]; };
''' + fn + r'''
int main(void)
{
    struct ua_cat_entry a = {{0xf00011, 0, 2, 0}};
    struct ua_cat_entry b = {{0xf00012, 1, 0, 0}};
    struct ua_cat_entry c = {{0xf00013, 2, 0, 0}};
    struct ua_plugin_inst in = {3, {&a, &b, &c}};
    assert(ua_inst_check_set(&in) == 0);
    c.info.kind = 3;
    assert(ua_inst_check_set(&in) == 0);
    c.info.flags = 1;
    assert(ua_inst_check_set(&in) == -EOPNOTSUPP);
    c.info.flags = 0; c.info.kind = 5;
    assert(ua_inst_check_set(&in) == -EOPNOTSUPP);
    c.info.kind = 2; c.info.arch = 1;
    assert(ua_inst_check_set(&in) == -EINVAL);
    c.info.arch = 0; c.info.id = b.info.id;
    assert(ua_inst_check_set(&in) == -EEXIST);
    in.res[2] = &b;
    assert(ua_inst_check_set(&in) == -EINVAL);
    return 0;
}
''')


def test_data2_words_framing_normalization_and_release():
    blob = catalog.fixture(0xf00013, 2, 0, 558, 2300)
    extra = catalog.c_bytes('data2', blob)
    extra += catalog.c_bytes('expected2', upload.sdk_command(blob, 0xc6dd0))
    upload.build_upload_test(r'''
int main(void)
{
    struct ua_cat_entry e;
    struct ua_xact *x;
    u8 out[4096];
    size_t n;
    mk(&e, data2, sizeof(data2));
    assert(e.info.pool == 2 && e.info.words == 558 && e.info.arch == 0);
    assert(ua_upload_header(&e.info, e.len) == 0x40241);
    x = ua_xact_alloc(0, 4096);
    assert(ua_upload_build(x, &e, 0xc6dd0, UA_BILL_SDK) == 0);
    n = stream(x, out);
    assert(n == sizeof(expected2) && !memcmp(out, expected2, n));
    assert(!memcmp(e.blob, data2, e.len));
    ua_xact_discard(&dev, x);
    x = ua_xact_alloc(0, 0);
    ua_release_build(x, &e.info);
    assert(x->n == 1 && x->ent[0][0] == 0x60003);
    assert(x->ent[0][1] == 0xf00013 && x->ent[0][2] == 2);
    ua_xact_discard(&dev, x);
    return 0;
}
''', extra)


def test_data2_placement_coexists_with_scratch_and_private_rows():
    place.run(r'''
int main(void)
{
    struct ua_pool_request rq[3];
    struct ua_pool_lease leases[3];
    struct ua_pool_usage before[4], after[4];
    unsigned int dsp = 99;
    int code, data, row;
    setup(true); start(true); connect_all();
    assert(!ua_pool_set_scratch(host.dsp[1].pools, 1, 1096));
    assert(!ua_pool_set_scratch(host.dsp[1].pools, 2, 192));
    assert(!ua_pool_read_usage(host.dsp[1].pools, before));
    rq[0] = (struct ua_pool_request){0, 128, &code, true};
    rq[1] = (struct ua_pool_request){2, 558, &data, true};
    rq[2] = (struct ua_pool_request){2, 24, &row, false};
    mutex_lock(&dev.lock);
    assert(!ua_plugin_place(&dev, 1, rq, 3, &dsp, leases) && dsp == 1);
    assert(leases[1].base >= before[2].base + 192);
    assert(leases[1].base + 558 <= before[2].base + before[2].total_words);
    assert(leases[2].base + 24 <= leases[1].base ||
           leases[1].base + 558 <= leases[2].base);
    assert(!ua_pool_read_usage(host.dsp[1].pools, after));
    assert(after[2].free_words == before[2].free_words - 582);
    assert(after[1].free_words == before[1].free_words);
    assert(!ua_plugin_unplace(&dev, 1, leases, 3));
    assert(!ua_pool_read_usage(host.dsp[1].pools, after));
    assert(after[2].free_words == before[2].free_words);
    mutex_unlock(&dev.lock);
    setup(true);
    return 0;
}
''')
