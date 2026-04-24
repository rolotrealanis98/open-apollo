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

  import { onMount } from "svelte";

  let clipActive = $derived(peakL > 0.97 || peakR > 0.97);

  // Wall-clock stable ballistics — dt-based exponential smoothing.
  let displayL = $state(0);
  let displayR = $state(0);
  const TAU_ATTACK_S = 0.012;
  const TAU_RELEASE_S = 0.170;

  onMount(() => {
    let prevL = 0, prevR = 0;
    let last = performance.now();
    let rafId = 0;
    const tick = (now) => {
      const dt = Math.max(0, (now - last) / 1000);
      last = now;

      const tauL = levelL > prevL ? TAU_ATTACK_S : TAU_RELEASE_S;
      const aL = 1 - Math.exp(-dt / tauL);
      prevL += (levelL - prevL) * aL;
      if (prevL < 1e-4) prevL = 0;
      displayL = prevL;

      const tauR = levelR > prevR ? TAU_ATTACK_S : TAU_RELEASE_S;
      const aR = 1 - Math.exp(-dt / tauR);
      prevR += (levelR - prevR) * aR;
      if (prevR < 1e-4) prevR = 0;
      displayR = prevR;

      rafId = requestAnimationFrame(tick);
    };
    rafId = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafId);
  });

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

  // dB scale ticks. Same non-linear pro-meter scale as Meter.svelte.
  const TICKS = [
    { db: "0",  pct: 100 },
    { db: "6",  pct: 85 },
    { db: "12", pct: 69 },
    { db: "18", pct: 51 },
    { db: "27", pct: 32 },
    { db: "46", pct: 10 },
  ];

  // Full tick set for accurate interpolation (matches Meter.svelte).
  const MAP_TICKS = [
    { db: 0, pct: 100 }, { db: 3, pct: 93 }, { db: 6, pct: 85 },
    { db: 9, pct: 77 }, { db: 12, pct: 69 }, { db: 15, pct: 60 },
    { db: 18, pct: 51 }, { db: 21, pct: 42 }, { db: 27, pct: 32 },
    { db: 36, pct: 20 }, { db: 46, pct: 10 }, { db: 60, pct: 0 },
  ];
  function linearToScalePct(linear) {
    if (linear <= 0) return 0;
    const db = -(20 * Math.log10(Math.max(linear, 1e-6)));
    if (db <= 0) return 100;
    for (let i = 1; i < MAP_TICKS.length; i++) {
      if (db <= MAP_TICKS[i].db) {
        const t = (db - MAP_TICKS[i - 1].db) / (MAP_TICKS[i].db - MAP_TICKS[i - 1].db);
        return MAP_TICKS[i - 1].pct + (MAP_TICKS[i].pct - MAP_TICKS[i - 1].pct) * t;
      }
    }
    return 0;
  }
  const levelLPct = $derived(linearToScalePct(displayL));
  const levelRPct = $derived(linearToScalePct(displayR));
  const peakLPct = $derived(linearToScalePct(peakL));
  const peakRPct = $derived(linearToScalePct(peakR));
</script>

<div class="stereo-meter" class:wide style="--height: {height}px;">
  <div class="meter-row">
    <!-- Two meter bars -->
    <div class="bars">
      <div class="bar">
        <div class="bar-fill" style="height: {levelLPct}%;"></div>
        {#if peakL > 0.01}
          <div class="peak-line" style="bottom: {peakLPct}%;"></div>
        {/if}
      </div>
      <div class="bar">
        <div class="bar-fill" style="height: {levelRPct}%;"></div>
        {#if peakR > 0.01}
          <div class="peak-line" style="bottom: {peakRPct}%;"></div>
        {/if}
      </div>
    </div>

    <!-- dB scale -->
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

  .bar {
    position: relative;
    width: 10px;
    height: 100%;
    background: var(--bg-recessed);
    border-radius: var(--radius-xs);
    border: 1px solid var(--bezel-dark);
    overflow: hidden;
    box-shadow: inset 0 1px 3px rgba(0,0,0,0.5);
  }

  .bar-fill {
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    background: linear-gradient(
      to top,
      var(--green) 0%,
      var(--green) 55%,
      var(--amber) 75%,
      var(--red) 95%
    );
    transition: height 50ms linear;
    mask-image: repeating-linear-gradient(
      to top,
      black 0px, black 3px,
      transparent 3px, transparent 4px
    );
    -webkit-mask-image: repeating-linear-gradient(
      to top,
      black 0px, black 3px,
      transparent 3px, transparent 4px
    );
  }

  .peak-line {
    position: absolute;
    left: 0;
    right: 0;
    height: 2px;
    background: var(--text-value);
    box-shadow: 0 0 4px rgba(228, 228, 231, 0.3);
    transition: bottom 50ms linear;
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
  .wide .bar { width: 22px; }
  .wide .lr-labels span { width: 22px; font-size: 8px; }
  .wide .lr-labels { gap: 4px; }
  .wide .peak-readout { width: 52px; font-size: 9px; padding: 4px 0; }
  .wide .scale { width: 20px; }
  .wide .tick-label { font-size: 8px; }
  .wide .tick-line { width: 5px; }
</style>
