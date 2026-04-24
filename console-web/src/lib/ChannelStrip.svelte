<!--
  Channel strip — daemon-backed fader, pan, mute, solo, meter.
  Sends + stereo-link deferred (separate plan).
-->
<script>
  import Knob from "./Knob.svelte";
  import Fader from "./Fader.svelte";
  import Meter from "./Meter.svelte";
  import ToggleButton from "./ToggleButton.svelte";
  import { getValue, setValue } from "./device-store.svelte.js";

  let {
    name = "AN 1",
    ch = null,        // daemon input channel index (0-based)
    level = 0,        // fallback meter level (demo mode only)
    peak = 0,         // fallback meter peak (demo mode only)
    stereo = false,
  } = $props();

  // Meter values — derive from daemon when ch is set. Stereo strips use
  // max(L, R) so neither side hides behind the other.
  const leftMeter = $derived(ch === null ? null : `/devices/0/inputs/${ch}/meters/0`);
  const rightMeter = $derived((ch === null || !stereo) ? null : `/devices/0/inputs/${ch + 1}/meters/0`);
  const levelDb = $derived(leftMeter
    ? Math.max(getValue(`${leftMeter}/MeterLevel/value`) ?? -77,
               rightMeter ? (getValue(`${rightMeter}/MeterLevel/value`) ?? -77) : -77)
    : null);
  const peakDb = $derived(leftMeter
    ? Math.max(getValue(`${leftMeter}/MeterPeakLevel/value`) ?? -77,
               rightMeter ? (getValue(`${rightMeter}/MeterPeakLevel/value`) ?? -77) : -77)
    : null);
  const liveLevel = $derived(levelDb === null ? level : (levelDb <= -77 ? 0 : Math.pow(10, levelDb / 20)));
  const livePeak = $derived(peakDb === null ? peak : (peakDb <= -77 ? 0 : Math.pow(10, peakDb / 20)));

  // Controls — daemon or local (demo). Stereo strips read max(L,R) for fader
  // and OR(L,R) for mute/solo so the displayed value reflects the "louder /
  // active" side. Writes mirror to both — starting from the displayed baseline
  // means dragging brings L and R into sync intentionally, never below the
  // previously-higher side.
  const chPath = $derived(ch === null ? null : `/devices/0/inputs/${ch}`);
  const chPathR = $derived(ch === null || !stereo ? null : `/devices/0/inputs/${ch + 1}`);

  function maxPair(prop, fallback) {
    const l = chPath ? (getValue(`${chPath}/${prop}/value`) ?? fallback) : null;
    if (l === null) return null;
    if (!chPathR) return l;
    const r = getValue(`${chPathR}/${prop}/value`) ?? fallback;
    return Math.max(l, r);
  }
  function orPair(prop) {
    const l = chPath ? !!getValue(`${chPath}/${prop}/value`) : false;
    if (!chPathR) return l;
    const r = !!getValue(`${chPathR}/${prop}/value`);
    return l || r;
  }

  const storedFader = $derived(maxPair("FaderLevelTapered", 0.75));
  const storedPan = $derived(chPath ? (getValue(`${chPath}/Pan/value`) ?? 0) : null);
  const storedMute = $derived(orPair("Mute"));
  const storedSolo = $derived(orPair("Solo"));

  let localFader = $state(0.75);
  let localPanNorm = $state(0.5);

  const faderVal = $derived(storedFader === null ? localFader : storedFader);
  // Pan: daemon -1..+1; knob internal 0..1 (0.5 = center). Convert at boundary.
  const panNorm = $derived(storedPan === null ? localPanNorm : (storedPan + 1) / 2);
  const mute = $derived(storedMute);
  const solo = $derived(storedSolo);

  function mirrorWrite(prop, value) {
    if (!chPath) return;
    setValue(`${chPath}/${prop}/value`, value);
    if (chPathR) setValue(`${chPathR}/${prop}/value`, value);
  }

  function writeFader(v) {
    if (chPath) mirrorWrite("FaderLevelTapered", v);
    else localFader = v;
  }
  function writePan(vNorm) {
    if (chPath) setValue(`${chPath}/Pan/value`, vNorm * 2 - 1);
    else localPanNorm = vNorm;
  }
  function writeMute(v) { if (chPath) mirrorWrite("Mute", v); }
  function writeSolo(v) { if (chPath) mirrorWrite("Solo", v); }
</script>

<div class="strip" class:stereo>
  <!-- Pan — daemon exposes one Pan per input, even for stereo pairs. -->
  <Knob min={-100} max={100} format="pan" color="gray"
        value={panNorm} onchange={writePan}
        size={stereo ? 48 : 64} showArc={false} />

  <!-- Solo / Mute -->
  <div class="strip-buttons">
    <ToggleButton label="S" color="green" active={solo} onclick={writeSolo} />
    <ToggleButton label="M" color="red"   active={mute} onclick={writeMute} />
  </div>

  <div class="strip-divider"></div>

  <!-- Fader + meter -->
  <div class="fader-with-meter">
    <Fader color="amber" height={220} value={faderVal} onchange={writeFader} />
    <Meter level={liveLevel} peak={livePeak} height={220} />
  </div>

  <div class="scribble-strip">{name}</div>
</div>

<style>
  .strip {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-sm);
    padding: var(--sp-lg);
    background: var(--bg-surface);
    border-radius: var(--radius-md);
    border: 1px solid var(--bezel-dark);
    width: 130px;
  }

  .strip-buttons {
    display: flex;
    gap: var(--sp-xs);
    width: 100%;
  }
  .strip-buttons :global(.toggle) {
    flex: 1;
    width: auto;
  }

  .strip-divider {
    width: 100%;
    height: 1px;
    background: var(--bezel-dark);
  }

  .fader-with-meter {
    display: flex;
    gap: var(--sp-sm);
    align-items: flex-start;
    justify-content: center;
    width: 100%;
  }

  .scribble-strip {
    background: var(--bg-recessed);
    color: var(--text-value);
    font-family: var(--font-family);
    font-size: var(--font-label);
    font-weight: 700;
    letter-spacing: 1px;
    text-align: center;
    padding: 3px 8px;
    border-radius: var(--radius-xs);
    border: 1px solid var(--bezel-dark);
    width: 100%;
    white-space: nowrap;
  }
</style>
