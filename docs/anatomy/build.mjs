// LeIndex Anatomy — "The Spindle"
// A framework-free isometric figure of LeIndex v2.1.1 as an open 8-station
// spindle lathe. Everything the readout shows is derived from the model below;
// no number in the drawing is typed by hand.
//
// Build: node docs/anatomy/build.mjs   (writes leindex-anatomy.html next to it)

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

// ── The model ────────────────────────────────────────────────────────────────
// Source: LeIndex @ Cargo.toml version 2.1.1. Counts read from the index.
const MODEL = {
  version: "2.1.1",
  files: 581,          // project inventory (phase 1)
  modules: 21,         // pub mod in src/lib.rs
  gated: 4,            // embed, intel, server, migration-era gates
  parsers: 17,         // parser_for_language arms
  gateSites: 458,      // cfg(feature = ...) occurrences
  gateFiles: 64,       // files carrying them
  features: 23,        // members of `full`
  layers: 8,           // manifest LayerKind variants
  testFiles: 39,       // integration test files
  tests: 312,          // test declarations in them
  routers: 5,          // grouped MCP tools
  handlers: 20,        // underlying handlers
  publicItems: 1269,   // pub struct/enum/trait/fn across 269 files
  edition: "2024",
  msrv: "1.88",
};

// The eight index phases (scheduler/index_job.rs IndexPhaseId), in order.
const PHASES = [
  { id: "Scan", label: "scan the tree" },
  { id: "Parse", label: "tree-sitter parse" },
  { id: "Pdg", label: "build the graph" },
  { id: "Lexical", label: "TF-IDF rows" },
  { id: "PublishCore", label: "publish core" },
  { id: "Neural", label: "neural rows" },
  { id: "PublishEnhanced", label: "publish enhanced" },
  { id: "Complete", label: "checkpoint" },
];

// The three execution providers that can own the neural layer.
const PROVIDERS = [
  { id: "cpu", label: "CPU", note: "onnxruntime" },
  { id: "cuda", label: "CUDA", note: "onnxruntime-gpu" },
  { id: "migraphx", label: "MIGraphX", note: "ROCm" },
];

const N = PHASES.length;
const STATIONS = N;

// ── Isometric projection ─────────────────────────────────────────────────────
const C30 = Math.cos(Math.PI / 6); // 0.8660
const S30 = 0.5;
const proj = (x, y, z) => [(x - y) * C30, (x + y) * S30 - z];
const pt = (p) => `${p[0].toFixed(2)},${p[1].toFixed(2)}`;
const poly = (pts, fill, extra = "") =>
  `<polygon points="${pts.map(pt).join(" ")}" fill="${fill}"${extra}/>`;

// A solid box. Sizes in world units; (x,y) is the near-bottom corner, z the
// base. Returns the three visible faces, painted top → +x → +y so the nearer
// face wins where they meet.
function box(x, y, z, w, d, h, tones) {
  const [top, xf, yf] = tones;
  const A = [x, y, z], B = [x + w, y, z], Cc = [x + w, y + d, z], D = [x, y + d, z];
  const A2 = [x, y, z + h], B2 = [x + w, y, z + h], C2 = [x + w, y + d, z + h], D2 = [x, y + d, z + h];
  let out = "";
  out += poly([A2, B2, C2, D2], top);   // top
  out += poly([B, B2, C2, Cc], xf);     // +x face (right)
  out += poly([D, D2, C2, Cc], yf);     // +y face (left)
  return out;
}

// A true projected circle perpendicular to an axis, sampled through proj so it
// sits exactly in the isometric plane rather than being faked with a squashed
// SVG ellipse.
function ring(cx, cy, cz, r, axis, fill, extra = "", steps = 28) {
  const pts = [];
  for (let i = 0; i < steps; i++) {
    const t = (i / steps) * Math.PI * 2;
    const c = Math.cos(t) * r, s = Math.sin(t) * r;
    if (axis === "x") pts.push(proj(cx, cy + c, cz + s));
    else if (axis === "y") pts.push(proj(cx + c, cy, cz + s));
    else pts.push(proj(cx + c, cy + s, cz));
  }
  return poly(pts, fill, extra);
}

