#!/usr/bin/env python3
"""Compile the real recovery callback with hardware stubs; no device access."""
from pathlib import Path
import os
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = Path(os.environ.get("APOLLO_CORE_SOURCE", root / "driver/ua_core.c")).read_text()
callback = source.split("static pci_ers_result_t ua_slot_reset", 1)[1].split("static void ua_io_resume", 1)[0]
stubs = r'''
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <assert.h>
typedef int pci_ers_result_t;
#define PCI_ERS_RESULT_RECOVERED 1
#define PCI_ERS_RESULT_DISCONNECT 2
#define UA_REG_FPGA_REV 0
#define dev_info(...) ((void)0)
#define dev_err(...) ((void)0)
struct pci_dev { int dev; };
struct ua_device { bool probe_minimal; int shutdown; uint32_t fpga_rev; void *regs; };
static struct ua_device ua;
static int enable_result, program_result, enabled, reads, masters, programmed, detected;
static uint32_t revision;
static void *pci_get_drvdata(struct pci_dev *p) { return &ua; }
static int pcim_enable_device(struct pci_dev *p) { enabled++; return enable_result; }
static void pci_set_master(struct pci_dev *p) { masters++; }
static uint32_t ioread32(void *p) { reads++; return revision; }
static uint32_t ua_read(struct ua_device *u, unsigned off) {
    return u->shutdown ? UINT32_MAX : ioread32(u->regs);
}
static void atomic_set(int *p, int value) { *p = value; }
static void ua_detect_capabilities(struct ua_device *u) { detected++; }
static int ua_program_registers(struct ua_device *u) {
    assert(!u->shutdown); programmed++; return program_result;
}
'''
checks = r'''
int main(void) {
    struct pci_dev p = {0};
    for (int minimal = 0; minimal <= 1; minimal++) {
        for (int scenario = 0; scenario < 4; scenario++) {
            ua = (struct ua_device){ .probe_minimal = minimal, .shutdown = 1 };
            enable_result = scenario == 1 ? -1 : 0;
            revision = scenario == 2 ? UINT32_MAX : 0x1234;
            program_result = scenario == 3 ? -1 : 0;
            enabled = reads = masters = programmed = detected = 0;
            int result = ua_slot_reset(&p);
            int reachable = scenario != 1 && scenario != 2;
            int recovered = reachable && (minimal || scenario != 3);
            assert(result == (recovered ? PCI_ERS_RESULT_RECOVERED : PCI_ERS_RESULT_DISCONNECT));
            assert(enabled == 1);
            assert(reads == (scenario != 1));
            assert(masters == (!minimal && reachable));
            assert(programmed == (!minimal && reachable));
            assert(detected == (!minimal && reachable));
            assert(ua.shutdown == (minimal || !recovered));
        }
    }
    puts("PASS: 8 recovery cases cover missing link, enable failure, probe-only isolation and re-init failure");
}
'''
with tempfile.TemporaryDirectory() as tmp:
    harness = Path(tmp) / "reset.c"
    binary = Path(tmp) / "reset-test"
    harness.write_text(stubs + "static pci_ers_result_t ua_slot_reset" + callback + checks)
    subprocess.run(["cc", "-std=gnu11", "-o", str(binary), str(harness)], check=True)
    subprocess.run([str(binary)], check=True)
