/*
 * apollo-selectors.d — capture UA driver ioctl payloads on macOS
 *
 * Traces IOConnectCallStructMethod in the UA Mixer Engine process and dumps
 * the raw input struct (and the reply, where there is one) for the selectors
 * that carry the data we need to write a routing config on Linux.
 *
 * READ-ONLY. This attaches to an existing userspace process and copies buffers
 * it was already passing to the kernel. It issues no ioctls and writes nothing
 * to the hardware.
 *
 * Usage (normally invoked by capture.sh, but standalone works):
 *
 *   sudo dtrace -s apollo-selectors.d -p $(pgrep -f "UA Mixer Engine")
 *
 * Requires SIP disabled. See WHAT-THIS-DOES.md.
 *
 * ---------------------------------------------------------------------------
 * IOConnectCallStructMethod(io_connect_t connection,      -> arg0
 *                           uint32_t     selector,        -> arg1
 *                           const void  *inputStruct,     -> arg2
 *                           size_t       inputStructCnt,  -> arg3
 *                           void        *outputStruct,    -> arg4
 *                           size_t      *outputStructCnt) -> arg5
 * ---------------------------------------------------------------------------
 *
 * Selectors traced, and why:
 *
 * Observed payload sizes are from a 26k-record x8p capture, and drive the
 * bucket sizes below:
 *
 *   171  GetRoutingTable   1552 B in / 1648 B out. 16-B header {u32 unused,
 *                          u32 direction, u32 unused, u32 num_channels} +
 *                          num_channels x 48-B entries {u32 session_id,
 *                          u32 pad, u32 channel_id, u32 type, char name[32]}.
 *                          32 input / 34 output entries on an x8p.
 *   113  2800 B. Large, contents not yet identified. Truncated in the
 *   129  2800 B. earlier capture -- these two are the outstanding unknowns,
 *                          and the likeliest home of the two 72-word IO
 *                          descriptor blocks (input -> SRAM 0xC1A4,
 *                          output -> 0xC2C4). First word is the length.
 *   130  136 B. Per-channel program burst. Fully captured already, but it
 *   131  24 B.  carries userspace pointers rather than descriptor payload.
 *   189  144 B. Stream format (channel count + type per direction).
 *   102  48 B.  Transport commit (client name, rate, buffer size).
 *   100  4 B.   DSP image selector.
 *   127  16 B.  Firmware block descriptor -- the block itself is DMA'd from
 *               host memory, so only the descriptor crosses this call.
 *
 * Sizing notes -- these matter, and getting them wrong is why the earlier
 * capture came back short:
 *
 *   tracemem()'s length must be a compile-time constant, so we copyin the
 *   actual arg3 bytes and then trace a fixed bucket size from that scratch
 *   buffer. Bytes past arg3 in each dump are uninitialised scratch -- ignore
 *   them and trust the "size=" field in the header line, which is the real
 *   payload length.
 *
 *   The copyin needs scratch space at least as large as the bucket. macOS
 *   gives no way to raise that (see the pragma block below), so DUMP_LARGE is
 *   3072 -- above the 2800-byte payloads we actually need, and not far above
 *   the 2048 that is known to work on this hardware. If scratch still runs
 *   out, the dtrace:::ERROR clause below reports fault 5 rather than letting
 *   records vanish.
 *
 *   Two buckets rather than one: SEL130 alone fires ~860 times per session,
 *   so dumping every record at the large size would bury the interesting
 *   2800-byte records in tens of MB of padding.
 */

#pragma D option quiet
#pragma D option bufsize=16m
#pragma D option dynvarsize=16m
#pragma D option switchrate=10hz

/*
 * No scratchsize option here on purpose. It exists on illumos/Solaris DTrace
 * but Apple's DTrace never picked it up -- setting it is a hard compile error
 * ("scratchsize is not a valid option"), not a warning. Scratch space is a
 * fixed per-CPU size on macOS and cannot be tuned from the script, which is
 * why DUMP_LARGE is kept only just above the largest payload we need rather
 * than rounded up to something comfortable.
 *
 * bufsize is per-CPU, so 16m on a 10-core machine is already a 160 MB
 * allocation. With switchrate draining to the output file 10x a second there
 * is no reason to go higher, and a too-large request can simply fail.
 */