const tones = {
  steel: ["#3c5c78", "#28415a", "#1b2d40"],
  steelDark: ["#2b435c", "#1d3044", "#142131"],
  navy: ["#22384f", "#17293d", "#0f1d2c"],
  base: ["#1e3348", "#0e1a26", "#09121b"],
  brass: ["#c9a15c", "#a37c3d", "#7a5a29"],
  glass: ["#a9d7e4", "#7fb4c6", "#5b8a9c"],
  obsidian: ["#121c28", "#0b131c", "#060b11"],
};

// ── Scene layout ─────────────────────────────────────────────────────────────
const S = {
  baseX: 0, baseY: 0, baseW: 300, baseD: 170, baseH: 12,
  railY1: 26, railY2: 148, railH: 16, railD: 16,
  headX: 0, headW: 44,
  tailX: 244, tailW: 34,
  axisY: 86, axisZ: 104, axisR: 9,
  bedZ: 12,
  gateXs: [96, 152, 208],
  gateW: 5,
};

const stationX = (i) => 58 + i * 24;

// ── Painter's order ──────────────────────────────────────────────────────────
// Every drawable is pushed with a depth key; larger = nearer. The key is the
// centroid's (x + y), which is exactly the projection's depth axis.
const parts = [];
const add = (key, svg, kind = "solid") => parts.push({ key, svg, kind });

function buildBed() {
  const k = S.baseX + S.baseW / 2 + S.baseY + S.baseD / 2;
  // feet (4 pads) — drawn first, under the plate
  const feet = [
    [10, 10], [S.baseW - 30, 10], [10, S.baseD - 30], [S.baseW - 30, S.baseD - 30],
  ];
  for (const [fx, fy] of feet) {
    add(-10, box(fx, fy, 0, 20, 20, 6, tones.steelDark));
  }
  add(k - 200, box(S.baseX, S.baseY, 6, S.baseW, S.baseD, S.baseH, tones.base));
  // bed rails
  add(k - 100, box(0, S.railY1, S.bedZ + S.baseH, S.baseW, S.railD, S.railH, tones.steel));
  add(k - 100, box(0, S.railY2, S.bedZ + S.baseH, S.baseW, S.railD, S.railH, tones.steel));
  // ruler strip: 24 ticks along the near rail
  let ticks = "";
  for (let i = 0; i < 24; i++) {
    const tx = 8 + i * 12.2;
    const tall = i % 4 === 0;
    const a = proj(tx, S.railY2 + S.railD, S.bedZ + S.baseH + S.railH);
    const b = proj(tx, S.railY2 + S.railD, S.bedZ + S.baseH + S.railH - (tall ? 7 : 4));
    ticks += `<line x1="${a[0].toFixed(2)}" y1="${a[1].toFixed(2)}" x2="${b[0].toFixed(2)}" y2="${b[1].toFixed(2)}" stroke="#c9d8e6" stroke-opacity="0.55" stroke-width="1"/>`;
  }
  add(k - 90, ticks, "line");
  // corner screws (4)
  for (const [sx, sy] of [[8, 8], [S.baseW - 12, 8], [8, S.baseD - 12], [S.baseW - 12, S.baseD - 12]]) {
    add(k - 199, ring(sx, sy, S.baseH + 6, 3.2, "z", "#0a1219"));
    add(k - 198, ring(sx, sy, S.baseH + 6, 1.4, "z", "#6b7f93"));
  }
}

function buildHeadstock() {
  const x = S.headX, w = S.headW;
  add(x + w / 2 + 68, box(x, 20, 12, w, 130, 128, tones.steel));
  // top cap + bearing boss
  add(x + w / 2 + 68, box(x + 4, 30, 140, w - 8, 110, 10, tones.steelDark));
  add(x + w / 2 + 60, ring(w, S.axisY, S.axisZ, 26, "x", "#2f4d68"));
  add(x + w / 2 + 60, ring(w, S.axisY, S.axisZ, 19, "x", "#0d1a26"));
  // chuck jaws: three radial blocks on the spindle nose
  const jaw = (ang, len) => {
    const cx = w + 2;
    const cy = S.axisY + Math.cos(ang) * 15;
    const cz = S.axisZ + Math.sin(ang) * 15;
    const a = proj(cx, cy, cz);
    const b = proj(cx + 14, cy + Math.cos(ang) * len, cz + Math.sin(ang) * len);
    return `<line x1="${a[0].toFixed(2)}" y1="${a[1].toFixed(2)}" x2="${b[0].toFixed(2)}" y2="${b[1].toFixed(2)}" stroke="#c9a15c" stroke-width="4.5" stroke-linecap="round"/>`;
  };
  let jaws = "";
  for (let j = 0; j < 3; j++) jaws += jaw((j / 3) * Math.PI * 2 + 0.4, 16);
  add(w + 72, jaws, "line");
}

