// SPDX-License-Identifier: GPL-2.0-only
/*
 * Test fixture for test-hot-unplug-pcm-open.sh, guest only.
 *
 * QEMU's secondary-vga (1234:1111) has the RAM-backed BAR0 ua_apollo
 * needs, but no interrupt pin, so the driver's INTx fallback fails.
 * Give it a private interrupt line on a dummy chip.  The line never
 * fires; it only lets ua_probe() get past request_irq().
 */
#include <linux/irq.h>
#include <linux/irqdesc.h>
#include <linux/module.h>
#include <linux/pci.h>

static int __init ua_fixture_init(void)
{
	struct pci_dev *pdev = pci_get_device(0x1234, 0x1111, NULL);
	int irq;

	if (!pdev)
		return -ENODEV;

	irq = irq_alloc_desc(NUMA_NO_NODE);
	if (irq < 0) {
		pci_dev_put(pdev);
		return irq;
	}
	irq_set_chip_and_handler(irq, &dummy_irq_chip, handle_simple_irq);
	pdev->irq = irq;
	dev_info(&pdev->dev, "ua_fixture: dummy INTx line %d\n", irq);
	pci_dev_put(pdev);
	return 0;
}
module_init(ua_fixture_init);

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("ua_apollo hot-unplug test fixture");
