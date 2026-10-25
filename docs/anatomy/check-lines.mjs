// Static line-end check for the generated figure — no browser, no Chromium.
//
// The skill's rule: every seam, rule, rib, tick and pointer ends on a stroke, a
// dot or an outline. Its own checker runs in-page; this one computes the same
// invariant over the emitted SVG text, so it needs no Chrome.
//
// An endpoint is satisfied when it lands (within TOL) on:
//   - any polygon/polyline EDGE (not just its vertices),
//   - a circle's circumference or centre,
//   - another line's segment.
// One documented exception: a pointer's tip may stop on a bare face. The needle
// is marked data-free-tip and is allowed exactly one free end.

import { readFileSync } from "node:fs";

const html = readFileSync(process.argv[2], "utf8");
const TOL = Number(process.env.LINE_TOL || 0.8);

const polys = [];   // arrays of [x,y]
const circles = []; // { x, y, r }
const lines = [];   // { x1, y1, x2, y2, freeTip }

for (const m of html.matchAll(/<(polygon|polyline)\b[^>]*?points="([^"]*)"/g)) {
  const pts = m[2].trim().split(/\s+/).map((p) => p.split(",").map(Number));
  const good = pts.filter(([x, y]) => Number.isFinite(x) && Number.isFinite(y));
  if (good.length >= 2) polys.push(good);
}

for (const m of html.matchAll(/<circle\b([^>]*?)\/?>/g)) {
  const at = (n) => {
    const r = new RegExp(n + '="([-\\d.]+)"').exec(m[1]);
    return r ? Number(r[1]) : NaN;
  };
  circles.push({ x: at("cx"), y: at("cy"), r: at("r") });
}

for (const m of html.matchAll(/<line\b([^>]*?)\/?>/g)) {
  const at = (n) => {
    const r = new RegExp(n + '="([-\\d.]+)"').exec(m[1]);
    return r ? Number(r[1]) : NaN;
  };
  lines.push({
    x1: at("x1"), y1: at("y1"), x2: at("x2"), y2: at("y2"),
    freeTip: /data-free-tip="1"/.test(m[1]),
    stroke: (/stroke="([^"]+)"/.exec(m[1]) || [])[1] || null,
  });
}

// Squared distance from point to segment.
function segDist2(px, py, ax, ay, bx, by) {
  const dx = bx - ax, dy = by - ay;
  const len2 = dx * dx + dy * dy;
  let t = len2 === 0 ? 0 : ((px - ax) * dx + (py - ay) * dy) / len2;
  t = Math.max(0, Math.min(1, t));
  const cx = ax + t * dx, cy = ay + t * dy;
  return (px - cx) ** 2 + (py - cy) ** 2;
}

const near = (d2) => d2 <= TOL * TOL;

function landsOnShape(px, py) {
  for (const pts of polys) {
    const n = pts.length;
    for (let i = 0; i < n; i++) {
      const [ax, ay] = pts[i];
      const [bx, by] = pts[(i + 1) % n];
      if (near(segDist2(px, py, ax, ay, bx, by))) return true;
    }
  }
  for (const c of circles) {
    if (!Number.isFinite(c.x) || !Number.isFinite(c.r)) continue;
    const d = Math.hypot(px - c.x, py - c.y);
    // circumference, or the centre dot the eye reads as a knot
    if (Math.abs(d - c.r) <= TOL || d <= TOL) return true;
  }
  for (const l of lines) {
    if (near(segDist2(px, py, l.x1, l.y1, l.x2, l.y2))) return true;
  }
  return false;
}

let free = 0;
const offenders = [];
for (const l of lines) {
  const ends = [
    { x: l.x1, y: l.y1, tip: false },
    { x: l.x2, y: l.y2, tip: true },
  ];
  for (const e of ends) {
    if (!Number.isFinite(e.x) || !Number.isFinite(e.y)) continue;
    if (landsOnShape(e.x, e.y)) continue;
    // documented exception: a pointer may rest its tip on a bare face
    if (l.freeTip && e.tip) continue;
    free++;
    if (offenders.length < 10) {
      offenders.push({ x: +e.x.toFixed(1), y: +e.y.toFixed(1), stroke: l.stroke });
    }
  }
}

const report = {
  lines: lines.length,
  polygons: polys.length,
  circles: circles.length,
  free_endpoints: free,
  offenders,
};
process.stdout.write(JSON.stringify(report, null, 2) + "\n");
process.exit(free === 0 ? 0 : 1);
