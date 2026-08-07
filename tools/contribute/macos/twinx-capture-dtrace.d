/*
 * twinx-capture-dtrace.d — full routing / program capture for Apollo Twin X.
 *
 * RUN twinx-dtrace-smoke.d FIRST. This script is worthless if the selectors
 * never fire, and the smoke test answers that in 20 seconds.
 *
 * Captures the payloads behind the selectors the Linux driver needs and does
 * not have for this model:
 *
 *   SEL171  routing tables (RT data)      -> ua_twinx_rec_routing[],
 *                                            ua_twinx_play_routing[],
 *                                            io_desc_input/output
 *   SEL127  DSP program blocks            -> Twin X equivalent of
 *                                            ua_x4_dsp0_programs[]
 *   SEL189  mixer settings                -> mixer setting layout
 *   SEL131  SetMixerParam                 -> control mapping
 *   SEL130  SetMixerBusParam              -> bus param mapping
 *   SEL163  SetClockMode                  -> clock source values
 *
 * KEY DIFFERENCE FROM UPSTREAM'S TEMPLATE: this traces both :entry (input
 * struct) and :return (output struct). Upstream's example only does
 * copyin(arg2, arg3) on entry. If a selector is a getter, its data comes back
 * in outputStruct and entry-only tracing captures nothing. Pointers are stashed
 * in thread-locals on entry and read back on return.
 *
 * IOConnectCallStructMethod(connection, selector, inputStruct, inputStructCnt,
 *                           outputStruct, outputStructCnt*)
 *   arg0 connection   arg1 selector      arg2 inputStruct
 *   arg3 inputStructCnt                  arg4 outputStruct
 *   arg5 outputStructCnt (pointer to size_t, in/out)
 *
 * Read-only. Observes calls UA's own software is already making. No writes, no
 * injection, no replay, no network.
 *
 * Requires SIP disabled.
 *
 * Usage:
 *   sudo dtrace -q -s twinx-capture-dtrace.d \
 *        -p $(pgrep -f "UA Mixer Engine" | head -1) \
 *        -o twinx-dtrace-capture.txt
 *
 * While it runs, drive UA Console so it re-sends configuration:
 *   1. change the sample rate (this usually forces a full routing re-send)
 *   2. toggle a Unison preamp: +48V, PAD, low cut, phase
 *   3. change monitor level, then mute and unmute
 *   4. start playback, then stop it
 *   5. unplug and replug the Thunderbolt cable if you are willing — cold init
 *      is when the largest payloads go across
 *
 * Step 1 and step 5 matter most. Routing tables are typically sent once at
 * device init, not continuously.
 */

#pragma D option quiet
#pragma D option bufsize=64m
#pragma D option dynvarsize=32m
#pragma D option strsize=256
/* Do not silently drop payloads; a truncated table is worse than a loud error. */
#pragma D option bufpolicy=fill

dtrace:::BEGIN
{
	printf("=== Apollo Twin X routing capture ===\n");
	printf("Exercise UA Console: change sample rate, toggle preamp\n");
	printf("switches, start/stop playback, replug Thunderbolt.\n");
	printf("Ctrl+C when done.\n\n");
}

/*
 * Entry: stash everything we will need on return, and dump the input struct
 * for the selectors we care about.
 *
 * Sizes are bounded before any copyin: a bogus length from a struct we do not
 * fully understand should abort the probe, not attempt a huge copy.
 */
pid$target::IOConnectCallStructMethod:entry
{
	self->sel    = arg1;
	self->inp    = arg2;
	self->insz   = arg3;
	self->outp   = arg4;
	self->outszp = arg5;

	self->want = (arg1 == 171 || arg1 == 127 || arg1 == 189 ||
		      arg1 == 131 || arg1 == 130 || arg1 == 163 ||
		      arg1 == 132 || arg1 == 119) ? 1 : 0;
}

pid$target::IOConnectCallStructMethod:entry
/self->want && self->insz > 0 && self->insz <= 65536/
{
	printf("\n########## SEL%d ENTRY  insize=%d  outptr=%s ##########\n",
	       self->sel, self->insz, self->outp != 0 ? "yes" : "no");
	tracemem(copyin(self->inp, self->insz), 65536, self->insz);
}

pid$target::IOConnectCallStructMethod:entry
/self->want && self->insz > 65536/
{
	printf("\n########## SEL%d ENTRY  insize=%d (TOO LARGE, not dumped) ##########\n",
	       self->sel, self->insz);
}

/*
 * Return: read the output size the kernel wrote back, then dump the output
 * struct. This is where a getter's payload actually lives — SEL171 included,
 * if it turns out to be a getter.
 */
pid$target::IOConnectCallStructMethod:return
/self->want && self->outp != 0 && self->outszp != 0/
{
	this->osz = *(size_t *)copyin(self->outszp, sizeof(size_t));

	printf("\n########## SEL%d RETURN ret=0x%x outsize=%d ##########\n",
	       self->sel, (int)arg1, this->osz);
}

pid$target::IOConnectCallStructMethod:return
/self->want && self->outp != 0 && self->outszp != 0 &&
 (this->osz = *(size_t *)copyin(self->outszp, sizeof(size_t))) > 0 &&
 this->osz <= 65536/
{
	tracemem(copyin(self->outp, this->osz), 65536, this->osz);
}

pid$target::IOConnectCallStructMethod:return
/self->want/
{
	self->sel = 0; self->inp = 0; self->insz = 0;
	self->outp = 0; self->outszp = 0; self->want = 0;
}

/*
 * Scalar/struct variant, in case some selectors travel this way. Smoke test
 * output tells you whether these fire; if they do not, this block costs nothing.
 */
pid$target::IOConnectCallMethod:entry
/arg1 == 171 || arg1 == 127 || arg1 == 189/
{
	printf("\n########## SEL%d via IOConnectCallMethod ##########\n", arg1);
}

dtrace:::END
{
	printf("\n=== capture ended ===\n");
	printf("Send the output file back for conversion into ua_routing.h\n");
	printf("tables. If SEL171 never appeared, note that explicitly — it is\n");
	printf("a real finding, not a failed run.\n");
	printf("\nRE-ENABLE SIP NOW:\n");
	printf("  Recovery Mode -> Terminal -> csrutil enable -> restart\n");
}
