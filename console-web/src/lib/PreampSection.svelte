<!--
  Preamp control section — real daemon-backed for Apollo x4.
  4 preamp channels. MIC/LINE toggle on inputs 1-2 only (hardware-gated).
  HiZ is hardware auto-detect (1/4" TRS plug on ch 0-1) — not surfaced here:
  daemon readback decoder doesn't publish bit 24, so any indicator would
  silently go stale. Add when daemon plumbing lands.
-->
<script>
  import Knob from "./Knob.svelte";
  import ToggleButton from "./ToggleButton.svelte";
  import { getValue, setValue } from "./device-store.svelte.js";

  const CHANNELS = [0, 1, 2, 3];
  // Apollo x4 hardware capability: ch 0-1 = Mic/Line switchable,
  // ch 2-3 = Line only. HiZ auto-detect works only on ch 0-1.
  const MIC_LINE_CAPABLE = new Set([0, 1]);

  // MIC/LINE, PAD, 48V write daemon state correctly but the ARM MCU does
  // not latch the physical relays yet — tracked in
  // plans/260424-0922-dsp-arm-relay-propagation/. Mark them "unverified" in
  // the UI so users aren't misled. Drop this once that plan ships.
  const RELAY_WARN = "Hardware relay not yet wired (DSP→ARM latch WIP, see plans/260424-0922-dsp-arm-relay-propagation/).";

  function preampPath(ch, prop) {
    return `/devices/0/inputs/${ch}/preamps/0/${prop}/value`;
  }
  function inputPath(ch, prop) {
    return `/devices/0/inputs/${ch}/${prop}/value`;
  }

  // Gain in dB (10..65). Knob is internally normalized 0..1 — convert at the boundary.
  const GAIN_MIN = 10;
  const GAIN_MAX = 65;
  function gainDbFor(ch) { return getValue(preampPath(ch, "Gain")) ?? GAIN_MIN; }
  function gainNormFor(ch) {
    const db = gainDbFor(ch);
    return Math.max(0, Math.min(1, (db - GAIN_MIN) / (GAIN_MAX - GAIN_MIN)));
  }
  function writeGainNorm(ch, norm) {
    const db = GAIN_MIN + norm * (GAIN_MAX - GAIN_MIN);
    setValue(preampPath(ch, "Gain"), db);
  }

  function v48For(ch)    { return getValue(preampPath(ch, "48V")) ?? false; }
  function padFor(ch)    { return getValue(preampPath(ch, "Pad")) ?? false; }
  function phaseFor(ch)  { return getValue(preampPath(ch, "Phase")) ?? false; }
  function lowCutFor(ch) { return getValue(preampPath(ch, "LowCut")) ?? false; }

  // IOType lives at /devices/0/inputs/{ch}/IOType — string "Mic"|"Line"|"Dante".
  // On ch 2-3 the device always reports "Line" and ignores writes.
  function ioTypeFor(ch) { return getValue(inputPath(ch, "IOType")) ?? "Mic"; }
  function isLine(ch)    { return String(ioTypeFor(ch)).toLowerCase() === "line"; }
  function writeIOType(ch, next) {
    // Only ch 0-1 are switchable; ch 2-3 daemon ignores but don't even send.
    if (!MIC_LINE_CAPABLE.has(ch)) return;
    setValue(inputPath(ch, "IOType"), next ? "Line" : "Mic");
  }

  function setPreamp(ch, prop, v) { setValue(preampPath(ch, prop), v); }
</script>

<div class="preamp-section">
  <div class="preamp-grid">
    {#each CHANNELS as ch}
      <div class="preamp-channel">
        <!-- Row 1: input-type toggle + Gain knob -->
        <div class="top-row">
          <div class="input-buttons">
            {#if MIC_LINE_CAPABLE.has(ch)}
              <ToggleButton
                label={isLine(ch) ? "LINE" : "MIC"}
                color={isLine(ch) ? "blue" : "green"}
                active={isLine(ch)}
                warn={RELAY_WARN}
                onclick={(v) => writeIOType(ch, v)}
              />
            {:else}
              <ToggleButton label="LINE" color="blue" active disabled />
            {/if}
          </div>
          <Knob
            min={GAIN_MIN} max={GAIN_MAX} unit="" color="green" size={52}
            value={gainNormFor(ch)}
            onchange={(v) => writeGainNorm(ch, v)}
          />
        </div>

        <!-- Row 2: dB readout -->
        <div class="gain-readout">{Math.round(gainDbFor(ch))} dB</div>

        <!-- Row 3: 48V + PAD -->
        <div class="btn-row">
          <ToggleButton label="48V" color="red" warn={RELAY_WARN}
            active={v48For(ch)} onclick={(v) => setPreamp(ch, "48V", v)} />
          <ToggleButton label="PAD" color="amber" warn={RELAY_WARN}
            active={padFor(ch)} onclick={(v) => setPreamp(ch, "Pad", v)} />
        </div>

        <!-- Row 4: Phase + Low Cut -->
        <div class="btn-row">
          <ToggleButton label="Ø" color="blue"
            active={phaseFor(ch)} onclick={(v) => setPreamp(ch, "Phase", v)} />
          <ToggleButton filter color="green"
            active={lowCutFor(ch)} onclick={(v) => setPreamp(ch, "LowCut", v)} />
        </div>
      </div>
    {/each}
  </div>
</div>

<style>
  .preamp-section { display: flex; flex-direction: column; gap: var(--sp-xs); }
  .preamp-grid { display: flex; gap: 2px; }
  .preamp-channel {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-sm);
    width: 130px;
    padding: var(--sp-md);
    background: var(--bg-surface);
    border-radius: var(--radius-md);
    border: 1px solid var(--bezel-dark);
  }
  .top-row {
    display: flex;
    align-items: center;
    gap: var(--sp-sm);
    width: 100%;
    justify-content: center;
  }
  .input-buttons { display: flex; flex-direction: column; gap: var(--sp-xs); }

  .gain-readout {
    background: var(--bg-recessed);
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-value);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: center;
    padding: 3px 0;
    border-radius: var(--radius-xs);
    border: 1px solid var(--bezel-dark);
    width: 100%;
  }
  .btn-row { display: flex; gap: var(--sp-xs); width: 100%; }
  .btn-row :global(.toggle) { flex: 1; width: auto; }
</style>
