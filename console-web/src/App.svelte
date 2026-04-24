<!--
  Apollo Console — Mixer Layout
  Preamp section on top (analog channels only),
  channel strips (analog + S/PDIF + virtual) + monitor section below.
-->
<script>
  import { onMount } from "svelte";
  import PreampSection from "./lib/PreampSection.svelte";
  import ChannelStrip from "./lib/ChannelStrip.svelte";
  import MonitorSection from "./lib/MonitorSection.svelte";
  import StatusBar from "./lib/StatusBar.svelte";
  import SettingsPage from "./lib/SettingsPage.svelte";

  let showSettings = $state(false);
  import { connectToDaemon, retryConnection, getConnectionState, getDeviceInfo, getValue } from "./lib/device-store.svelte.js";

  let connState = $derived(getConnectionState());
  let device = $derived(getDeviceInfo());

  // Connection settings — env default, localStorage overrides, UI inputs override those.
  // VITE_WS_URL format: ws://host:port  (e.g. ws://192.168.1.3:4721)
  function _parseDefault() {
    const envUrl = import.meta.env.VITE_WS_URL;
    if (envUrl) {
      try {
        const u = new URL(envUrl);
        return { host: u.hostname, port: Number(u.port) || 4721 };
      } catch {}
    }
    return { host: "localhost", port: 4721 };
  }
  const _defaults = _parseDefault();
  const _stored = (() => {
    try { return JSON.parse(localStorage.getItem("apollo.wsTarget") || "null"); } catch { return null; }
  })();

  let host = $state(_stored?.host ?? _defaults.host);
  let port = $state(_stored?.port ?? _defaults.port);

  function doConnect() {
    try { localStorage.setItem("apollo.wsTarget", JSON.stringify({ host, port })); } catch {}
    connectToDaemon(host, port);
  }

  // Simulated meter levels (fallback when not connected)
  let meters = $state([0.6, 0.45, 0.3, 0.2, 0.35, 0.5, 0.4, 0.25]);
  let peaks  = $state([0.7, 0.55, 0.4, 0.25, 0.45, 0.6, 0.5, 0.3]);

  onMount(() => {
    const interval = setInterval(() => {
      // Only simulate when not connected to real daemon
      if (connState === "connected") return;
      for (let i = 0; i < 8; i++) {
        meters[i] = Math.max(0, Math.min(1,
          meters[i] + (Math.random() - 0.52) * 0.12
        ));
        if (meters[i] > peaks[i]) peaks[i] = meters[i];
        else peaks[i] = Math.max(meters[i], peaks[i] - 0.004);
      }
    }, 50);
    return () => clearInterval(interval);
  });
</script>