function buildSpindle() {
  // The work axis: 8 banded segments, one per phase station.
  for (let i = 0; i < STATIONS; i++) {
    const x0 = stationX(i) - 11;
    const x1 = x0 + 22;
    const y = S.axisY, z = S.axisZ, r = S.axisR;
    const seg = poly(
      [
        proj(x0, y - r, z + r), proj(x1, y - r, z + r),
        proj(x1, y + r, z + r), proj(x0, y + r, z + r),
      ],
      i % 2 ? "#31506c" : "#2a465f",
    );
    add(x0 + 11 + y + r, seg);
    add(x0 + 11 + y + r, ring(x0, y, z, r, "x", "#1a2c3e"));
    // the station bore: a small glass ring the tool pushes into
    add(x0 + 12 + y + r, ring(x0 + 1, y, z, r * 0.52, "x", "#0a141d"));
  }
  // tailstock end cap
  add(stationX(STATIONS - 1) + 12 + S.axisY + S.axisR,
    ring(stationX(STATIONS - 1) + 12, S.axisY, S.axisZ, S.axisR, "x", "#3a5a76"));
}

function buildStations() {
  // 8 tool holders: a post on the near rail, a carriage, a bit aimed at the
  // spindle. Each carries data-station so motion updates only its transforms.
  for (let i = 0; i < STATIONS; i++) {
    const sx = stationX(i);
    const key = sx + S.railY2;
    add(key, box(sx - 8, S.railY2 + 4, S.bedZ + S.baseH + S.railH, 16, 30, 26, tones.steelDark));
    add(key + 1, `<g data-station="${i}" data-carriage="1">` +
      box(sx - 6, S.railY2 + 6, S.bedZ + S.baseH + S.railH + 26, 12, 24, 30, tones.brass) +
      `</g>`, "group");
    // the bit: a slim probe from the carriage toward the axis
    add(key + 2, `<g data-station="${i}" data-bit="1">` +
      (() => {
        const a = proj(sx, S.railY2 + 18, S.bedZ + S.baseH + S.railH + 56);
        const b = proj(sx, S.axisY + 2, S.axisZ - S.axisR);
        return `<line x1="${a[0].toFixed(2)}" y1="${a[1].toFixed(2)}" x2="${b[0].toFixed(2)}" y2="${b[1].toFixed(2)}" stroke="#e8eef4" stroke-width="2" stroke-linecap="round"/>`;
      })() + `</g>`, "group");
  }
  // station index plates: one small tick each, engraved, no words
  let marks = "";
  for (let i = 0; i < STATIONS; i++) {
    const p = proj(stationX(i), S.railY2 + 2, S.bedZ + S.baseH + S.railH + 30);
    marks += `<circle cx="${p[0].toFixed(2)}" cy="${p[1].toFixed(2)}" r="${i === 0 ? 2.4 : 1.5}" fill="#0b1622"/>`;
  }
  add(400, marks, "line");
}

