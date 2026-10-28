// Verify every doc surface that enumerates LeIndex tools against the LIVE CLI
// schemas. Guards against the drift this branch just repaired: a branch or
// argument renamed in code but left stale in README/skill/compat docs.
//
// Usage: node scripts/check-docs-against-schemas.mjs
// Requires: `leindex` on PATH (writes schemas to a temp dir itself).
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const TOOLS = [
  "leindex_explore",
  "leindex_analyze",
  "leindex_edit",
  "leindex_manage",
  "leindex_git",
];

const DOCS = [
  "README.md",
  "MCP_COMPATIBILITY.md",
  "docs/MCP.md",
  "docs/AGENT_GUIDANCE.md",
  "integrations/skills/leindex-code-intelligence/SKILL.md",
  "integrations/skills/leindex-code-intelligence/references/tool-selection.md",
  "pi/skills/leindex/SKILL.md",
];

const dir = mkdtempSync(join(tmpdir(), "leindex-schemas-"));

/** Load live schemas once; every check reads the same snapshot. */
function loadSchemas() {
  const schemas = new Map();
  for (const tool of TOOLS) {
    const out = execFileSync("leindex", ["tools", "schema", tool], {
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
    });
    const path = join(dir, `${tool}.json`);
    schemas.set(tool, JSON.parse(out));
  }
  return schemas;
}

const schemas = loadSchemas();
const failures = [];

// Every advertised branch title, per tool.
const branchesByTool = new Map(
  [...schemas].map(([tool, schema]) => [
    tool,
    new Set((schema.oneOf || []).map((b) => b.title)),
  ]),
);

// 1. No doc may name a branch that no longer exists. The retired names are
//    specific and load-bearing: `git_status`/`git_diff` on analyze, and
//    `phase` on manage, were all real branches that moved.
const RETIRED = [
  { pattern: /leindex_analyze`?\s*`?mode=git_(status|diff)/, why: "analyze git_* modes moved to leindex_git" },
  { pattern: /leindex_manage`?\s*`?action=phase/, why: "manage action=phase moved to leindex_analyze" },
];

for (const doc of DOCS) {
  const text = readFileSync(doc, "utf8");
  for (const { pattern, why } of RETIRED) {
    const hit = pattern.exec(text);
    if (hit) failures.push(`${doc}: names a retired branch — ${why} (matched ${hit[0]})`);
  }

  // 2. A "four tools/routers" claim is impossible now: there are five.
  for (const m of text.matchAll(/\bfour (tools|routers)\b/gi)) {
    failures.push(`${doc}: stale router count "${m[0]}" (there are five)`);
  }
}

// 3. The schema reference must mention every live branch of every tool.
const schemaDoc = "integrations/skills/leindex-code-intelligence/references/tool-schemas.md";
const schemaText = readFileSync(schemaDoc, "utf8");
for (const [tool, titles] of branchesByTool) {
  for (const title of titles) {
    if (!schemaText.includes(`"const": "${title}"`)) {
      failures.push(`${schemaDoc}: missing branch ${tool}.${title}`);
    }
  }
}

// 4. The skill must name every router, so a model reading it can discover them.
const skillDoc = "integrations/skills/leindex-code-intelligence/SKILL.md";
const skillText = readFileSync(skillDoc, "utf8");
for (const tool of TOOLS) {
  if (!skillText.includes(tool)) failures.push(`${skillDoc}: does not mention ${tool}`);
}

// 5. The live guide must agree with the live manifest.
try {
  const guide = execFileSync("leindex", ["tools", "inspect", "leindex_git"], {
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  });
  if (!guide.includes("status (default)")) {
    failures.push("leindex_git guide: does not advertise status as the default action");
  }
} catch (error) {
  failures.push(`could not inspect leindex_git: ${error.message}`);
}

rmSync(dir, { recursive: true, force: true });

for (const failure of failures) process.stdout.write(`FAIL ${failure}\n`);
process.stdout.write(
  failures.length === 0
    ? `OK   ${DOCS.length} doc surfaces agree with ${TOOLS.length} live schemas\n`
    : `\n${failures.length} drift finding(s)\n`,
);
process.exit(failures.length === 0 ? 0 : 1);
