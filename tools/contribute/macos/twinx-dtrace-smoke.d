/*
 * twinx-dtrace-smoke.d — selector census for the UA mixer IOKit channel.
 *
 * PURPOSE: cheap proof that the DTrace approach works at all, BEFORE anyone
 * invests a long capture session (or trusts a large untested script) with SIP
 * disabled. It records nothing but a count per selector, so it is fast, tiny,
 * and reveals whether the interesting selectors actually fire on this macOS
 * version and driver build.
 *
 * WHY THIS EXISTS: upstream's capture.sh ships a placeholder where the probes
 * should be, and its example traces only :entry with copyin(arg2, arg3) — the
 * INPUT struct. If SEL171 is a getter, the routing table arrives in the OUTPUT
 * struct on return and that template would capture nothing useful. Establish
 * ground truth here first.
 *
 * Read-only: counts calls the UA software is already making. No writes, no
 * injection, no replay.
 *
 * Requires SIP disabled (DTrace restriction).
 *
 * Usage:
 *   # find the mixer process
 *   pgrep -fl "UA Mixer Engine|UAMixerEngine|UA Console|uaudio"
 *
 *   # census by PID, 20 seconds — click around UA Console while it runs,
 *   # change a preamp setting, start and stop playback
 *   sudo dtrace -q -s twinx-dtrace-smoke.d -p <PID>
 *
 *   # or trace every process calling into IOKit (noisier but finds the right one)
 *   sudo dtrace -q -s twinx-dtrace-smoke.d
 *
 * INTERPRETING THE RESULT:
 *   - SEL171 appears  -> routing capture is viable; move to the full script.
 *   - only :entry sizes are non-zero for 171 -> routing goes IN, not out.
 *   - only :return sizes are non-zero for 171 -> it is a getter, and the full
 *     script's return-side capture is the one that matters.
 *   - nothing at all   -> the symbol is not being hit. Try IOConnectCallMethod
 *     and IOConnectCallScalarMethod (also counted below), or the driver may use
 *     a DriverKit/userspace transport instead of a kext IOKit channel, in which
 *     case DTrace on these symbols is the wrong tool and we stop here rather
 *     than leaving SIP off.
 */

#pragma D option quiet
#pragma D option bufsize=8m
#pragma D option dynvarsize=8m

dtrace:::BEGIN
{
	printf("Apollo selector census — 20s. Exercise UA Console now.\n");
	printf("(change a preamp gain, toggle monitor, start/stop audio)\n\n");
}

/*
 * Struct variant: the one upstream's notes name. Signature is
 *   IOConnectCallStructMethod(connection, selector, inputStruct,
 *                             inputStructCnt, outputStruct, outputStructCnt*)
 * so arg1 is the selector and arg3 the input size.
 */
pid$target::IOConnectCallStructMethod:entry
{
	@struct_calls[arg1] = count();
	@struct_insize[arg1] = max(arg3);
	self->s = arg1;
}

pid$target::IOConnectCallStructMethod:return
/self->s/
{
	@struct_returns[self->s] = count();
	self->s = 0;
}

/* Scalar+struct variant — UA may use this instead for some selectors. */
pid$target::IOConnectCallMethod:entry
{
	@method_calls[arg1] = count();
}

pid$target::IOConnectCallScalarMethod:entry
{
	@scalar_calls[arg1] = count();
}

/* Async variant, for completeness. */
pid$target::IOConnectCallAsyncStructMethod:entry
{
	@async_calls[arg1] = count();
}

tick-20s
{
	exit(0);
}

dtrace:::END
{
	printf("\n=== IOConnectCallStructMethod: calls per selector ===\n");
	printf("(selectors of interest: 171=routing, 127=DSP programs,\n");
	printf(" 189=mixer settings, 131=SetMixerParam, 130=SetMixerBusParam,\n");
	printf(" 163=SetClockMode, 119=ProcessPlugin)\n\n");
	printa("  SEL%-6d calls=%@d\n", @struct_calls);

	printf("\n=== max INPUT struct size per selector ===\n");
	printa("  SEL%-6d insize_max=%@d\n", @struct_insize);

	printf("\n=== returns seen per selector ===\n");
	printa("  SEL%-6d returns=%@d\n", @struct_returns);

	printf("\n=== IOConnectCallMethod ===\n");
	printa("  SEL%-6d calls=%@d\n", @method_calls);

	printf("\n=== IOConnectCallScalarMethod ===\n");
	printa("  SEL%-6d calls=%@d\n", @scalar_calls);

	printf("\n=== IOConnectCallAsyncStructMethod ===\n");
	printa("  SEL%-6d calls=%@d\n", @async_calls);

	printf("\nIf SEL171 appears above, proceed to twinx-capture-dtrace.d.\n");
	printf("If nothing appears at all, stop and re-enable SIP — DTrace on\n");
	printf("these symbols is not the right tool for this driver build.\n");
}
