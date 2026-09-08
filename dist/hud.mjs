// Pure view-model helpers for the HUD — no DOM, no Tauri.
// Unit-tested by hud.test.mjs (`node --test`).

export const NICE = {
  claude: "Claude Code",
  codex: "Codex",
  gemini: "Gemini",
  cursor: "Cursor",
  copilot: "Copilot",
};

/** Compact token count: 1_234_567 -> "1.2M". */
export function fmtTokens(n) {
  n = Number(n) || 0;
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(0) + "k";
  return String(Math.round(n));
}

/** "$130" over 100, "$12.34" under. */
export function fmtUsd(n) {
  n = Number(n) || 0;
  return "$" + (n >= 100 ? n.toFixed(0) : n.toFixed(2));
}

/** Force a ratio to a finite 0..1; junk (NaN, negative, undefined) -> 0. */
export function clamp01(r) {
  r = Number(r);
  return Number.isFinite(r) && r > 0 ? Math.min(r, 1) : 0;
}

/** Semantic token name for a usage ratio. */
export function ratioTone(r) {
  const x = clamp01(r);
  return x < 0.6 ? "ok" : x < 0.85 ? "warn" : "hot";
}

/** Minutes -> "3h", "1h20", "45m"; null for non-positive/unknown. */
export function shortDuration(min) {
  min = Number(min);
  if (!Number.isFinite(min) || min <= 0) return null;
  const h = Math.floor(min / 60);
  const m = Math.round(min % 60);
  if (!h) return `${m}m`;
  return h < 3 ? `${h}h${String(m).padStart(2, "0")}` : `${h}h`;
}

/** A plan reading older than 45 min (desktop app closed?) is stale. */
export function isStale(observedAtSec, nowMs = Date.now()) {
  const obs = Number(observedAtSec);
  return Number.isFinite(obs) && nowMs / 1000 - obs > 45 * 60;
}

/** Minutes remaining until the 5h block resets — live-recomputed from the
 *  reset instant when the backend sent one. */
export function minutesLeft(snap, nowMs = Date.now()) {
  if (Number.isFinite(snap?.five_h_reset)) {
    return Math.round((snap.five_h_reset * 1000 - nowMs) / 60000);
  }
  const m = Number(snap?.five_h_minutes_left);
  return Number.isFinite(m) ? m : null;
}

/** 0..1 usage ratio for a named window: authoritative % first, then cap ratio. */
export function windowRatio(snap, label, nowMs = Date.now()) {
  const rl = (snap.rate_limits || []).find(
    (r) => r.window_label === label || (label === "5h" && String(r.window_label).startsWith("5")),
  );
  if (rl && Number.isFinite(rl.used_percent)) {
    return { ratio: rl.used_percent / 100, has: true, stale: isStale(rl.observed_at, nowMs) };
  }
  const w = label === "5h" ? snap.five_h : label === "weekly" ? snap.week : snap.hour;
  if (w && w.ratio != null && Number.isFinite(w.ratio)) {
    return { ratio: w.ratio, has: true, stale: false };
  }
  return { ratio: 0, has: false, stale: false };
}

/** The tool to feature in circle mode: first with authoritative limits, else first. */
export function primarySnap(snaps) {
  const list = (snaps || []).filter(Boolean);
  return list.find((s) => (s.rate_limits || []).length) || list[0] || null;
}

/** Ring view-models for circle mode: 5h %, weekly %, and the reset countdown. */
export function ringsFor(snap, nowMs = Date.now()) {
  if (!snap) return [];
  const rings = [];
  for (const label of ["5h", "weekly"]) {
    const { ratio, has, stale } = windowRatio(snap, label, nowMs);
    rings.push({
      key: label,
      label: label === "weekly" ? "wk" : "5h",
      ratio: clamp01(ratio),
      has,
      stale,
      text: has ? Math.round(clamp01(ratio) * 100) + "%" : "–",
      tone: ratioTone(ratio),
      title: `${label} — ${has ? Math.round(clamp01(ratio) * 100) + "% used" : "no data"}${stale ? " (stale)" : ""}`,
    });
  }
  const left = shortDuration(minutesLeft(snap, nowMs));
  if (left) {
    const mins = minutesLeft(snap, nowMs);
    rings.push({
      key: "left",
      label: "left",
      ratio: clamp01(mins / 300),
      has: true,
      stale: false,
      text: left,
      tone: "info",
      title: `5h block resets in ~${left}`,
    });
  }
  return rings;
}

