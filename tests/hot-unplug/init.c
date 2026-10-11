// SPDX-License-Identifier: GPL-2.0-only
/*
 * Guest PID 1 for test-hot-unplug-pcm-open.sh.
 *
 * Binds ua_apollo to an emulated PCI device, holds the ALSA PCM and
 * /dev/ua_apolloN open while the device is removed (as PipeWire and the
 * mixer daemon do on a Thunderbolt unplug), uses and closes them, then
 * asks kmemleak whether anything the driver allocated was never freed.
 * Progress goes to the kernel log as "UA-TEST:" lines; the host reads
 * the serial console and looks for KASAN reports.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "../../driver/ua_ioctl.h"

static void say(const char *fmt, ...)
{
	char buf[256];
	va_list ap;
	int fd, n;

	n = snprintf(buf, sizeof(buf), "UA-TEST: ");
	va_start(ap, fmt);
	vsnprintf(buf + n, sizeof(buf) - n, fmt, ap);
	va_end(ap);

	fd = open("/dev/kmsg", O_WRONLY);
	if (fd >= 0) {
		write(fd, buf, strlen(buf));
		close(fd);
	}
}

static void finish(const char *result)
{
	say("result %s", result);
	sync();
	reboot(RB_POWER_OFF);
	exit(0);
}

static int write_file(const char *path, const char *s)
{
	int fd = open(path, O_WRONLY);
	int ok;

	if (fd < 0)
		return -1;
	ok = write(fd, s, strlen(s)) == (ssize_t)strlen(s);
	close(fd);
	return ok ? 0 : -1;
}

/*
 * QEMU secondary-vga (1234:1111, BAR0 is 16 MiB of plain RAM) on the only
 * PCIe root port the test script creates, so its address is fixed.
 */
#define FIXTURE "0000:01:00.0"

static void insmod(const char *path)
{
	int fd = open(path, O_RDONLY);

	if (fd < 0 || syscall(SYS_finit_module, fd, "", 0)) {
		say("insmod %s failed", path);
		finish("SETUP-FAIL insmod");
	}
	close(fd);
}

static int find_card(void)
{
	char line[256];
	FILE *f = fopen("/proc/asound/cards", "r");
	int card = -1;

	if (!f)
		return -1;
	while (fgets(line, sizeof(line), f)) {
		if (strstr(line, "[ua_apollo") || strstr(line, "ua_apollo -")) {
			card = atoi(line);
			break;
		}
	}
	fclose(f);
	return card;
}

/* Count kmemleak reports whose allocation backtrace is in ua_apollo */
static void kmemleak_check(void)
{
	char line[512];
	int total = 0, ours = 0, in_ours = 0;
	FILE *f;

	write_file("/sys/kernel/debug/kmemleak", "scan");
	write_file("/sys/kernel/debug/kmemleak", "scan");
	f = fopen("/sys/kernel/debug/kmemleak", "r");
	if (!f) {
		say("kmemleak unavailable");
		return;
	}
	while (fgets(line, sizeof(line), f)) {
		if (strstr(line, "unreferenced object")) {
			total++;
			in_ours = 0;
		} else if (!in_ours && strstr(line, "[ua_apollo]")) {
			ours++;
			in_ours = 1;
		}
	}
	fclose(f);
	say("kmemleak: %d unreferenced, %d from ua_apollo", total, ours);
}

int main(void)
{
	struct ua_device_info info;
	char path[256], link[256];
	int card, play, rec, ctl, err;
	ssize_t n;

	mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
	mount("proc", "/proc", "proc", 0, NULL);
	mount("sysfs", "/sys", "sysfs", 0, NULL);
	mount("debugfs", "/sys/kernel/debug", "debugfs", 0, NULL);

	insmod("/ua_fixture.ko");
	insmod("/ua_apollo.ko");

	write_file("/sys/bus/pci/devices/" FIXTURE "/driver_override", "ua_apollo");
	write_file("/sys/bus/pci/drivers_probe", FIXTURE);

	n = readlink("/sys/bus/pci/devices/" FIXTURE "/driver", link,
		     sizeof(link) - 1);
	if (n <= 0)
		finish("SETUP-FAIL driver did not bind");
	link[n] = '\0';
	say("bound " FIXTURE " -> %s", strrchr(link, '/') + 1);

	card = find_card();
	if (card < 0)
		finish("SETUP-FAIL no ua_apollo ALSA card");

	snprintf(path, sizeof(path), "/dev/snd/pcmC%dD0p", card);
	play = open(path, O_RDWR | O_NONBLOCK);
	snprintf(path, sizeof(path), "/dev/snd/pcmC%dD0c", card);
	rec = open(path, O_RDONLY | O_NONBLOCK);
	if (play < 0 || rec < 0)
		finish("SETUP-FAIL PCM open");
	say("card %d playback and capture open", card);

	ctl = open("/dev/ua_apollo0", O_RDWR);
	if (ctl < 0 || ioctl(ctl, UA_IOCTL_GET_DEVICE_INFO, &info))
		finish("SETUP-FAIL /dev/ua_apollo0");
	say("/dev/ua_apollo0 open");

	/* Same teardown pciehp runs on a Thunderbolt unplug */
	if (write_file("/sys/bus/pci/devices/" FIXTURE "/remove", "1"))
		finish("SETUP-FAIL remove");
	say("device removed with PCM still open");

	sleep(1);
	err = ioctl(ctl, UA_IOCTL_GET_DEVICE_INFO, &info) ? errno : 0;
	say("ioctl after removal: %s", err ? strerror(err) : "succeeded");
	close(ctl);
	close(play);
	close(rec);
	say("PCM and /dev/ua_apollo0 closed after removal");

	/* kmemleak ignores objects younger than 5 s */
	sleep(6);
	kmemleak_check();

	finish("COMPLETED");
	return 0;
}
