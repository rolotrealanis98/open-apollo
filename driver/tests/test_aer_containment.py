"""PCIe error handling keeps driver-v2's recovery design: BAR0 is closed
before the software that touches it is stopped, and the slot-reset
bring-up pulls the DSP rings back before bus mastering returns.

Codex's version fails closed (DISCONNECT on every error, no slot reset);
driver-v2 recovers through the warm path or a deferred cold re-init (see
ua_slot_reset()), so this checks the ordering that makes that recovery
safe instead.  Rewritten from the parallel Codex work
(test_aer_fail_closed).
"""
import csrc


def test_error_detected_order():
    core = csrc.source('ua_core.c')
    err = csrc.extract(core, 'static pci_ers_result_t ua_error_detected(')
    steps = ['atomic_set(&ua->shutdown, 1);',
             'cancel_work_sync(&ua->resume_work);',
             'ua_dsp_service_stop(ua);',
             'ua_irq_mask_all(ua, false);']
    at = [err.index(s) for s in steps]
    assert at == sorted(at)
    assert 'PCI_ERS_RESULT_DISCONNECT' in err   # permanent failure


def test_slot_reset_neutralizes_before_bus_mastering():
    core = csrc.source('ua_core.c')
    rst = csrc.extract(core, 'static pci_ers_result_t ua_slot_reset(')
    steps = ['pcim_enable_device(pdev);',
             'ioread32(ua->regs + UA_REG_FPGA_REV);',
             'mutex_lock(&ua->lock);',
             'ua_hw_stop_xport_irqs(ua);',
             'ua_dsp_rings_neutralize(ua);',
             'pci_set_master(pdev);']
    at = [rst.index(s) for s in steps]
    assert at == sorted(at)
    # A device that did not come back is disconnected, not re-enabled
    assert rst.index('return PCI_ERS_RESULT_DISCONNECT;') < \
        rst.index('pci_set_master(pdev);')