/** Row view-models for card mode. */
export function rowsFor(snap, nowMs = Date.now()) {
  const short = (label) => {
    const l = String(label).toLowerCase();
    if (l.startsWith("5") || l.includes("five")) return "5h";
    if (l.includes("week")) return "wk";
    if (l.includes("month")) return "mo";
    if (l.includes("hour")) return "1h";
    return l.slice(0, 4);
  };
  const auth = snap.rate_limits || [];
  if (auth.length) {
    const rows = auth.map((rl) => {
      const stale = isStale(rl.observed_at, nowMs);
      const pct = Math.max(0, Number(rl.used_percent) || 0);
      return {
        kind: "auth",
        label: short(rl.window_label),
        ratio: clamp01(pct / 100),
        tone: ratioTone(pct / 100),
        value: Math.round(pct) + "%" + (stale ? " old" : ""),
        stale,
      };
    });
    rows.push({
      kind: "note",
      value: `5h ${fmtTokens(snap.five_h.total)} · wk ${fmtTokens(snap.week.total)} · ${fmtUsd(snap.week.cost_usd)}`,
    });
    return rows;
  }
  // No authoritative data → the two real plan windows, cap-relative if set.
  return [
    ["5h", snap.five_h],
    ["wk", snap.week],
  ].map(([label, w]) => {
    if (w.cap != null && w.ratio != null) {
      return {
        kind: "cap",
        label,
        ratio: clamp01(w.ratio),
        tone: ratioTone(w.ratio),
        value: `${fmtTokens(w.total)} / ${fmtTokens(w.cap)}`,
      };
    }
    return {
      kind: "raw",
      label,
      ratio: 0,
      tone: "muted",
      value: `${fmtTokens(w.total)} · ${fmtUsd(w.cost_usd)}`,
    };
  });
}

// ---- DOM rendering (browser only; not imported by the node tests) -----------

const _el = (tag, cls, txt) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (txt != null) e.textContent = txt;
  return e;
};

function _barRow(r) {
  const row = _el("div", `row ${r.kind}${r.stale ? " stale" : ""}`);
  if (r.kind === "note") {
    row.append(_el("div", "v", r.value));
    return row;
  }
  const bar = _el("div", "bar");
  const span = _el("span", r.tone);
  span.style.width = (Math.min(r.ratio, 1) * 100).toFixed(1) + "%";
  bar.append(span);
  row.append(_el("div", "k", r.label), bar, _el("div", "v", r.value));
  return row;
}

function _toolBlock(s, nowMs) {
  const wrap = _el("div", "tool");
  const name = _el("div", "name");
  name.append(
    _el("span", null, NICE[s.tool] || s.tool),
    _el("span", "cost", "wk " + fmtUsd(s.week.cost_usd)),
  );
  wrap.append(name);
  rowsFor(s, nowMs).forEach((r) => wrap.append(_barRow(r)));
  (s.advisories || []).forEach((a) => {
    const d = _el("div", "adv " + a.severity);
    d.append(_el("span", "dot"), _el("span", null, a.text));
    wrap.append(d);
  });
  return wrap;
}

const _R = 24;
const _C = 2 * Math.PI * _R;
function _ringEl(r) {
  const w = _el("div", `ring${r.stale ? " stale" : ""}`);
  w.title = r.title;
  w.innerHTML =
    `<svg viewBox="0 0 60 60" width="60" height="60">` +
    `<circle class="track" cx="30" cy="30" r="${_R}" fill="none" stroke-width="5"/>` +
    `<circle class="fill ${r.tone}" cx="30" cy="30" r="${_R}" fill="none" stroke-width="5" ` +
    `stroke-dasharray="${_C.toFixed(1)}" stroke-dashoffset="${(r.has ? _C * (1 - r.ratio) : _C).toFixed(1)}"/>` +
    `</svg><div class="lbl"><b>${r.text}</b><i>${r.label}</i></div>`;
  return w;
}

/** Render circle mode into `ringsEl`. */
export function renderCircle(ringsEl, snaps, nowMs = Date.now()) {
  ringsEl.innerHTML = "";
  ringsFor(primarySnap(snaps), nowMs).forEach((r) => ringsEl.append(_ringEl(r)));
}

/** Render card mode into `bodyEl`; returns a footer string (or ""). */
export function renderCard(bodyEl, snaps, nowMs = Date.now()) {
  bodyEl.innerHTML = "";
  const list = (snaps || []).filter(Boolean);
  if (!list.length) {
    bodyEl.append(_el("div", "empty", "no CLI usage found"));
    return "";
  }
  list.forEach((s, i) => {
    if (i) bodyEl.append(_el("div", "sep"));
    bodyEl.append(_toolBlock(s, nowMs));
  });
  return new Date(nowMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
