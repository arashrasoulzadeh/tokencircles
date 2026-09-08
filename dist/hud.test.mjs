import { test } from "node:test";
import assert from "node:assert/strict";
import {
  fmtTokens,
  fmtUsd,
  clamp01,
  ratioTone,
  shortDuration,
  isStale,
  minutesLeft,
  windowRatio,
  primarySnap,
  ringsFor,
  rowsFor,
} from "./hud.mjs";

test("fmtTokens scales with magnitude", () => {
  assert.equal(fmtTokens(0), "0");
  assert.equal(fmtTokens(940), "940");
  assert.equal(fmtTokens(12_345), "12k");
  assert.equal(fmtTokens(1_240_000), "1.2M");
  assert.equal(fmtTokens(527_000_000), "527.0M");
  assert.equal(fmtTokens(3_400_000_000), "3.40B");
  assert.equal(fmtTokens("bad"), "0");
});

test("fmtUsd switches precision at $100", () => {
  assert.equal(fmtUsd(0), "$0.00");
  assert.equal(fmtUsd(12.3456), "$12.35");
  assert.equal(fmtUsd(130.7), "$131");
  assert.equal(fmtUsd(undefined), "$0.00");
});

test("clamp01 rejects junk and clamps range", () => {
  assert.equal(clamp01(0.42), 0.42);
  assert.equal(clamp01(-1), 0);
  assert.equal(clamp01(2), 1);
  assert.equal(clamp01(NaN), 0);
  assert.equal(clamp01(undefined), 0);
});

test("ratioTone thresholds", () => {
  assert.equal(ratioTone(0.1), "ok");
  assert.equal(ratioTone(0.59), "ok");
  assert.equal(ratioTone(0.6), "warn");
  assert.equal(ratioTone(0.84), "warn");
  assert.equal(ratioTone(0.85), "hot");
  assert.equal(ratioTone(5), "hot");
});

test("shortDuration formats hours and minutes", () => {
  assert.equal(shortDuration(0), null);
  assert.equal(shortDuration(-3), null);
  assert.equal(shortDuration(45), "45m");
  assert.equal(shortDuration(80), "1h20");
  assert.equal(shortDuration(184), "3h");
  assert.equal(shortDuration("nope"), null);
});

test("isStale over 45 minutes", () => {
  const now = 1_000_000_000_000;
  assert.equal(isStale(now / 1000 - 10 * 60, now), false);
  assert.equal(isStale(now / 1000 - 46 * 60, now), true);
  assert.equal(isStale(undefined, now), false);
});

test("minutesLeft prefers the live reset instant", () => {
  const now = 1_000_000_000_000;
  assert.equal(minutesLeft({ five_h_reset: now / 1000 + 3600 }, now), 60);
  assert.equal(minutesLeft({ five_h_minutes_left: 42 }, now), 42);
  assert.equal(minutesLeft({}, now), null);
});

const rl = (label, pct, obs) => ({
  window_label: label,
  used_percent: pct,
  observed_at: obs ?? Math.floor(Date.now() / 1000),
});
const wstat = (over) => ({ total: 0, cost_usd: 0, cap: null, ratio: null, ...over });

test("windowRatio: authoritative % beats cap ratio", () => {
  const snap = {
    rate_limits: [rl("5h", 18)],
    five_h: wstat({ cap: 100, ratio: 0.9 }),
    week: wstat({}),
  };
  const r = windowRatio(snap, "5h");
  assert.equal(r.has, true);
  assert.equal(r.ratio, 0.18);
});

test("windowRatio: falls back to cap ratio, then to no-data", () => {
  const snap = { rate_limits: [], five_h: wstat({ cap: 200, ratio: 0.25 }), week: wstat({}) };
  assert.deepEqual(windowRatio(snap, "5h"), { ratio: 0.25, has: true, stale: false });
  assert.deepEqual(windowRatio(snap, "weekly"), { ratio: 0, has: false, stale: false });
});

test("primarySnap prefers a tool with authoritative limits", () => {
  const a = { tool: "codex", rate_limits: [] };
  const b = { tool: "claude", rate_limits: [rl("5h", 10)] };
  assert.equal(primarySnap([a, b]).tool, "claude");
  assert.equal(primarySnap([a]).tool, "codex");
  assert.equal(primarySnap([]), null);
});

test("ringsFor builds 5h + weekly + countdown", () => {
  const now = Date.now();
  const snap = {
    tool: "claude",
    rate_limits: [rl("5h", 18), rl("weekly", 17)],
    five_h: wstat({}),
    week: wstat({}),
    five_h_reset: Math.floor(now / 1000) + 184 * 60,
  };
  const rings = ringsFor(snap, now);
  assert.equal(rings.length, 3);
  assert.deepEqual(
    rings.map((r) => [r.label, r.text]),
    [
      ["5h", "18%"],
      ["wk", "17%"],
      ["left", "3h"],
    ],
  );
  assert.equal(rings[0].tone, "ok");
});

test("ringsFor omits the countdown when there is no reset", () => {
  const snap = { rate_limits: [rl("5h", 50)], five_h: wstat({}), week: wstat({}) };
  assert.equal(ringsFor(snap).length, 2);
});

test("ringsFor marks a stale reading and shows a dash for missing data", () => {
  const oldObs = Math.floor(Date.now() / 1000) - 60 * 60;
  const snap = {
    rate_limits: [rl("5h", 30, oldObs)],
    five_h: wstat({}),
    week: wstat({}),
  };
  const rings = ringsFor(snap);
  assert.equal(rings[0].stale, true);
  assert.equal(rings[1].text, "–"); // weekly has no data
});

test("rowsFor: authoritative mode shows % rows plus a totals note", () => {
  const snap = {
    rate_limits: [rl("5h", 18), rl("weekly", 17)],
    five_h: wstat({ total: 41_000_000 }),
    week: wstat({ total: 527_000_000, cost_usd: 130 }),
  };
  const rows = rowsFor(snap);
  assert.deepEqual(
    rows.map((r) => r.kind),
    ["auth", "auth", "note"],
  );
  assert.deepEqual(
    rows.slice(0, 2).map((r) => r.label),
    ["5h", "wk"],
  );
  assert.equal(rows[0].value, "18%");
  assert.equal(rows[0].tone, "ok");
  assert.match(rows[2].value, /wk 527\.0M · \$130/);
});

test("rowsFor: a stale reading is flagged", () => {
  const old = Math.floor(Date.now() / 1000) - 60 * 60;
  const snap = {
    rate_limits: [rl("weekly", 40, old)],
    five_h: wstat({}),
    week: wstat({ total: 1, cost_usd: 0 }),
  };
  const rows = rowsFor(snap);
  assert.equal(rows[0].stale, true);
  assert.match(rows[0].value, /old$/);
});

test("rowsFor: no auth data → cap rows when a cap is set, raw otherwise", () => {
  const capped = {
    rate_limits: [],
    five_h: wstat({ total: 100, cap: 400, ratio: 0.25 }),
    week: wstat({ total: 10, cost_usd: 1 }),
  };
  const rows = rowsFor(capped);
  assert.equal(rows[0].kind, "cap");
  assert.equal(rows[0].value, "100 / 400");
  assert.equal(rows[1].kind, "raw");
  assert.equal(rows[1].value, "10 · $1.00");
});
