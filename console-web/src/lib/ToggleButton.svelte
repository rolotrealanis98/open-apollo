<!--
  Skeuomorphic toggle button — modeled after UAD Console channel buttons.
  Recessed cavity when off, colored glow when on, physical bevel edges.

  Usage:
    <ToggleButton label="48V" color="red" active />
    <ToggleButton filter color="green" />
-->
<script>
  let {
    label = "",
    color = "amber",     // red | amber | green | blue | gray
    active = $bindable(false),
    disabled = false,
    filter = false,       // show HPF curve icon instead of text
    warn = "",            // non-empty → render "unverified" outline + tooltip
    onclick = undefined,
  } = $props();

  // Stateless: parent owns truth. Report the intended next value; parent
  // decides when to re-render with new `active`. Writing to `active` locally
  // breaks downstream prop flow when the parent uses a $derived source.
  function toggle() {
    if (disabled) return;
    const next = !active;
    if (onclick) onclick(next);
    else active = next;
  }
</script>

<button
  class="toggle"
  class:active
  class:disabled
  class:warn={!!warn}
  style="
    --accent: var(--{color});
    --accent-dim: var(--{color}-dim);
    --accent-glow: var(--{color}-glow);
  "
  onclick={toggle}
  {disabled}
  title={warn}
>
  {#if filter}
    <!-- HPF filter curve icon -->
    <svg class="filter-icon" viewBox="0 0 28 14" fill="none">
      <path
        d="M2 11 L6 11 C12 11 12 3 18 3 L26 3"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  {:else}
    <span class="label">{label}</span>
  {/if}
</button>

<style>
  .toggle {
    /* Layout */
    width: 46px;
    height: 22px;
    position: relative;
    cursor: pointer;
    border: none;
    outline: none;

    /* 3D bezel — gradient border simulates raised edge */
    background:
      linear-gradient(180deg, var(--bezel-light), var(--bezel-dark));
    border-radius: var(--radius-sm);
    padding: 1px;

    /* Text */
    color: var(--text-secondary);
    font-family: var(--font-family);
    font-size: var(--font-label);
    font-weight: 600;
    letter-spacing: 0.5px;

    /* Smooth transitions */
    transition:
      box-shadow var(--anim-normal) ease,
      color var(--anim-fast) ease;

    /* No glow when off */
    box-shadow: none;
  }

  /* ── Inner face ──────────────────────────────────────────── */
  .toggle::before {
    content: '';
    position: absolute;
    inset: 1px;
    border-radius: calc(var(--radius-sm) - 1px);
    transition: background var(--anim-fast) ease, border-color var(--anim-fast) ease;
    border: 1px solid transparent;

    /* Off: dark recessed cavity with top shadow */
    background:
      linear-gradient(
        180deg,
        rgba(0,0,0,0.35) 0%,
        transparent 45%,
        rgba(255,255,255,0.03) 100%
      ),
      var(--bg-recessed);
  }

  /* ── Active state ────────────────────────────────────────── */
  .toggle.active {
    color: var(--text-on-accent);

    /* Subtle glow — just enough to feel lit */
    box-shadow:
      0 0 3px 0 var(--accent-glow),
      0 0 8px 1px color-mix(in srgb, var(--accent) 25%, transparent),
      inset 0 0 2px rgba(255,255,255,0.15);
  }

  .toggle.active::before {
    /* Lit surface: brighter, saturated face */
    background:
      linear-gradient(
        180deg,
        rgba(255,255,255,0.22) 0%,
        rgba(255,255,255,0.04) 40%,
        transparent 70%
      ),
      var(--accent-dim);
    border-color: color-mix(in srgb, var(--accent) 60%, transparent);
  }

  /* ── Hover ───────────────────────────────────────────────── */
  .toggle:hover:not(.active):not(.disabled) {
    box-shadow: 0 0 4px 1px rgba(255,255,255,0.06);
  }
  .toggle:hover:not(.active):not(.disabled)::before {
    background:
      linear-gradient(
        180deg,
        rgba(0,0,0,0.25) 0%,
        transparent 45%,
        rgba(255,255,255,0.06) 100%
      ),
      var(--bg-recessed);
  }

  /* ── Press ───────────────────────────────────────────────── */
  .toggle:active:not(.disabled) {
    transform: translateY(0.5px);
  }

  /* ── Disabled ────────────────────────────────────────────── */
  .toggle.disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  /* ── Warn: control writes daemon state but hardware path unverified ── */
  .toggle.warn::after {
    content: "!";
    position: absolute;
    top: -4px;
    right: -4px;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--amber);
    color: #000;
    font-size: 8px;
    font-weight: 900;
    line-height: 10px;
    text-align: center;
    z-index: 2;
    box-shadow: 0 0 3px var(--amber-glow);
    pointer-events: none;
  }

  /* ── Content positioning ─────────────────────────────────── */
  .label, .filter-icon {
    position: relative;
    z-index: 1;
    pointer-events: none;
  }

  .filter-icon {
    width: 28px;
    height: 14px;
    display: block;
    margin: auto;
  }

  .label {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
  }
</style>