/*
 * Deliberately no cpp macro for the selector list: #define would require
 * `dtrace -C`, and this script is meant to also work as a bare
 * `dtrace -s apollo-selectors.d`. The list is written out in the two clauses
 * that need it; the rest key off self->sel.
 */
inline int DUMP_SMALL = 256;
inline int DUMP_LARGE = 3072;

dtrace:::BEGIN
{
	nerrors = 0;
	printf("# open-apollo macOS selector capture\n");
	printf("# Each record: '=== SELn size=N' followed by a tracemem hexdump.\n");
	printf("# Only the first 'size' bytes of each dump are real payload.\n");
	printf("#\n");
}

/*
 * Surface faults instead of losing records silently. The one that matters
 * here is fault 5 (DTRACEFLT_NOSCRATCH) -- copyin had nowhere to land, which
 * on macOS can only be fixed by lowering DUMP_LARGE, since scratch space is
 * not tunable. Fault 1 (BADADDR) on a copyin usually just means the buffer
 * was unmapped at that instant and is harmless if rare.
 */
dtrace:::ERROR
{
	nerrors = nerrors + 1;
	printf("\n### DTRACE ERROR fault=%d addr=0x%x\n", arg4, arg5);
}

/*
 * Entry: dump the input struct.
 *
 * The size guard is deliberate -- copyin(ptr, 0) is an error, and an
 * absurd length means we misread the frame and should skip rather than
 * fault the probe.
 */
pid$target::IOConnectCallStructMethod:entry
/(arg1 == 171 || arg1 == 130 || arg1 == 131 || arg1 == 113 || arg1 == 129 ||
  arg1 == 100 || arg1 == 127 || arg1 == 189 || arg1 == 102) &&
 arg2 != 0 && arg3 > 0 && arg3 <= DUMP_LARGE/
{
	self->sel  = arg1;
	self->obuf = arg4;
	self->ocnt = arg5;

	printf("\n=== SEL%d dir=in size=%d ts=%d\n", arg1, arg3, timestamp);
}

pid$target::IOConnectCallStructMethod:entry
/self->sel && arg3 <= DUMP_SMALL/
{
	tracemem(copyin(arg2, arg3), DUMP_SMALL);
}

pid$target::IOConnectCallStructMethod:entry
/self->sel && arg3 > DUMP_SMALL/
{
	tracemem(copyin(arg2, arg3), DUMP_LARGE);
}

/*
 * A payload larger than DUMP_LARGE would be silently cut, which is exactly
 * the failure we are trying to stop repeating. Record it loudly instead.
 */
pid$target::IOConnectCallStructMethod:entry
/(arg1 == 171 || arg1 == 130 || arg1 == 131 || arg1 == 113 || arg1 == 129 ||
  arg1 == 100 || arg1 == 127 || arg1 == 189 || arg1 == 102) &&
 arg3 > DUMP_LARGE/
{
	printf("\n=== SEL%d dir=in size=%d OVERSIZE-NOT-DUMPED ts=%d\n",
	    arg1, arg3, timestamp);
	printf("# raise DUMP_LARGE above %d and re-run to capture this one\n",
	    arg3);
}

/* Return: resolve the reply length, which is behind a size_t pointer. */
pid$target::IOConnectCallStructMethod:return
/self->sel && self->ocnt != 0/
{
	self->olen = *(uint64_t *)copyin(self->ocnt, sizeof(uint64_t));
}

pid$target::IOConnectCallStructMethod:return
/self->sel/
{
	printf("\n=== SEL%d dir=out size=%d rc=0x%x ts=%d\n",
	    self->sel, self->olen, arg1, timestamp);
}

pid$target::IOConnectCallStructMethod:return
/self->sel && self->obuf != 0 && self->olen > 0 && self->olen <= DUMP_SMALL/
{
	tracemem(copyin(self->obuf, self->olen), DUMP_SMALL);
}

pid$target::IOConnectCallStructMethod:return
/self->sel && self->obuf != 0 && self->olen > DUMP_SMALL &&
 self->olen <= DUMP_LARGE/
{
	tracemem(copyin(self->obuf, self->olen), DUMP_LARGE);
}

pid$target::IOConnectCallStructMethod:return
/self->sel/
{
	self->sel  = 0;
	self->obuf = 0;
	self->ocnt = 0;
	self->olen = 0;
}

dtrace:::END
{
	printf("\n# capture ended, %d dtrace errors\n", nerrors);
}
