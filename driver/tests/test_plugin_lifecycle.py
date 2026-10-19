"""Plug-in host lifecycle wiring (task S2).

The strips (and later the plug-in instances) are taken down in order on
every reconnect, rate change, suspend, shutdown and remove, and forgotten
without a command when the device's state is gone; the host's waits happen
with ua->lock released.  These are the call sites in ua_core.c and
ua_audio.c, checked where they sit (the behaviour behind them is in
test_input_strips.py on the fake FPGA).
"""
import csrc


def fn(file, sig):
    return csrc.extract(csrc.source(file), sig)


def before(body, a, b):
    assert a in body, a
    assert b in body, b
    return body.index(a) < body.index(b)


def test_teardown_before_every_reconnect_and_rate_change():
    disc = fn('ua_audio.c', 'static void ua_audio_disconnect_hw(')
    assert before(disc, 'if (!audio->connected)\n\t\treturn;',
                  'ua_plugin_teardown(ua, "disconnect");')
    assert before(disc, 'ua_plugin_teardown(ua, "disconnect");',
                  'ua_write(ua, UA_REG_AX_CONTROL, 0);')
    rec = fn('ua_audio.c', 'int ua_audio_reconnect(')
    assert before(rec, 'ua_plugin_teardown(ua, "reconnect");',
                  'ret = ua_audio_connect(ua);')
    clk = fn('ua_audio.c', 'static int ua_audio_set_clock_cfg(')
    assert before(clk, 'ua_plugin_teardown(ua, "rate change");',
                  'ret = ua_audio_clock_change_locked(ua, clock_config);')
    assert 'if (host_down && audio->connected)\n\t\tua_plugin_connected(ua);' in clk
    kext = fn('ua_audio.c', 'static int ua_audio_rate_change_kext(')
    assert before(kext, 'ua_plugin_teardown(ua, "rate change");',
                  'ret = ua_audio_clock_change_locked(ua, cfg);')
    # rebuilt after a failed change (old rate) and after a good one
    assert kext.count('ua_plugin_connected(ua);') == 2
    res = fn('ua_audio.c', 'int ua_audio_resume(')
    assert 'if (how != UA_RESUME_CONTROLS)\n\t\tua_plugin_teardown(ua, "resume");' in res


def test_quiesce_waits_off_lock_then_the_hardware_quiesce():
    q = fn('ua_core.c', 'static void ua_quiesce_device(')
    assert before(q, 'ua_plugin_host_quiesce(ua, why,', 'mutex_lock(&ua->lock);')
    rm = fn('ua_core.c', 'static void ua_remove(')
    assert before(rm, 'ua->removed = true;', 'ua_plugin_host_quiesce(ua, "remove",')
    assert before(rm, 'ua_plugin_host_quiesce(ua, "remove",', 'ua_hw_quiesce_locked(ua);')
    hw = fn('ua_core.c', 'static void ua_hw_quiesce_locked(')
    assert before(hw, 'ua_plugin_teardown(ua, "quiesce");', 'ua_dsp_rings_quiesce(ua);')
    aer = fn('ua_core.c', 'static pci_ers_result_t ua_error_detected(')
    assert before(aer, 'atomic_set(&ua->shutdown, 1);',
                  'ua_plugin_device_lost(ua, "a PCIe error");')


def test_connect_reads_maps_and_arms():
    audio = csrc.source('ua_audio.c')
    assert audio.count('ua_plugin_connected(ua);') == 4  # connect, clock cfg, kext x2
    host = fn('ua_plugin_host.c', 'void ua_plugin_connected(')
    assert before(host, 'ua_plugin_read_geometry(ua, i);', 'ua_strips_connected(ua);')
    svc = fn('ua_audio.c', 'static void ua_dsp_service_handler(')
    assert 'ua_plugin_service(ua);' in svc


def test_host_waits_never_hold_the_device_lock():
    host = csrc.source('ua_plugin_host.c')
    fence = csrc.extract(host, 'int ua_plugin_fence_wait(')
    assert before(fence, 'mutex_unlock(&ua->lock);', 'ua_xact_wait(ua, x, UA_XACT_WAIT_MS);')
    quiesce = csrc.extract(host, 'void ua_plugin_host_quiesce(')
    assert before(quiesce, 'cancel_delayed_work_sync(&h->work);', 'mutex_lock(&h->mutex);')
    # the fence waits come after the lock is dropped
    tail = quiesce[quiesce.index('ua_plugin_teardown(ua, why);'):]
    assert before(tail, 'mutex_unlock(&ua->lock);', 'ua_plugin_fence_wait(ua, i);')
    # teardown only publishes
    td = csrc.extract(host, 'void ua_plugin_teardown(')
    assert 'wait' not in td and 'sleep' not in td
