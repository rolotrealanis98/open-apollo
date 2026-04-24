<!--
  Vertical fader with textured thumb grip and dB scale.
  Scale matches UAD Console: +12 top, 0dB (unity) at ~75%, log spacing below.

  Usage:
    <Fader bind:value label="MON" color="amber" />
-->
<script>
  let {
    value = $bindable(0.75),   // 0-1 normalized, 0.75 = unity (0dB)
    label = "",
    color = "amber",
    height = 160,
    onchange = undefined,      // callback: (normalizedValue) => void
  } = $props();

  let dragging = $state(false);
  let trackEl;

  // UAD Console dB scale (non-linear)
  // value 0.0=−∞, 0.75=0dB (unity), 1.0=+12dB
  const TICKS = [
    { db: "+12", pct: 100 },
    { db: "+6",  pct: 88 },
    { db: "0",   pct: 75 },
    { db: "−6",  pct: 63 },
    { db: "−12", pct: 50 },
    { db: "−20", pct: 37 },
    { db: "−32", pct: 24 },
    { db: "−56", pct: 12 },
    { db: "−∞",  pct: 0 },
  ];

  // Convert 0-1 value to dB for display
  let displayDb = $derived.by(() => {
    if (value < 0.01) return "−∞";
    if (value <= 0.75) {
      const db = -56 * Math.pow(1 - value / 0.75, 2);
      return db > -0.5 ? "0.0" : db.toFixed(1);
    }
    const db = ((value - 0.75) / 0.25) * 12;
    return "+" + db.toFixed(1);
  });

  function onPointerDown(e) {
    dragging = true;
    e.currentTarget.setPointerCapture(e.pointerId);
    updateValue(e);
  }

  function onPointerMove(e) {
    if (!dragging) return;
    updateValue(e);
  }

  function onPointerUp() {
    dragging = false;
  }

  function updateValue(e) {
    if (!trackEl) return;
    const rect = trackEl.getBoundingClientRect();
    const y = Math.max(0, Math.min(1, 1 - (e.clientY - rect.top) / rect.height));
    value = y;
    onchange?.(y);
  }
</script>

<div
  class="fader-container"
  style="--height: {height}px; --accent: var(--{color}); --accent-glow: var(--{color}-glow);"
>
  <div class="fader-body">
    <!-- dB scale with tick lines -->
    <div class="scale">
      {#each TICKS as tick}
        <div class="tick" class:unity={tick.db === "0"} style="bottom: {tick.pct}%;">
          <span class="tick-label">{tick.db}</span>
          <span class="tick-line"></span>
        </div>
      {/each}
    </div>

    <!-- Track (slot) -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="track-area"
      bind:this={trackEl}
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
    >
      <div class="slot">
        <!-- Unity mark on the slot -->
        <div class="unity-mark"></div>
      </div>

      <!-- Thumb / fader cap -->
      <div class="thumb" class:dragging style="bottom: {value * 100}%;">
        <div class="grip-line"></div>
        <div class="grip-line"></div>
        <div class="grip-line wide"></div>
        <div class="grip-line"></div>
        <div class="grip-line"></div>
      </div>
    </div>
  </div>

  <span class="db-readout">{displayDb}</span>
</div>

<style>
  .fader-container {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-sm);
    user-select: none;
  }

  .fader-body {
    display: flex;
    gap: 0;
    height: var(--height);
    align-items: stretch;
    /* Offset left so the slot (not scale+slot) is centered */
    margin-left: -14px;
  }

  /* ── Scale with tick marks ───────────────────────────────── */
  .scale {
    position: relative;
    width: 28px;
    height: 100%;
  }

  .tick {
    position: absolute;
    right: 0;
    display: flex;
    align-items: center;
    gap: 2px;
    transform: translateY(50%);
  }

  .tick-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: 7px;
    font-variant-numeric: tabular-nums;
    text-align: right;
    min-width: 18px;
  }

  .tick-line {
    display: block;
    width: 4px;
    height: 1px;
    background: var(--text-dimmed);
  }

  /* Unity (0dB) gets brighter tick */
  .tick.unity .tick-label {
    color: var(--text-secondary);
    font-weight: 600;
  }
  .tick.unity .tick-line {
    width: 6px;
    background: var(--text-secondary);
  }

  /* ── Track area (wider hit zone) ─────────────────────────── */
  .track-area {
    position: relative;
    width: 32px;
    height: 100%;
    cursor: pointer;
    touch-action: none;
    display: flex;
    justify-content: center;
  }

  /* ── Slot (the groove) ─────────────────────────────────── */
  .slot {
    position: relative;
    width: 10px;
    height: 100%;
    background: var(--bg-recessed);
    border-radius: 5px;
    border: 1px solid var(--bezel-dark);
    box-shadow:
      inset 0 1px 3px rgba(0,0,0,0.6),
      inset 0 -1px 1px rgba(255,255,255,0.02);
    overflow: hidden;
  }

  /* Unity mark — small notch on the slot at 75% */
  .unity-mark {
    position: absolute;
    bottom: 75%;
    left: -1px;
    right: -1px;
    height: 1px;
    background: var(--text-dimmed);
    opacity: 0.5;
  }

  /* ── Thumb / fader cap ───────────────────────────────────── */
  .thumb {
    position: absolute;
    left: -1px;
    right: -1px;
    height: 30px;
    transform: translateY(50%);
    cursor: grab;

    background:
      linear-gradient(180deg,
        #666 0%, #555 10%, #444 45%, #333 85%, #2a2a2a 100%
      );
    border-radius: 3px;
    border: 1px solid rgba(255,255,255,0.12);
    border-bottom-color: rgba(0,0,0,0.3);
    box-shadow:
      0 2px 6px rgba(0,0,0,0.5),
      inset 0 1px 0 rgba(255,255,255,0.15);

    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 3px;

    transition: box-shadow var(--anim-fast) ease;
  }

  .thumb.dragging {
    cursor: grabbing;
    box-shadow:
      0 0 6px var(--accent-glow),
      0 1px 4px rgba(0,0,0,0.5),
      inset 0 1px 0 rgba(255,255,255,0.12);
  }

  .grip-line {
    width: 16px;
    height: 1px;
    background: rgba(255,255,255,0.1);
  }
  .grip-line.wide {
    width: 20px;
    background: rgba(255,255,255,0.18);
  }

  /* ── Readout ─────────────────────────────────────────────── */
  .db-readout {
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-value);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
</style>