<div class="mixer">
  <!-- ── Connection bar ──────────────────────────────────── -->
  <div class="conn-bar">
    <div class="conn-status">
      <span class="status-dot" class:connected={connState === "connected"} class:connecting={connState === "connecting"}></span>
      <span class="status-text">
        {#if connState === "connected"}
          {device.name}
        {:else if connState === "connecting"}
          Connecting…
        {:else if connState === "reconnecting"}
          Reconnecting…
        {:else if connState === "failed"}
          Connection failed
        {:else}
          Disconnected
        {/if}
      </span>
    </div>
    <button class="settings-btn" onclick={() => showSettings = true} title="Settings">SETTINGS</button>
    {#if connState === "disconnected" || connState === "failed"}
      <div class="conn-form">
        <input type="text" bind:value={host} placeholder="Host" class="conn-input" />
        <input type="number" bind:value={port} class="conn-input port" />
        <button class="conn-btn" onclick={doConnect}>Connect</button>
        {#if connState === "failed"}
          <button class="conn-btn" onclick={retryConnection}>Retry</button>
        {/if}
      </div>
    {/if}
  </div>

  <!-- ── Top: Preamp Section (analog channels only) ──────── -->
  <div class="preamp-row">
    <PreampSection />
    <!-- Empty spacer for non-preamp channels -->
    <div class="preamp-spacer"></div>
  </div>

  <!-- ── Bottom: Channel Strips (scrollable) + Monitor (fixed right) ── -->
  <div class="mixer-bottom">
    <!-- Scrollable channel area -->
    <div class="channels-scroll">
      <div class="strip-group">
        <div class="group-label">ANALOG</div>
        <div class="strips">
          <ChannelStrip name="AN 1" ch={0} level={meters[0]} peak={peaks[0]} />
          <ChannelStrip name="AN 2" ch={1} level={meters[1]} peak={peaks[1]} />
          <ChannelStrip name="AN 3" ch={2} level={meters[2]} peak={peaks[2]} />
          <ChannelStrip name="AN 4" ch={3} level={meters[3]} peak={peaks[3]} />
        </div>
      </div>

      <div class="strip-group">
        <div class="group-label">S/PDIF</div>
        <div class="strips">
          <ChannelStrip name="S/PDIF" ch={4} stereo level={meters[4]} peak={peaks[4]} />
        </div>
      </div>

      <div class="strip-group">
        <div class="group-label">VIRTUAL</div>
        <div class="strips">
          <ChannelStrip name="VIRT 1-2" ch={6} stereo level={meters[5]} peak={peaks[5]} />
          <ChannelStrip name="VIRT 3-4" ch={8} stereo level={meters[6]} peak={peaks[6]} />
        </div>
      </div>

      <div class="strip-group">
        <div class="group-label">AUX</div>
        <div class="strips">
          <ChannelStrip name="AUX 1" ch={10} stereo level={meters[7]} peak={peaks[7]} />
          <ChannelStrip name="AUX 2" ch={12} stereo level={meters[7]} peak={peaks[7]} />
        </div>
      </div>
    </div>

    <!-- Monitor: fixed right, outside scroll -->
    <div class="monitor-fixed">
      <MonitorSection />
    </div>
  </div>
</div>

<!-- Status bar (fixed bottom) -->
<StatusBar />

<!-- Settings modal -->
{#if showSettings}
  <SettingsPage onclose={() => showSettings = false} />
{/if}

<style>
  .mixer {
    display: flex;
    flex-direction: column;
    gap: var(--sp-sm);
    padding: var(--sp-xl);
    padding-bottom: 56px; /* room for fixed status bar */
  }

  /* ── Connection bar ──────────────────────────────────────── */
  .conn-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-sm) var(--sp-md);
    background: var(--bg-surface);
    border-radius: var(--radius-md);
    border: 1px solid var(--bezel-dark);
  }
  .conn-status {
    display: flex;
    align-items: center;
    gap: var(--sp-sm);
  }
  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--red);
  }
  .status-dot.connecting {
    background: var(--amber);
    animation: pulse 1s infinite;
  }
  .status-dot.connected {
    background: var(--green);
    box-shadow: 0 0 4px var(--green-glow);
  }
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
  }
  .status-text {
    color: var(--text-secondary);
    font-family: var(--font-family);
    font-size: var(--font-value);
    font-weight: 600;
  }
  .conn-form {
    display: flex;
    gap: var(--sp-xs);
    align-items: center;
  }
  .conn-input {
    background: var(--bg-recessed);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-label);
    padding: 4px 8px;
    width: 120px;
    outline: none;
  }
  .conn-input.port {
    width: 50px;
  }
  .conn-input:focus {
    border-color: var(--text-dimmed);
  }
  .conn-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-label);
    font-weight: 600;
    padding: 4px 12px;
    cursor: pointer;
  }
  .conn-btn:hover {
    background: var(--bezel-light);
  }
  .settings-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--bezel-dark);
    border-radius: var(--radius-xs);
    color: var(--text-secondary);
    font-family: var(--font-family);
    font-size: var(--font-tiny);
    font-weight: 700;
    letter-spacing: 1px;
    cursor: pointer;
    padding: 4px 12px;
  }
  .settings-btn:hover {
    color: var(--text-value);
    background: var(--bezel-light);
  }

  /* ── Preamp row aligns with analog strips only ──────────── */
  .preamp-row {
    display: flex;
    gap: var(--sp-lg);
  }
  .preamp-spacer {
    flex: 1;
  }

  /* ── Bottom section: channels + monitor ────────────────── */
  .mixer-bottom {
    display: flex;
    gap: 0;
    align-items: flex-end;
    flex: 1;
    min-height: 0;
  }

  /* Scrollable channel area */
  .channels-scroll {
    display: flex;
    gap: var(--sp-lg);
    align-items: flex-end;
    overflow-x: auto;
    overflow-y: hidden;
    flex: 1;
    padding-right: var(--sp-md);
    /* Hide scrollbar but keep scroll functionality */
    scrollbar-width: thin;
    scrollbar-color: var(--bezel-light) transparent;
  }
  .channels-scroll::-webkit-scrollbar {
    height: 6px;
  }
  .channels-scroll::-webkit-scrollbar-track {
    background: transparent;
  }
  .channels-scroll::-webkit-scrollbar-thumb {
    background: var(--bezel-light);
    border-radius: 3px;
  }

  /* Monitor: fixed right, not scrollable */
  .monitor-fixed {
    flex-shrink: 0;
    padding-left: var(--sp-md);
    border-left: 1px solid var(--bezel-dark);
  }

  /* ── Strip groups ────────────────────────────────────────── */
  .strip-group {
    display: flex;
    flex-direction: column;
    gap: var(--sp-xs);
    flex-shrink: 0;
  }

  .group-label {
    color: var(--text-dimmed);
    font-family: var(--font-family);
    font-size: var(--font-tiny);
    font-weight: 700;
    letter-spacing: 2px;
    padding-left: var(--sp-xs);
  }

  .strips {
    display: flex;
    gap: 2px;
  }

</style>