function buildTailstock() {
  const x = S.tailX;
  add(x + 17 + S.axisY, box(x, 40, 12, 34, 92, 96, tones.steel));
  add(x + 17 + S.axisY, ring(x, S.axisY, S.axisZ, 13, "x", "#2f4d68"));
  add(x + 17 + S.axisY, ring(x, S.axisY, S.axisZ, 7, "x", "#0d1a26"));
  // quill hand-wheel
  add(x + 30 + S.axisY, ring(x + 34, 150, 74, 15, "y", "#2b435c"));
  add(x + 30 + S.axisY, ring(x + 34, 150, 74, 9, "y", "#142131"));
  // the precision dial on the tailstock: "TIER-0 / PRECISION"
  const c = proj(x + 6, 60, 150);
  add(c[0] + c[1] + x, `<g data-precision="1">` +
    `<circle cx="${c[0].toFixed(2)}" cy="${c[1].toFixed(2)}" r="17" fill="#0f1e2c" stroke="#3c5c78" stroke-width="1.5"/>` +
    `<circle cx="${c[0].toFixed(2)}" cy="${c[1].toFixed(2)}" r="13" fill="none" stroke="#6b8499" stroke-opacity="0.5"/>` +
    `<line data-needle="1" data-free-tip="1" data-cx="${c[0].toFixed(2)}" data-cy="${c[1].toFixed(2)}" x1="${c[0].toFixed(2)}" y1="${c[1].toFixed(2)}" x2="${c[0].toFixed(2)}" y2="${(c[1] - 12).toFixed(2)}" stroke="#e2eeea" stroke-width="1.6" stroke-linecap="round"/>` +
    `</g>`, "group");
}

function buildGates() {
  // 3 obsidian laminations over the bed: the execution providers. Each is a
  // thin plate that drops between two stations when closed.
  for (let g = 0; g < S.gateXs.length; g++) {
    const gx = S.gateXs[g];
    const y0 = 18, y1 = 152, z0 = 26, z1 = 186;
    const face = [[gx, y0, z1], [gx, y1, z1], [gx, y1, z0], [gx, y0, z0]].map(([a, b, c]) => proj(a, b, c));
    const edge = [[gx, y1, z1], [gx + S.gateW, y1, z1], [gx + S.gateW, y1, z0], [gx, y1, z0]].map(([a, b, c]) => proj(a, b, c));
    const cy = proj(gx, (y0 + y1) / 2, (z0 + z1) / 2);
    add(cy[0] + cy[1] + 40, `<g data-gate="${g}">` +
      poly(face, "#0d1725", ` stroke="#27425c" stroke-width="1.4" opacity="0.92"`) +
      poly(edge, "#1a2c3e") +
      // hatch lines on the plate, so it reads as a solid part not a slab
      (() => {
        let h = "";
        for (let i = 1; i < 7; i++) {
          const zz = z0 + ((z1 - z0) * i) / 7;
          const a = proj(gx + S.gateW, y0 + 10, zz), b = proj(gx + S.gateW, y1 - 10, zz);
          h += `<line x1="${a[0].toFixed(2)}" y1="${a[1].toFixed(2)}" x2="${b[0].toFixed(2)}" y2="${b[1].toFixed(2)}" stroke="#4d6d8a" stroke-opacity="0.28" stroke-width="1"/>`;
        }
        return h;
      })() +
      `</g>`, "group");
    // gate posts: the frame the plate hangs from
    add(-50, box(gx - 3, 14, S.baseH, S.gateW + 6, 8, 180, tones.navy));
    add(-50, box(gx - 3, 148, S.baseH, S.gateW + 6, 8, 180, tones.navy));
  }
}

function buildRosette() {
  // The phase rosette: 8 teeth, one lit per active phase.
  const cx = -62, cy = 92, cz = 34, r1 = 20, r2 = 33;
  const teeth = [];
  for (let i = 0; i < N; i++) {
    const a0 = (i / N) * Math.PI * 2, a1 = ((i + 0.5) / N) * Math.PI * 2;
    teeth.push([cx + Math.cos(a0) * r1, cy + Math.sin(a0) * r1, cz]);
    teeth.push([cx + Math.cos(a1) * r2, cy + Math.sin(a1) * r2, cz]);
  }
  add(-200, poly(teeth.map(([a, b, c]) => proj(a, b, c)), "#22384f", ` stroke="#3c5c78" stroke-width="1"`), "solid");
  // lit wedge, redrawn on phase change
  const litAt = (i) => {
    const a0 = (i / N) * Math.PI * 2, a1 = ((i + 0.5) / N) * Math.PI * 2;
    const p = [
      proj(cx + Math.cos(a0) * r1, cy + Math.sin(a0) * r1, cz),
      proj(cx + Math.cos(a1) * r2, cy + Math.sin(a1) * r2, cz),
      proj(cx + Math.cos(a1) * r1, cy + Math.sin(a1) * r1, cz),
    ];
    return poly(p, "#f2b455");
  };
  add(-199, `<g data-rosette="1">${litAt(0)}</g>`, "group");
  add(-198, ring(cx, cy, cz, 8, "z", "#0f1e2c"), "solid");
  // stand for the rosette
  add(-201, box(cx - 5, cy - 5, S.baseH, 10, 10, cz - S.baseH, tones.steelDark));
  // phase ticks around it
  let ticks = "";
  for (let i = 0; i < N; i++) {
    const a = (i / N) * Math.PI * 2;
    const p1 = proj(cx + Math.cos(a) * (r2 + 4), cy + Math.sin(a) * (r2 + 4), cz);
    const p2 = proj(cx + Math.cos(a) * (r2 + 10), cy + Math.sin(a) * (r2 + 10), cz);
    ticks += `<line x1="${p1[0].toFixed(2)}" y1="${p1[1].toFixed(2)}" x2="${p2[0].toFixed(2)}" y2="${p2[1].toFixed(2)}" stroke="#6b8499" stroke-width="1.2"/>`;
  }
  add(-197, ticks, "line");
}

