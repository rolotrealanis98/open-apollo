<!--
  Settings modal — tabbed panel matching UAD Console layout.
  Tabs: HARDWARE, I/O MATRIX, OPTIONS
-->
<script>
  let { onclose } = $props();

  let activeTab = $state("hardware");

  // Hardware settings
  let sampleRate = $state("48 kHz");
  let clockSource = $state("INTERNAL");
  let digitalMirror = $state("OFF");
  let inputDelay = $state("MEDIUM");
  let cueBusCount = $state("2");
  let altCount = $state("0");

  // Device settings
  let deviceName = $state("Apollo x4");
  let digitalInput = $state("S/PDIF");
  let digitalOutput = $state("S/PDIF");
  let monLevel = $state("20 dBu");
  let refLevel12 = $state("+4 dBu");
  let refLevel34 = $state("+4 dBu");

  // Options
  let metering = $state("PRE-FADER");
  let clipHold = $state("3 SEC");
  let peakHold = $state("3 SEC");
  let controlsMode = $state("LINEAR");
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="overlay" onclick={onclose}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="settings-modal" onclick={(e) => e.stopPropagation()}>
    <!-- Header -->
    <div class="modal-header">
      <button class="close-btn" onclick={onclose}>&#10005;</button>
      <span class="modal-title">SETTINGS</span>
    </div>

    <!-- Tabs -->
    <div class="tab-bar">
      <button class="tab" class:active={activeTab === "hardware"} onclick={() => activeTab = "hardware"}>HARDWARE</button>
      <button class="tab" class:active={activeTab === "io"} onclick={() => activeTab = "io"}>I/O MATRIX</button>
      <button class="tab" class:active={activeTab === "options"} onclick={() => activeTab = "options"}>OPTIONS</button>
    </div>

    <!-- Tab content -->
    <div class="tab-content">
      {#if activeTab === "hardware"}
        <!-- ── HARDWARE TAB ──────────────────────────────── -->
        <div class="settings-row">
          <div class="setting-card">
            <span class="setting-label">SAMPLE RATE</span>
            <select class="setting-select" bind:value={sampleRate}>
              <option>44.1 kHz</option>
              <option>48 kHz</option>
              <option>88.2 kHz</option>
              <option>96 kHz</option>
              <option>176.4 kHz</option>
              <option>192 kHz</option>
            </select>
          </div>
          <div class="setting-card">
            <span class="setting-label">CLOCK SOURCE</span>
            <select class="setting-select" bind:value={clockSource}>
              <option>INTERNAL</option>
              <option>S/PDIF</option>
              <option>ADAT</option>
              <option>WORD CLOCK</option>
            </select>
          </div>
          <div class="setting-card">
            <span class="setting-label">DIGITAL MIRROR</span>
            <select class="setting-select" bind:value={digitalMirror}>
              <option>OFF</option>
              <option>ON</option>
            </select>
          </div>
        </div>

        <div class="settings-row">
          <div class="setting-card">
            <span class="setting-label">INPUT DELAY COMPENSATION</span>
            <select class="setting-select" bind:value={inputDelay}>
              <option>SHORT</option>
              <option>MEDIUM</option>
              <option>LONG</option>
            </select>
          </div>
          <div class="setting-card">
            <span class="setting-label">CUE BUS COUNT</span>
            <select class="setting-select" bind:value={cueBusCount}>
              <option>1</option>
              <option>2</option>
            </select>
          </div>
          <div class="setting-card">
            <span class="setting-label">ALT COUNT</span>
            <select class="setting-select" bind:value={altCount}>
              <option>0</option>
              <option>1</option>
              <option>2</option>
            </select>
          </div>
        </div>

        <!-- Devices section -->
        <div class="section-divider">
          <span class="tab active">DEVICES</span>
        </div>

        <div class="device-panel">
          <div class="settings-row">
            <div class="setting-card wide">
              <span class="setting-label">DEVICE NAME</span>
              <input type="text" class="setting-input" bind:value={deviceName} />
            </div>
            <button class="action-btn">IDENTIFY</button>
          </div>

          <div class="settings-row">
            <div class="setting-card">
              <span class="setting-label">DIGITAL INPUT</span>
              <select class="setting-select" bind:value={digitalInput}>
                <option>S/PDIF</option>
                <option>ADAT</option>
              </select>
            </div>
            <div class="setting-card">
              <span class="setting-label">DIGITAL OUTPUT</span>
              <select class="setting-select" bind:value={digitalOutput}>
                <option>S/PDIF</option>
                <option>ADAT</option>
              </select>
            </div>
            <div class="setting-card">
              <span class="setting-label">MON LEVEL</span>
              <select class="setting-select" bind:value={monLevel}>
                <option>20 dBu</option>
                <option>14 dBu</option>
              </select>
            </div>
          </div>

          <div class="sub-heading">OUTPUT REFERENCE LEVELS</div>
          <div class="settings-row">
            <div class="setting-card">
              <span class="setting-label">1-2</span>
              <select class="setting-select" bind:value={refLevel12}>
                <option>+4 dBu</option>
                <option>-10 dBV</option>
              </select>
            </div>
            <div class="setting-card">
              <span class="setting-label">3-4</span>
              <select class="setting-select" bind:value={refLevel34}>
                <option>+4 dBu</option>
                <option>-10 dBV</option>
              </select>
            </div>
          </div>
        </div>

      {:else if activeTab === "io"}
        <!-- ── I/O MATRIX TAB ────────────────────────────── -->
        <div class="io-matrix">
          <div class="io-table-wrap">
            <table class="io-table">
              <thead>
                <tr>
                  <th>CH</th>
                  <th>DEVICE</th>
                  <th>INPUT</th>
                  <th>CUSTOM NAME</th>
                </tr>
              </thead>
              <tbody>
                {#each [
                  { ch: 1, input: "MIC/LINE/HIZ 1" },
                  { ch: 2, input: "MIC/LINE/HIZ 2" },
                  { ch: 3, input: "MIC/LINE 3" },
                  { ch: 4, input: "MIC/LINE 4" },
                  { ch: 5, input: "S/PDIF L" },
                  { ch: 6, input: "S/PDIF R" },
                  { ch: 7, input: "VIRTUAL 1" },
                  { ch: 8, input: "VIRTUAL 2" },
                  { ch: 9, input: "VIRTUAL 3" },
                  { ch: 10, input: "VIRTUAL 4" },
                ] as row}
                  <tr>
                    <td class="ch-num">{row.ch}</td>
                    <td>Apollo x4</td>
                    <td>{row.input}</td>
                    <td><input type="text" class="name-input" value={row.input} /></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>

          <div class="io-table-wrap">
            <table class="io-table">
              <thead>
                <tr>
                  <th>CH</th>
                  <th>DEVICE</th>
                  <th>OUTPUT</th>
                  <th>CUSTOM NAME</th>
                </tr>
              </thead>
              <tbody>
                {#each [
                  { ch: 1, output: "MON L" },
                  { ch: 2, output: "MON R" },
                  { ch: 3, output: "LINE 1" },
                  { ch: 4, output: "LINE 2" },
                  { ch: 5, output: "LINE 3" },
                  { ch: 6, output: "LINE 4" },
                  { ch: 7, output: "S/PDIF L" },
                  { ch: 8, output: "S/PDIF R" },
                ] as row}
                  <tr>
                    <td class="ch-num">{row.ch}</td>
                    <td>Apollo x4</td>
                    <td>{row.output}</td>
                    <td><input type="text" class="name-input" value={row.output} /></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </div>

      {:else if activeTab === "options"}
        <!-- ── OPTIONS TAB ───────────────────────────────── -->
        <div class="sub-heading">DISPLAY</div>
        <div class="settings-row">
          <div class="setting-card">
            <span class="setting-label">METERING</span>
            <select class="setting-select" bind:value={metering}>
              <option>PRE-FADER</option>
              <option>POST-FADER</option>
            </select>
          </div>
          <div class="setting-card">
            <span class="setting-label">CLIP HOLD</span>
            <select class="setting-select" bind:value={clipHold}>
              <option>OFF</option>
              <option>1 SEC</option>
              <option>3 SEC</option>
              <option>INFINITE</option>
            </select>
          </div>
          <div class="setting-card">
            <span class="setting-label">PEAK HOLD</span>
            <select class="setting-select" bind:value={peakHold}>
              <option>OFF</option>
              <option>1 SEC</option>
              <option>3 SEC</option>
              <option>INFINITE</option>
            </select>
          </div>
        </div>

        <div class="sub-heading">EDITING</div>
        <div class="settings-row">
          <div class="setting-card">
            <span class="setting-label">CONTROLS MODE</span>
            <select class="setting-select" bind:value={controlsMode}>
              <option>LINEAR</option>
              <option>CIRCULAR</option>
            </select>
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  /* ── Overlay ─────────────────────────────────────────────── */
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.7);
    z-index: 200;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .settings-modal {
    width: 700px;
    max-height: 85vh;
    background: var(--bg-primary);
    border: 1px solid var(--bezel-light);
    border-radius: var(--radius-md);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  /* ── Header ──────────────────────────────────────────────── */
  .modal-header {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--sp-sm) var(--sp-md);
    background: var(--bg-elevated);
    border-bottom: 1px solid var(--bezel-dark);
    position: relative;
  }

  .modal-title {
    color: var(--text-secondary);
    font-family: var(--font-family);
    font-size: var(--font-heading);
    font-weight: 700;
    letter-spacing: 2px;
  }

  .close-btn {
    position: absolute;
    left: var(--sp-md);
    background: none;
    border: none;
    color: var(--text-secondary);
    font-size: 14px;
    cursor: pointer;
    padding: 4px 8px;
  }
  .close-btn:hover { color: var(--text-value); }

  /* ── Tab bar ─────────────────────────────────────────────── */
  .tab-bar {
    display: flex;
    gap: 0;
    background: var(--bg-surface);
    padding: var(--sp-sm) var(--sp-lg);
  }

  .tab {
    flex: 1;
    padding: var(--sp-sm) var(--sp-md);
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: var(--font-label);
    font-weight: 700;
    letter-spacing: 1.5px;
    cursor: pointer;
    text-align: center;
  }
  .tab:hover { color: var(--text-secondary); }
  .tab.active {
    color: var(--amber);
    border-bottom-color: var(--amber);
  }

  /* ── Tab content ─────────────────────────────────────────── */
  .tab-content {
    padding: var(--sp-lg);
    overflow-y: auto;
    flex: 1;
  }

  /* ── Settings rows and cards ─────────────────────────────── */
  .settings-row {
    display: flex;
    gap: var(--sp-sm);
    margin-bottom: var(--sp-md);
    align-items: flex-end;
  }

  .setting-card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-xs);
    padding: var(--sp-sm) var(--sp-md);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
  }
  .setting-card.wide { flex: 1; }

  .setting-label {
    color: var(--amber);
    font-family: var(--font-family);
    font-size: var(--font-tiny);
    font-weight: 700;
    letter-spacing: 0.5px;
  }

  .setting-select, .setting-input {
    background: var(--bg-recessed);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-value);
    font-weight: 600;
    padding: 6px 10px;
    outline: none;
    min-width: 100px;
    appearance: none;
    -webkit-appearance: none;
  }
  .setting-select {
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='8' height='5'%3E%3Cpath d='M0 0l4 5 4-5z' fill='%2371717a'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 8px center;
    padding-right: 24px;
    cursor: pointer;
  }
  .setting-select:focus, .setting-input:focus {
    border-color: var(--text-dimmed);
  }

  .action-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-label);
    font-weight: 700;
    letter-spacing: 1px;
    padding: 8px 16px;
    cursor: pointer;
    height: fit-content;
  }
  .action-btn:hover { background: var(--bezel-light); }

  .sub-heading {
    color: var(--text-secondary);
    font-family: var(--font-family);
    font-size: var(--font-value);
    font-weight: 700;
    letter-spacing: 1px;
    margin: var(--sp-md) 0 var(--sp-sm);
  }

  .section-divider {
    display: flex;
    gap: var(--sp-lg);
    border-bottom: 1px solid var(--bezel-dark);
    margin: var(--sp-lg) 0 var(--sp-md);
    padding-bottom: var(--sp-xs);
  }

  .device-panel {
    padding-left: var(--sp-sm);
  }

  /* ── I/O Matrix table ────────────────────────────────────── */
  .io-matrix {
    display: flex;
    gap: var(--sp-lg);
  }

  .io-table-wrap {
    flex: 1;
    overflow-y: auto;
    max-height: 400px;
  }

  .io-table {
    width: 100%;
    border-collapse: collapse;
    font-family: var(--font-family);
    font-size: var(--font-label);
  }

  .io-table thead {
    position: sticky;
    top: 0;
    background: var(--bg-elevated);
  }

  .io-table th {
    color: var(--text-dimmed);
    font-weight: 700;
    letter-spacing: 0.5px;
    padding: 6px 8px;
    text-align: left;
    border-bottom: 1px solid var(--bezel-dark);
  }

  .io-table td {
    color: var(--text-secondary);
    padding: 5px 8px;
    border-bottom: 1px solid var(--bg-recessed);
  }

  .io-table .ch-num {
    color: var(--text-value);
    font-weight: 700;
    width: 30px;
    text-align: center;
  }

  .name-input {
    background: transparent;
    border: none;
    color: var(--text-secondary);
    font-family: var(--font-family);
    font-size: var(--font-label);
    padding: 2px 4px;
    width: 100%;
    outline: none;
  }
  .name-input:focus {
    background: var(--bg-recessed);
    border-radius: var(--radius-xs);
    color: var(--text-value);
  }
</style>
