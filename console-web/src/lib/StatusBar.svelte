<!--
  Bottom status bar — sample rate, clock source, DSP load, device name.
  All values daemon-backed; falls back to sensible defaults when offline.
-->
<script>
  import { getValue, setValue, getConnectionState, getDeviceInfo }
    from "./device-store.svelte.js";

  const connected = $derived(getConnectionState() === "connected");

  // Daemon-backed values, with offline fallbacks.
  const sampleRate = $derived(getValue("/SampleRate/value") ?? 48000);
  const clockSource = $derived(getValue("/ClockSource/value") ?? "Internal");
  const dspLoad = $derived(Math.round(getValue("/TotalDSPLoad/value") ?? 0));
  const pgmLoad = $derived(Math.round(getValue("/TotalPGMLoad/value") ?? 0));
  const memLoad = $derived(Math.round(getValue("/TotalMEMLoad/value") ?? 0));
  const deviceName = $derived(getDeviceInfo().name || "Apollo");

  // Write-through when user changes the dropdown. Offline = no-op so the
  // local select still appears responsive; daemon will reject if unbacked.
  const SAMPLE_RATES = [44100, 48000, 88200, 96000, 176400, 192000];
  const CLOCK_SOURCES = ["Internal", "S/PDIF"];  // Apollo x4 map

  function writeRate(e) {
    const v = Number(e.target.value);
    if (connected) setValue("/SampleRate/value", v);
  }
  function writeClock(e) {
    if (connected) setValue("/ClockSource/value", e.target.value);
  }

  function formatRate(rate) {
    return (rate / 1000).toFixed(rate % 1000 ? 1 : 0) + " kHz";
  }
</script>

<div class="status-bar">
  <!-- Sample Rate -->
  <div class="status-item">
    <span class="status-label">RATE</span>
    <select class="status-select" value={sampleRate} onchange={writeRate}
            disabled={!connected}>
      {#each SAMPLE_RATES as rate}
        <option value={rate}>{formatRate(rate)}</option>
      {/each}
    </select>
  </div>

  <div class="divider"></div>

  <!-- Clock Source -->
  <div class="status-item">
    <span class="status-label">CLOCK</span>
    <select class="status-select" value={clockSource} onchange={writeClock}
            disabled={!connected}>
      {#each CLOCK_SOURCES as src}
        <option value={src}>{src.toUpperCase()}</option>
      {/each}
    </select>
  </div>

  <div class="divider"></div>

  <!-- DSP Load Meters -->
  <div class="status-item dsp-meters">
    <div class="load-row">
      <span class="load-label">DSP</span>
      <div class="load-bar"><div class="load-fill" style="width: {dspLoad}%;"></div></div>
      <span class="load-pct">{dspLoad}%</span>
    </div>
    <div class="load-row">
      <span class="load-label">PGM</span>
      <div class="load-bar"><div class="load-fill pgm" style="width: {pgmLoad}%;"></div></div>
      <span class="load-pct">{pgmLoad}%</span>
    </div>
    <div class="load-row">
      <span class="load-label">MEM</span>
      <div class="load-bar"><div class="load-fill mem" style="width: {memLoad}%;"></div></div>
      <span class="load-pct">{memLoad}%</span>
    </div>
  </div>

  <div class="spacer"></div>

  <!-- Device info -->
  <div class="status-item device-info">
    <span class="status-label">{deviceName}</span>
  </div>
</div>

<style>
  .status-bar {
    display: flex;
    align-items: center;
    gap: 0;
    padding: 0 var(--sp-lg);
    height: 32px;
    background: var(--bg-elevated);
    border-top: 1px solid var(--bezel-dark);
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    z-index: 100;
  }

  .status-item {
    display: flex;
    align-items: center;
    gap: var(--sp-sm);
    padding: 0 var(--sp-md);
    height: 100%;
  }

  .status-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: var(--font-tiny);
    font-weight: 700;
    letter-spacing: 1px;
  }

  .status-select {
    background: var(--bg-recessed);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-value);
    font-weight: 600;
    padding: 2px 20px 2px 6px;
    outline: none;
    appearance: none;
    -webkit-appearance: none;
    cursor: pointer;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='8' height='5'%3E%3Cpath d='M0 0l4 5 4-5z' fill='%2371717a'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 6px center;
  }
  .status-select:hover:not(:disabled) {
    border-color: var(--text-dimmed);
  }
  .status-select:focus { border-color: var(--amber); }
  .status-select:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .divider {
    width: 1px;
    height: 16px;
    background: var(--bezel-dark);
  }

  /* ── DSP Load Meters ─────────────────────────────────────── */
  .dsp-meters {
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    padding: 3px var(--sp-md);
  }

  .load-row {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .load-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: 6px;
    font-weight: 700;
    letter-spacing: 0.5px;
    width: 18px;
  }

  .load-bar {
    width: 60px;
    height: 4px;
    background: var(--bg-recessed);
    border-radius: 2px;
    overflow: hidden;
  }

  .load-fill {
    height: 100%;
    background: var(--green);
    border-radius: 2px;
    transition: width 200ms linear;
  }
  .load-fill.pgm { background: var(--amber); }
  .load-fill.mem { background: var(--blue); }

  .load-pct {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: 6px;
    font-variant-numeric: tabular-nums;
    width: 16px;
    text-align: right;
  }

  .spacer { flex: 1; }

  .device-info .status-label {
    color: var(--text-secondary);
  }
</style>
