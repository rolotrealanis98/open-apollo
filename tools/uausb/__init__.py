"""Shared code for the Apollo USB tools and the mixer daemon's USB backend.

  dspcmd    the DSP command language (the same one the PCIe command ring
            carries) and the USB bulk framing around it
  soloinit  the Apollo Solo USB DSP bring-up, built from that language
            instead of replayed (J1)
  device    vendor control requests: register map, DSP-ready poll (J3),
            info blocks (U36), the 40-word readback (J2)
  mixer     the 38 mixer settings as a one-shot mailbox and the
            SetMixerParam mapping of the PCIe driver, for USB

Nothing in here needs pyusb: the code that talks to a device takes any
object with pyusb's ctrl_transfer/read/write methods, so it runs against
a fake device in the tests.

Everything that touches hardware is UNTESTED ON HARDWARE; what each part
was checked against (captures, binaries, byte-exact comparisons) is in its
module docstring.
"""
