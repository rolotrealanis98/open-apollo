# SPDX-License-Identifier: GPL-2.0-only
#
# Universal Audio Apollo PCIe driver
#
# Out-of-tree build:
#   make -C /lib/modules/$(uname -r)/build M=$(pwd) modules
#
# Clean:
#   make -C /lib/modules/$(uname -r)/build M=$(pwd) clean

obj-m := ua_apollo.o
ua_apollo-y := ua_core.o ua_audio.o ua_dsp.o

# GCC 13.3 ICE workaround: try_forward_edges crash at -O1/-O2
# Both ua_core.c and ua_audio.c trigger cfgcleanup.cc:580 ICE.
# Use -Og for development; revisit when functions are split smaller.
# Clang does not have this bug, so only apply for GCC.
ifndef CONFIG_CC_IS_CLANG
CFLAGS_ua_core.o := -Og
CFLAGS_ua_audio.o := -Og
endif

KDIR ?= /lib/modules/$(shell uname -r)/build

all:
	$(MAKE) -C $(KDIR) M=$(CURDIR) modules

clean:
	$(MAKE) -C $(KDIR) M=$(CURDIR) clean

install:
	$(MAKE) -C $(KDIR) M=$(CURDIR) modules_install
	depmod -a

firmware-install:
	install -m 644 $(CURDIR)/../ua-apollo-mixer.bin /lib/firmware/ua-apollo-mixer.bin
	install -m 644 $(CURDIR)/../configs/firmware/ua-apollo-plugin-chain.bin /lib/firmware/ua-apollo-plugin-chain.bin

.PHONY: all clean install firmware-install
