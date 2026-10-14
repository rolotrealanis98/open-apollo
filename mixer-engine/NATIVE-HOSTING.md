# Experimental native plug-in hosting

This private development path connects the daemon's existing insert transactions
to the kernel INSTANCE API. It requires an explicitly selected local provider and
preserves the running device's hardware settings:

```sh
python3 mixer-engine/ua_mixer_daemon.py \
  --native-plugin-provider /absolute/path/to/provider.py \
  --preserve-hardware --require-hardware
```

A provider is trusted Python code with `create_provider()`. It is never discovered
or loaded automatically. The product ships no vendor recipe, coefficient pages,
or plug-in binary. The kernel still requires the device's authorization answer.

## Current contract

- One configured mono input, effect slot zero, at the provider's exact sample rate.
- One live native lease; replacement and overlapping inserts are refused.
- CREATE starts the chain, then every expected coefficient is read back before
  publication. Both engine and helper trees receive the actual owned handle.
- Parameter plans carry writes, exact expected observations, formatted values,
  and proposed provider state. The provider commits its state only after delivery
  and readback. Empty writes still require observations.
- Meters use fresh scalar reads from `provider.remote_reads`; arbitrary locations,
  writes, and VendorSpecific opcodes are not exposed.
- DESTROY and owning-fd close restore the retained plain strip. Uncertain I/O
  invalidates the native session. Device/rate changes require a daemon restart;
  saved slot properties never recreate DSP leases.
- Bulk scene/preset/undo operations and clock/link/input-compensation changes are refused in this
  mode because they do not yet participate in the native transaction contract.
- Slot Power needs its own provider contract. An effect's Power parameter does
  not implicitly implement slot Power.

The provider interface is implemented in `plugin_native.py`: `reset`,
`create_arguments`, `initial`, `observations`, `snapshot`, `plan_control`,
`plan_restore`, and `accept`. `ControlPlan` and `LoadedPlugin` are typed boundaries.
Formatting and metadata remain provider responsibilities; the backend checks the
actual DSP observations and ownership before publishing them.

## Qualified development slice

Apollo x4, 48 kHz, 1176LN Legacy: live gain/control/meter operation, all seven
parameters, all five ratios, explicit unload, process exit, and stale-lease-free
restart have passed real hardware tests. Four analog inputs passed sequential
insert/remove tests; simultaneous multiple inserts and stereo have not.

The captured recipe's Mic Input Strip recording delay differs from the plain
strip's delay. Console input delay compensation is adjustable: Off, Short,
Medium, Medium-Long, and Long correspond to 0, 100, 200, 300, and 1000 samples
in the recovered settings table. The private bench provider aligns at Medium
(200); it does not implement runtime compensation changes or the warning when
a chain exceeds the chosen budget. MaxDelayComp writes are refused in native
mode until that shared strip/chain contract is implemented. Do not infer general delay-compensation policy, absolute latency, or
other-rate support from that single-path result.

GUI interaction, broader providers, automatic reconnect, and distribution-ready
recipe generation remain separate gates. See `HARDWARE-SESSIONS.md` for measured
results. Tests use synthetic driver I/O and contain no captured vendor pages.
