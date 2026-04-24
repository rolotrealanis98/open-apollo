<!--
  Monitor section — stereo meter, monitor knob, dim/talkback controls.
  Same width as channel strips.
-->
<script>
  import Knob from "./Knob.svelte";
  import ToggleButton from "./ToggleButton.svelte";
  import StereoMeter from "./StereoMeter.svelte";
  import { getValue, setValue } from "./device-store.svelte.js";

  const MON = "/devices/0/outputs/18";

  // ── Daemon-backed state (read) ──────────────────────────
  let monVol = $derived(getValue(`${MON}/CRMonitorLevelTapered/value`) ?? 0);
  let mute = $derived(getValue(`${MON}/Mute/value`) ?? false);
  let mono = $derived(getValue(`${MON}/MixToMono/value`) ?? false);
  let dim = $derived(getValue(`${MON}/DimOn/value`) ?? false);
  let talkback = $state(false);  // not yet implemented in daemon

  // ── Meters from daemon subscriptions ────────────────────
  let meterL = $derived(getValue(`${MON}/meters/0/MeterLevel/value`) ?? -77);
  let meterR = $derived(getValue(`${MON}/meters/1/MeterLevel/value`) ?? -77);
  let peakL = $derived(getValue(`${MON}/meters/0/MeterPeakLevel/value`) ?? -77);
  let peakR = $derived(getValue(`${MON}/meters/1/MeterPeakLevel/value`) ?? -77);

  // Normalize meters from dBFS (-77..0) to 0..1 for the meter component
  let meterLNorm = $derived(meterL <= -77 ? 0 : Math.pow(10, meterL / 20));
  let meterRNorm = $derived(meterR <= -77 ? 0 : Math.pow(10, meterR / 20));
  let peakLNorm = $derived(peakL <= -77 ? 0 : Math.pow(10, peakL / 20));
  let peakRNorm = $derived(peakR <= -77 ? 0 : Math.pow(10, peakR / 20));

  // dB readout from the raw dB value
  let monDb = $derived.by(() => {
    const db = getValue(`${MON}/CRMonitorLevel/value`);
    if (db === undefined || db === null || db <= -96) return "−∞ dB";
    return db.toFixed(1) + " dB";
  });

  // ── Write helpers ───────────────────────────────────────
  function setMonVol(v) { setValue(`${MON}/CRMonitorLevelTapered/value`, v); }
  function setMute(v) { setValue(`${MON}/Mute/value`, v); }
  function setMono(v) { setValue(`${MON}/MixToMono/value`, v); }
  function setDim(v) { setValue(`${MON}/DimOn/value`, v); }
</script>

<div class="monitor-strip">
  <!-- Tall stereo meter -->
  <StereoMeter levelL={meterLNorm} levelR={meterRNorm} peakL={peakLNorm} peakR={peakRNorm} height={250} wide />

  <!-- Output controls -->
  <div class="control-section">
    <div class="section-label">OUTPUT</div>
    <div class="btn-row">
      <ToggleButton label="MONO" color="gray" active={mono} onclick={(v) => setMono(v)} />
      <ToggleButton label="MUTE" color="red"  active={mute} onclick={(v) => setMute(v)} />
    </div>
    <div class="btn-row">
      <ToggleButton label="DIM" color="amber" active={dim} onclick={(v) => setDim(v)} />
      <ToggleButton label="TB"  color="green" bind:active={talkback} />
    </div>
  </div>

  <!-- Monitor knob + readout -->
  <div class="mon-section">
    <div class="section-label">MONITOR</div>
    <Knob label="" min={0} max={1} unit="" color="green" value={monVol} onchange={(v) => setMonVol(v)} size={72} />
    <div class="mon-readout">{monDb}</div>
  </div>
</div>

<style>
  .monitor-strip {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-sm);
    padding: var(--sp-lg) var(--sp-md);
    background: var(--bg-surface);
    border-radius: var(--radius-md);
    border: 1px solid var(--bezel-dark);
    width: 130px;
    align-self: flex-end;
  }

  .control-section, .mon-section {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-xs);
    width: 100%;
  }

  .mon-section {
    padding-top: var(--sp-xs);
    border-top: 1px solid var(--bezel-dark);
  }

  .section-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: var(--font-tiny);
    font-weight: 700;
    letter-spacing: 1.5px;
  }

  .btn-row {
    display: flex;
    gap: var(--sp-xs);
    width: 100%;
  }
  .btn-row :global(.toggle) {
    flex: 1;
    width: auto;
  }

  /* ── Dim level control ───────────────────────────────────── */
  .dim-control {
    display: flex;
    align-items: center;
    gap: var(--sp-xs);
    padding: 2px var(--sp-sm);
    background: var(--bg-recessed);
    border-radius: var(--radius-xs);
    border: 1px solid var(--bezel-dark);
    width: 100%;
    justify-content: center;
  }

  .dim-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: 6px;
    font-weight: 700;
    letter-spacing: 0.5px;
  }

  .mon-readout {
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
</style>