function buildCable() {
  // The data cable: from the tailstock end of the spindle, down the base, to
  // the rosette stand. Drawn as a sampled tube route so it reads as a cable.
  const route = [
    [S.tailX + 20, S.axisY, S.axisZ - 30],
    [S.tailX + 30, S.axisY + 20, 60],
    [S.tailX + 10, 60, 26],
    [140, 30, 22],
    [40, 22, 22],
    [-30, 40, 26],
    [-56, 80, 30],
  ];
  const samples = [];
  for (let i = 0; i < route.length - 1; i++) {
    const [a, b] = [route[i], route[i + 1]];
    for (let t = 0; t < 8; t++) {
      const u = t / 8;
      samples.push([
        a[0] + (b[0] - a[0]) * u,
        a[1] + (b[1] - a[1]) * u,
        a[2] + (b[2] - a[2]) * u,
      ]);
    }
  }
  samples.push(route[route.length - 1]);
  const d = samples.map(([x, y, z], i) => `${i ? "L" : "M"}${pt(proj(x, y, z))}`).join(" ");
  add(120, `<path d="${d}" fill="none" stroke="#101d2a" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/>`, "line");
  add(121, `<path d="${d}" fill="none" stroke="#2f4d68" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round"/>`, "line");
  add(122, `<path d="${d}" fill="none" stroke="#7fb4c6" stroke-opacity="0.5" stroke-width="1.4" stroke-linecap="round"/>`, "line");
}

// ── Assemble ─────────────────────────────────────────────────────────────────
buildBed();
buildGates();
buildHeadstock();
buildSpindle();
buildStations();
buildTailstock();
buildRosette();
buildCable();

parts.sort((a, b) => a.key - b.key);

// fit the drawing into the card
const VB_W = 1180, VB_H = 760;
let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
const allPts = parts
  .map((p) => p.svg)
  .join("")
  .match(/-?\d+\.\d+,-?\d+\.\d+/g) || [];
for (const s of allPts) {
  const [x, y] = s.split(",").map(Number);
  if (x < minX) minX = x; if (x > maxX) maxX = x;
  if (y < minY) minY = y; if (y > maxY) maxY = y;
}
const PAD = 56;
const w = maxX - minX, h = maxY - minY;
const scale = Math.min((VB_W - PAD * 2) / w, (VB_H - PAD * 2) / h);
const tx = VB_W / 2 - ((minX + maxX) / 2) * scale;
const ty = VB_H / 2 - ((minY + maxY) / 2) * scale;

const body = parts.map((p) => p.svg).join("\n");

const HTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>LeIndex ${MODEL.version} — The Spindle</title>
<style>
  :root{
    --ink:#e8eef4; --paper:#070b11; --card:#0a1018; --line:#1c2735;
    --muted:#8fa3b8; --accent:#f2b455; --cool:#7fb4c6;
    --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    --sans: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  }
  *{box-sizing:border-box}
  html,body{margin:0;padding:0;background:var(--paper);color:var(--ink);font-family:var(--sans)}
  body{padding:40px 24px 64px;display:flex;flex-direction:column;align-items:center;gap:22px}
  .card{width:min(1180px,100%);background:linear-gradient(180deg,#0b131d 0%,#080e15 100%);
    border:1px solid var(--line);border-radius:14px;overflow:hidden;
    box-shadow:0 30px 80px -40px #000, inset 0 1px 0 #ffffff0a}
  .bar{display:flex;align-items:baseline;justify-content:space-between;gap:16px;
    padding:14px 20px;border-bottom:1px solid var(--line);font-family:var(--mono);font-size:12px;letter-spacing:.08em}
  .bar .fig{color:var(--accent)}
  .bar .title{color:var(--muted);text-transform:uppercase}
  .stage{padding:0}
  svg{display:block;width:100%;height:auto;touch-action:none}
  .foot{display:flex;align-items:center;justify-content:space-between;gap:16px;
    padding:12px 20px;border-top:1px solid var(--line);font-family:var(--mono);font-size:12px}
  .hint{color:var(--muted)}
  .readout{color:var(--accent);font-variant-numeric:tabular-nums}
  .legend{width:min(1180px,100%);display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:14px 28px;font-size:13.5px;line-height:1.55}
  .legend h3{margin:0 0 4px;font-family:var(--mono);font-size:11px;letter-spacing:.12em;text-transform:uppercase;color:var(--cool)}
  .legend p{margin:0;color:#c3d2e0}
  .legend .k{color:var(--accent);font-family:var(--mono);font-size:12px}
  .caption{width:min(1180px,100%);color:var(--muted);font-size:13px;line-height:1.6;margin:0}
  .caption b{color:#dbe7f2}
  .swatch{display:inline-block;width:9px;height:9px;border-radius:2px;margin-right:6px;vertical-align:baseline}
  @media (prefers-reduced-motion: reduce){ *{transition:none!important} }
</style>
</head>
<body>

<div class="card">
  <div class="bar">
    <span class="fig">Fig 1</span>
    <span class="title">LeIndex ${MODEL.version} &middot; the spindle</span>
  </div>
  <div class="stage">
    <svg id="fig" viewBox="0 0 ${VB_W} ${VB_H}" role="img"
         aria-label="An open eight-station spindle lathe: a rotating work axis with eight tool stations, three obsidian gates hanging over the bed, a tailstock carrying a precision dial, and an eight-tooth phase rosette on the left.">
      <defs>
        <radialGradient id="halo" cx="50%" cy="50%" r="50%">
          <stop offset="0%" stop-color="#0e1a26" stop-opacity="0.85"/>
          <stop offset="100%" stop-color="#0e1a26" stop-opacity="0"/>
        </radialGradient>
        <linearGradient id="bedGrad" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stop-color="#16273a"/><stop offset="100%" stop-color="#0a141f"/>
        </linearGradient>
      </defs>
      <ellipse cx="${VB_W / 2}" cy="${VB_H * 0.78}" rx="430" ry="90" fill="url(#halo)"/>
      <g id="scene" transform="translate(${tx.toFixed(2)},${ty.toFixed(2)}) scale(${scale.toFixed(4)})">
${body}
      </g>
    </svg>
  </div>
  <div class="foot">
    <span class="hint">drag along the bed &middot; click a gate &middot; <b>←</b><b>→</b> step &middot; <b>space</b> gate &middot; <b>r</b> reset</span>
    <span class="readout" id="readout">station 1/${N} &middot; ${PHASES[0].id.toLowerCase()} &middot; ${PHASES[0].label} &middot; 1/${PROVIDERS.length} gates shut</span>
  </div>
</div>

<div class="legend">
  <div><h3>The axis</h3><p>One node-level corpus, banded into <span class="k">8</span> phase stations from scan to checkpoint. Every station turns on the same <span class="k">${MODEL.files}</span>-file inventory.</p></div>
  <div><h3>The gates</h3><p>Three obsidian laminations: <span class="k">cpu</span>, <span class="k">cuda</span>, <span class="k">migraphx</span>. A closed gate means that provider owns the neural layer; the others stay shut.</p></div>
  <div><h3>The tailstock</h3><p>The precision layer. It dials in only when a language indexer is present; otherwise it retracts and the <span class="k">tier-0</span> graph answers.</p></div>
  <div><h3>The rosette</h3><p>Eight teeth, one per index phase, lit in order. The job is checkpointed: pull the plug and it resumes on the lit tooth.</p></div>
  <div><h3>Five routers</h3><p><span class="k">${MODEL.handlers}</span> handlers are advertised as <span class="k">${MODEL.routers}</span> routers, one discriminator each, so a model picks an operation instead of a synonym.</p></div>
  <div><h3>The ledger</h3><p><span class="k">${MODEL.layers}</span> manifest layers, <span class="k">${MODEL.gateSites}</span> feature-gate sites across <span class="k">${MODEL.gateFiles}</span> files, <span class="k">${MODEL.parsers}</span> language parsers.</p></div>
</div>

<p class="caption">
  <b>The object is the explanation.</b> LeIndex is one work axis that eight stations take turns cutting:
  scan, parse, graph, lexical, publish, neural, publish, checkpoint. The gates over the bed are the three
  execution providers — exactly one is open, and the neural station only cuts when one is. The tailstock is
  precision: it advances only when an external indexer is on the bench, and the tier-0 graph carries on without it.
  Nothing here is decorative: the band count is the phase count, the tooth count is the phase count, the gate count
  is the provider count, and the ruler has a tick per twelve files of the ${MODEL.files}-file inventory.
</p>

<script>
(() => {
  const M = ${JSON.stringify(MODEL)};
  const PHASES = ${JSON.stringify(PHASES)};
  const N = ${N};
  const svg = document.getElementById("fig");
  const scene = document.getElementById("scene");
  const readout = document.getElementById("readout");
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;

  const state = {
    step: 0, stepTarget: 0,
    gates: [false, true, true],   // gates[i] === true  => shut
    activeGate: 0,
    precision: 1 / 3,
  };

  // Cache the handful of nodes motion touches — never re-render the scene.
  const carriages = [...scene.querySelectorAll("[data-carriage]")];
  const bits = [...scene.querySelectorAll("[data-bit]")];
  const gates = [...scene.querySelectorAll("[data-gate]")];
  const rosette = scene.querySelector("[data-rosette]");
  const needle = scene.querySelector("[data-needle]");

  // The work axis reads as turning through its band ticks, not by rotating the
  // bed: the whole scene would have to rotate about a line through it.
  function litRosette(i) {
    const cx = -62, cy = 92, cz = 34, r1 = 20, r2 = 33;
    const C30 = Math.cos(Math.PI / 6), S30 = 0.5;
    const pj = (x, y, z) => [(x - y) * C30, (x + y) * S30 - z];
    const a0 = (i / N) * Math.PI * 2, a1 = ((i + 0.5) / N) * Math.PI * 2;
    const pts = [
      pj(cx + Math.cos(a0) * r1, cy + Math.sin(a0) * r1, cz),
      pj(cx + Math.cos(a1) * r2, cy + Math.sin(a1) * r2, cz),
      pj(cx + Math.cos(a1) * r1, cy + Math.sin(a1) * r1, cz),
    ].map(([x, y]) => x.toFixed(2) + "," + y.toFixed(2)).join(" ");
    return '<polygon points="' + pts + '" fill="#f2b455"/>';
  }

  function apply() {
    const i = Math.round(state.step);
    // Active station lifts its bit into the bore; the rest retract.
    bits.forEach((b, k) => {
      const d = Math.abs(k - state.step);
      const engage = Math.max(0, 1 - d);            // 1 at the active station
      b.setAttribute("transform", "translate(0," + (engage * 5).toFixed(2) + ")");
      b.style.opacity = (0.35 + engage * 0.65).toFixed(3);
    });
    carriages.forEach((c, k) => {
      const d = Math.abs(k - state.step);
      const lift = Math.max(0, 1 - d) * 4;
      c.setAttribute("transform", "translate(0," + (-lift).toFixed(2) + ")");
    });
    // Gates: shut = opaque, open = barely there.
    gates.forEach((g, k) => {
      const shut = state.gates[k];
      const face = g.querySelector("polygon");
      if (face) face.setAttribute("opacity", shut ? "0.94" : "0.16");
      g.style.filter = shut ? "none" : "saturate(0.4)";
    });
    if (rosette) rosette.innerHTML = litRosette(i);
    if (needle) {
      const cx = needle.dataset.cx, cy = needle.dataset.cy;
      const ang = -140 + state.precision * 280;
      needle.setAttribute("transform", "rotate(" + ang.toFixed(1) + " " + cx + " " + cy + ")");
    }
    const shut = state.gates.filter(Boolean).length;
    readout.textContent =
      "station " + (i + 1) + "/" + N + " \u00b7 " + PHASES[i].id.toLowerCase() +
      " \u00b7 " + (PHASES[i].label) +
      " \u00b7 " + shut + "/3 gates shut";
  }

  function frame() {
    const ease = reduce ? 1 : 0.12;
    state.step += (state.stepTarget - state.step) * ease;
    if (Math.abs(state.stepTarget - state.step) < 0.001) state.step = state.stepTarget;
    apply();
    raf = !reduce && state.step !== state.stepTarget ? requestAnimationFrame(frame) : null;
  }
  let raf = null;
  const kick = () => { if (!raf) raf = requestAnimationFrame(frame); };

  // pointer: map the pointer's x along the card onto a station index
  const pick = (ev) => {
    const r = svg.getBoundingClientRect();
    const u = (ev.clientX - r.left) / r.width;
    return Math.min(N - 1, Math.max(0, Math.round(u * (N - 1))));
  };
  let dragging = false;
  svg.addEventListener("pointerdown", (ev) => {
    if (ev.target.closest("[data-gate]")) {
      const k = Number(ev.target.closest("[data-gate]").dataset.gate);
      state.gates[k] = !state.gates[k];
      state.activeGate = k;
      state.precision = state.gates.filter(Boolean).length / 3;
      apply(); return;
    }
    dragging = true;
    state.stepTarget = pick(ev); kick();
  });
  svg.addEventListener("pointermove", (ev) => {
    if (!dragging) return;
    state.stepTarget = pick(ev); kick();
  });
  addEventListener("pointerup", () => { dragging = false; });
  addEventListener("keydown", (ev) => {
    if (ev.key === "ArrowRight") { state.stepTarget = Math.min(N - 1, state.stepTarget + 1); kick(); ev.preventDefault(); }
    if (ev.key === "ArrowLeft")  { state.stepTarget = Math.max(0, state.stepTarget - 1); kick(); ev.preventDefault(); }
    if (ev.key === " ") { state.gates[state.activeGate] = !state.gates[state.activeGate];
      state.activeGate = (state.activeGate + 1) % 3;
      state.precision = state.gates.filter(Boolean).length / 3; apply(); ev.preventDefault(); }
    if (ev.key === "r" || ev.key === "R") {
      state.stepTarget = 0; state.gates = [false, true, true];
      state.activeGate = 0; state.precision = 1/3; kick(); apply();
    }
  });

  // idle tour: walk the stations slowly when untouched
  let idle = 0;
  const tour = setInterval(() => {
    if (reduce) return;
    idle++;
    if (idle > 6) { state.stepTarget = (state.stepTarget + 1) % N; kick(); }
  }, 1400);
  ["pointerdown", "pointermove", "keydown"].forEach((e) =>
    addEventListener(e, () => { idle = 0; }, { passive: true }));

  // stop the tour when offscreen
  if ("IntersectionObserver" in window) {
    new IntersectionObserver((es) => {
      for (const e of es) if (!e.isIntersecting) idle = -1e9;
    }, { threshold: 0.05 }).observe(svg);
  }

  state.precision = 1/3;
  apply();
  if (!reduce) { setTimeout(() => { state.stepTarget = 1; kick(); }, 900); }
})();
</script>
</body>
</html>
`;

const out = join(HERE, "leindex-anatomy.html");
writeFileSync(out, HTML, "utf8");

// Honest console report, derived from what was actually emitted.
const drawn = parts.length;
writeFileSync(
  join(HERE, "build-report.json"),
  JSON.stringify({ out, drawn, vb: [VB_W, VB_H], scale: Number(scale.toFixed(4)), model: MODEL }, null, 2),
  "utf8",
);
process.stdout.write(`wrote ${out} (${drawn} drawables, svg ${Math.round(HTML.length / 1024)} KiB)\n`);
