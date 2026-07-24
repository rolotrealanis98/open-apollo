// Shared dB-to-percent meter scale used by Meter.svelte and StereoMeter.svelte.
// This is the ONLY copy of the tick table / mapping function — do not
// re-derive or duplicate it elsewhere.

// dB scale ticks for the meter (dBFS values and positions). Pct values
// are derived from consoleOS meters — non-linear, higher resolution at top.
export const METER_TICKS = [
  { db: 0, pct: 100 },
  { db: 3, pct: 93 },
  { db: 6, pct: 85 },
  { db: 9, pct: 77 },
  { db: 12, pct: 69 },
  { db: 15, pct: 60 },
  { db: 18, pct: 51 },
  { db: 21, pct: 42 },
  { db: 27, pct: 32 },
  { db: 36, pct: 20 },
  { db: 46, pct: 10 },
  { db: 60, pct: 0 },
];

// Piecewise linear mapping — linear 0..1 → pct on METER_TICKS scale.
// Keeps the visible fill aligned with the dB labels on the scale.
export function linearToScalePct(linear) {
  if (linear <= 0) return 0;
  const db = -(20 * Math.log10(Math.max(linear, 1e-6))); // positive dBFS (e.g. 12 for -12 dBFS)
  // METER_TICKS is sorted: db ascending (0, 3, 6, ..., 60), pct descending (100 → 0)
  if (db <= 0) return 100;
  for (let i = 1; i < METER_TICKS.length; i++) {
    const tickDb = METER_TICKS[i].db;
    if (db <= tickDb) {
      const prevDb = METER_TICKS[i - 1].db;
      const prevPct = METER_TICKS[i - 1].pct;
      const pct = METER_TICKS[i].pct;
      const t = (db - prevDb) / (tickDb - prevDb);
      return prevPct + (pct - prevPct) * t;
    }
  }
  return 0;
}
