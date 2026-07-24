<!--
  LED-style segmented level meter with peak hold, dB scale, and clip indicator.

  Usage:
    <Meter level={0.6} peak={0.8} />
-->
<script>
  let {
    level = 0,      // 0-1 current level
    peak = 0,       // 0-1 peak hold
    height = 160,
    clip = false,   // true when clipping
    // The three props below exist solely so StereoMeter.svelte can compose
    // two Meter instances (its 1:1 consumer) — not a general-purpose API.
    showClip = true,   // show the clip indicator dot
    showScale = true,  // show the dB scale ticks + peak dB readout
    width,             // optional override for meter body width (px)
  } = $props();

  import { onMount } from "svelte";
  import { METER_TICKS, linearToScalePct } from "./meter-scale.js";

  let clipActive = $derived(clip || peak > 0.97);

  // Ballistic smoothing with wall-clock time constants. alpha = 1 − e^(−dt/τ)
  // is frame-rate independent (valid at any RAF cadence or when tab throttled).
  // Peak line stays unsmoothed so transients remain visible.
  let displayLevel = $state(0);
  const TAU_ATTACK_S = 0.012;   // 12ms rise time constant
  const TAU_RELEASE_S = 0.170;  // 170ms release time constant → ~500ms to silence

  onMount(() => {
    let prev = 0;
    let last = performance.now();
    let rafId = 0;
    const tick = (now) => {
      const dt = Math.max(0, (now - last) / 1000);
      last = now;
      const target = level;
      const tau = target > prev ? TAU_ATTACK_S : TAU_RELEASE_S;
      const alpha = 1 - Math.exp(-dt / tau);
      prev += (target - prev) * alpha;
      if (prev < 1e-4) prev = 0;
      displayLevel = prev;
      rafId = requestAnimationFrame(tick);
    };
    rafId = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafId);
  });

  // Track the highest peak seen (true peak hold, only resets on click)
  let heldPeak = $state(0);

  $effect(() => {
    if (peak > heldPeak) heldPeak = peak;
  });

  function resetPeak() {
    heldPeak = 0;
  }

  // Convert held peak to dBFS
  let peakDb = $derived.by(() => {
    if (heldPeak < 0.01) return "−∞";
    const db = 20 * Math.log10(heldPeak);
    if (db > -0.5) return "0.0";
    return db.toFixed(1);
  });

  const levelPct = $derived(linearToScalePct(displayLevel));
  const peakPct = $derived(linearToScalePct(peak));
</script>

<div class="meter" style="--height: {height}px; --width: {width ?? 14}px;">
  {#if showClip}
    <!-- Clip indicator dot -->
    <div class="clip-dot" class:active={clipActive}></div>
  {/if}

  <div class="meter-row">
    <!-- Meter body -->
    <div class="meter-body">
      <!-- Colored fill with segmented mask -->
      <div class="meter-fill" style="height: {levelPct}%;"></div>

      <!-- Peak hold line -->
      {#if peak > 0.01}
        <div class="peak-line" style="bottom: {peakPct}%;"></div>
      {/if}
    </div>

    <!-- dB scale on right side -->
    {#if showScale}
      <div class="meter-scale">
        {#each METER_TICKS as tick}
          <div class="meter-tick" style="bottom: {tick.pct}%;">
            <span class="meter-tick-line"></span>
            <span class="meter-tick-label">{tick.db}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>

  {#if showScale}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- Peak hold dB readout (click to reset) -->
    <span class="meter-readout" onclick={resetPeak} title="Click to reset peak">{peakDb}</span>
  {/if}
</div>

<style>
  .meter {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
  }

  .meter-row {
    display: flex;
    gap: 2px;
    align-items: stretch;
  }

  /* ── Clip indicator ──────────────────────────────────────── */
  .clip-dot {
    width: 14px;
    height: 4px;
    border-radius: var(--radius-xs);
    background: var(--bg-recessed);
    border: 1px solid var(--bezel-dark);
    transition: all var(--anim-fast) ease;
  }
  .clip-dot.active {
    background: var(--red);
    box-shadow: 0 0 6px var(--red-glow);
    border-color: transparent;
  }

  /* ── Meter body ──────────────────────────────────────────── */
  .meter-body {
    position: relative;
    width: var(--width);
    height: var(--height);
    background: var(--bg-recessed);
    border-radius: var(--radius-xs);
    border: 1px solid var(--bezel-dark);
    overflow: hidden;
    box-shadow: inset 0 1px 3px rgba(0,0,0,0.5);
  }

  /* ── Fill — green→amber→red gradient with segment mask ──── */
  .meter-fill {
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

    /* Segmented LED look — 3px bars with 1px gaps */
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

  /* ── Peak hold line ──────────────────────────────────────── */
  .peak-line {
    position: absolute;
    left: 0;
    right: 0;
    height: 2px;
    background: var(--text-value);
    box-shadow: 0 0 4px rgba(228, 228, 231, 0.3);
    transition: bottom 50ms linear;
  }

  /* ── dB scale (right side of meter) ──────────────────────── */
  .meter-scale {
    position: relative;
    width: 16px;
    height: var(--height);
  }

  .meter-tick {
    position: absolute;
    left: 0;
    display: flex;
    align-items: center;
    gap: 1px;
    transform: translateY(50%);
  }

  .meter-tick-line {
    display: block;
    width: 3px;
    height: 1px;
    background: var(--text-dimmed);
  }

  .meter-tick-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: 6px;
    font-variant-numeric: tabular-nums;
  }

  /* ── Peak hold dB readout ────────────────────────────────── */
  .meter-readout {
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
  .meter-readout:hover {
    border-color: var(--text-dimmed);
  }
</style>
