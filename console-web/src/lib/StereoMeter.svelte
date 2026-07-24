<!--
  Stereo LED meter — two bars (L/R) with shared clip indicator and peak readout.

  Usage:
    <StereoMeter levelL={0.6} levelR={0.5} peakL={0.8} peakR={0.7} />
-->
<script>
  let {
    levelL = 0,
    levelR = 0,
    peakL = 0,
    peakR = 0,
    height = 160,
    wide = false,   // wider bars for monitor column
  } = $props();

  import Meter from "./Meter.svelte";
  import { METER_TICKS } from "./meter-scale.js";

  const barWidth = $derived(wide ? 22 : 10);

  // Sparser subset of METER_TICKS for the narrower shared scale column.
  const DISPLAY_DBS = [0, 6, 12, 18, 27, 46];
  const TICKS = METER_TICKS.filter((t) => DISPLAY_DBS.includes(t.db));

  // Track highest peak (true peak hold, click to reset)
  let heldPeakL = $state(0);
  let heldPeakR = $state(0);

  $effect(() => { if (peakL > heldPeakL) heldPeakL = peakL; });
  $effect(() => { if (peakR > heldPeakR) heldPeakR = peakR; });

  function resetPeak() {
    heldPeakL = 0;
    heldPeakR = 0;
  }

  // Show the louder channel's peak dB
  let peakDb = $derived.by(() => {
    const p = Math.max(heldPeakL, heldPeakR);
    if (p < 0.01) return "−∞";
    const db = 20 * Math.log10(p);
    if (db > -0.5) return "0.0";
    return db.toFixed(1);
  });
</script>

<div class="stereo-meter" class:wide style="--height: {height}px;">
  <div class="meter-row">
    <!-- Two composed meter bars (no per-instance clip dot / scale / readout) -->
    <div class="bars">
      <Meter level={levelL} peak={peakL} height={height} width={barWidth} showClip={false} showScale={false} />
      <Meter level={levelR} peak={peakR} height={height} width={barWidth} showClip={false} showScale={false} />
    </div>

    <!-- Shared dB scale -->
    <div class="scale">
      {#each TICKS as tick}
        <div class="tick" style="bottom: {tick.pct}%;">
          <span class="tick-line"></span>
          <span class="tick-label">{tick.db}</span>
        </div>
      {/each}
    </div>
  </div>

  <!-- L/R labels below the bars -->
  <div class="lr-labels">
    <span>L</span>
    <span>R</span>
  </div>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <span class="peak-readout" onclick={resetPeak} title="Click to reset peak">{peakDb}</span>
</div>

<style>
  .stereo-meter {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
  }

  .meter-row {
    display: flex;
    gap: 2px;
    align-items: stretch;
    height: var(--height);
    justify-content: center;
    /* Offset left to visually center the bars, not bars+scale */
    margin-right: -16px;
  }

  /* ── L/R labels (below bars) ─────────────────────────────── */
  .lr-labels {
    display: flex;
    gap: 2px;
    justify-content: center;
  }
  .lr-labels span {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: 7px;
    font-weight: 700;
    width: 10px;
    text-align: center;
  }

  /* ── Bars container ──────────────────────────────────────── */
  .bars {
    display: flex;
    gap: 2px;
  }

  /* ── Scale ───────────────────────────────────────────────── */
  .scale {
    position: relative;
    width: 14px;
    height: 100%;
  }

  .tick {
    position: absolute;
    left: 0;
    display: flex;
    align-items: center;
    gap: 1px;
    transform: translateY(50%);
  }

  .tick-line {
    display: block;
    width: 3px;
    height: 1px;
    background: var(--text-dimmed);
  }

  .tick-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: 6px;
    font-variant-numeric: tabular-nums;
  }

  /* ── Peak readout ────────────────────────────────────────── */
  .peak-readout {
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: 7px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    text-align: center;
    width: 32px;
    background: var(--bg-recessed);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
    padding: 3px 0;
    margin-top: 2px;
    cursor: pointer;
    overflow: hidden;
  }
  .peak-readout:hover {
    border-color: var(--text-dimmed);
  }

  /* ── Wide variant (monitor column) ─────────────────────── */
  .wide .meter-row { margin-right: -20px; }
  .wide .bars { gap: 4px; }
  .wide .lr-labels span { width: 22px; font-size: 8px; }
  .wide .lr-labels { gap: 4px; }
  .wide .peak-readout { width: 52px; font-size: 9px; padding: 4px 0; }
  .wide .scale { width: 20px; }
  .wide .tick-label { font-size: 8px; }
  .wide .tick-line { width: 5px; }
</style>
