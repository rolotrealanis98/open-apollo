<!--
  Skeuomorphic rotary knob with 270° arc indicator.
  Drag vertically to change value. Metallic cap with indicator line.

  Usage:
    <Knob bind:value label="GAIN" min={10} max={65} unit="dB" color="amber" />
-->
<script>
  let {
    value = $bindable(0.5),
    min = 0,
    max = 100,
    unit = "",
    color = "amber",
    size = 64,
    label = "",
    format = "number",  // "number" or "pan" (L/C/R)
    showArc = true,     // show the LED arc indicator
    onchange = undefined,  // callback: (normalizedValue) => void
  } = $props();

  let dragging = $state(false);
  let startY = 0;
  let startValue = 0;

  // Arc geometry (270° sweep, gap at bottom)
  const R = 40;
  const CIRC = 2 * Math.PI * R;            // ~251.33
  const ARC = (270 / 360) * CIRC;           // ~188.5
  const GAP = CIRC - ARC;                   // ~62.83

  // Sanitize label for SVG gradient IDs (no spaces/special chars)
  let safeId = $derived(label.replace(/[^a-zA-Z0-9]/g, '-') || 'knob');

  let displayValue = $derived.by(() => {
    if (format === "pan") {
      const raw = Math.round(min + value * (max - min));
      if (Math.abs(raw) < 2) return "C";
      return raw < 0 ? `L${Math.abs(raw)}` : `R${raw}`;
    }
    return Math.round(min + value * (max - min));
  });
  let valueArc = $derived(ARC * value);
  let indicatorAngle = $derived(-135 + value * 270);

  function onPointerDown(e) {
    dragging = true;
    startY = e.clientY;
    startValue = value;
    e.currentTarget.setPointerCapture(e.pointerId);
  }

  function onPointerMove(e) {
    if (!dragging) return;
    const dy = startY - e.clientY;
    value = Math.max(0, Math.min(1, startValue + dy / 150));
    onchange?.(value);
  }

  function onPointerUp() {
    dragging = false;
  }

  function onWheel(e) {
    e.preventDefault();
    value = Math.max(0, Math.min(1, value - e.deltaY / 2000));
  }
</script>

<div
  class="knob-container"
  style="--size: {size}px; --accent: var(--{color}); --accent-glow: var(--{color}-glow);"
>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <svg
    viewBox="0 0 100 100"
    class="knob-svg"
    class:dragging
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onwheel={onWheel}
  >
    {#if showArc}
      <!-- Track arc (background) -->
      <circle
        cx="50" cy="50" r={R}
        fill="none"
        stroke="var(--bg-recessed)"
        stroke-width="4"
        stroke-dasharray="{ARC}, {GAP}"
        transform="rotate(135, 50, 50)"
        stroke-linecap="round"
      />

      <!-- Value arc (filled portion) -->
      <circle
        cx="50" cy="50" r={R}
        fill="none"
        stroke="var(--accent)"
        stroke-width="4"
        stroke-dasharray="{valueArc}, {CIRC}"
        transform="rotate(135, 50, 50)"
        stroke-linecap="round"
        class="value-arc"
      />

      <!-- Glow behind the value arc -->
      <circle
        cx="50" cy="50" r={R}
        fill="none"
        stroke="var(--accent)"
        stroke-width="8"
        stroke-dasharray="{valueArc}, {CIRC}"
        transform="rotate(135, 50, 50)"
        stroke-linecap="round"
        opacity="0.15"
        class="value-arc"
      />
    {/if}

    <!-- Knob cap — metallic surface with depth -->
    <defs>
      <!-- Main cap gradient: light source top-left -->
      <radialGradient id="knob-cap-{safeId}" cx="40%" cy="35%" r="60%">
        <stop offset="0%"  stop-color="#8a8a90" />
        <stop offset="30%" stop-color="#6a6a70" />
        <stop offset="60%" stop-color="#505058" />
        <stop offset="100%" stop-color="#38383e" />
      </radialGradient>
      <!-- Edge shadow ring -->
      <radialGradient id="knob-edge-{safeId}" cx="50%" cy="50%">
        <stop offset="85%" stop-color="transparent" />
        <stop offset="100%" stop-color="rgba(0,0,0,0.35)" />
      </radialGradient>
    </defs>
    <!-- Outer ring / bezel -->
    <circle
      cx="50" cy="50" r="30"
      fill="#2e2e33"
      stroke="#222226"
      stroke-width="0.5"
    />
    <!-- Knurled edge (subtle ridged ring) -->
    <circle
      cx="50" cy="50" r="29"
      fill="none"
      stroke="rgba(255,255,255,0.06)"
      stroke-width="0.5"
    />
    <!-- Main cap face -->
    <circle
      cx="50" cy="50" r="26"
      fill="url(#knob-cap-{safeId})"
      stroke="rgba(255,255,255,0.1)"
      stroke-width="0.5"
    />
    <!-- Edge shadow overlay -->
    <circle
      cx="50" cy="50" r="26"
      fill="url(#knob-edge-{safeId})"
    />
    <!-- Top highlight crescent -->
    <ellipse
      cx="48" cy="38"
      rx="14" ry="8"
      fill="rgba(255,255,255,0.06)"
    />

    <!-- Indicator line -->
    <line
      x1="50" y1="32" x2="50" y2="23"
      transform="rotate({indicatorAngle}, 50, 50)"
      stroke="white"
      stroke-width="2"
      stroke-linecap="round"
    />
  </svg>

  <!-- Value readout -->
  <span class="value-readout">{displayValue}{unit && format !== "pan" ? ` ${unit}` : ''}</span>
  {#if label}
    <span class="knob-label">{label}</span>
  {/if}
</div>

<style>
  .knob-container {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    width: var(--size);
    user-select: none;
  }

  .knob-svg {
    width: var(--size);
    height: var(--size);
    cursor: grab;
    filter: drop-shadow(0 2px 4px rgba(0,0,0,0.4));
  }
  .knob-svg.dragging {
    cursor: grabbing;
  }

  .value-arc {
    transition: stroke-dasharray 30ms linear;
  }

  .value-readout {
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-value);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .knob-label {
    color: var(--text-secondary);
    font-family: var(--font-family);
    font-size: var(--font-tiny);
    font-weight: 700;
    letter-spacing: 1.5px;
    text-transform: uppercase;
  }
</style>
