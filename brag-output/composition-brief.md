# Hyperframes Composition Brief: LeIndex

## Objective
Create a short launch-style brag video for LeIndex.

## Output
- Composition directory: `brag-output/composition/`
- Rendered video: `brag-output/brag.mp4`
- Format: landscape — 1920x1080, 30 fps
- Duration: 20 s

## Source Material
- Project root: /mnt/WD-SSD/Prod/LeIndex-s1-wt
- Primary files read: `dashboard/index.html`, `dashboard/src/styles.css`, `README.md`, `dashboard/src/routes/{index,graph,search}.tsx`, `dashboard/src/routes/edit/$codebaseId/$path.tsx`, `dashboard/src/components/Layout/Header.tsx`, `dashboard/src/components/Codebase/CodebaseList.tsx`
- Product name: LeIndex
- Tagline / strongest claim: "Semantic code intelligence for humans and AI agents." (README)
- Key UI or visual moment to recreate: dashboard edit flow — Stage Changes → Apply All (`edit/$codebaseId/$path.tsx`); codebases list; search results panel
- Copy that must appear verbatim:
  - "Semantic code intelligence for humans and AI agents."
  - "~2,000 → ~380 tokens" — README, understand-a-file row (not "find one function")
  - "~12,000 → ~340 tokens (97%)" — README, cross-file rename row
  - "85.7 s → 5.0 s (17x)" — README v2.0.0 re-index row
  - "github.com/scooter-lacroix/LeIndex"

## Creative Direction
- Tone preset: deadpan
- Creative direction: restrained, numbers-first, a little proud of saving the context window. Matches the README's dry tone.
- Interpretation: slow-ish pacing, big type, few effects, minimal SFX (≤4). No jump-scare cuts.
- Angle: an AI agent burns context reading whole files to answer small questions; LeIndex answers in a fraction of the tokens.
- Hook: "Your agent just read 2,000 tokens to understand one file." (first 2.5 s)
- Outro / punchline: "Re-index tail: 85.7 s → 5.0 s." then the repo URL.
- Avoid:
  - Generic SaaS language ("supercharge", "seamless", "10x")
  - Abstract filler visuals (particles, equalizers, glowing orbs)
  - Unrelated visual redesign of the dashboard

## Visual Identity
- Background: `#061423` (gradient to `#12263a`)
- Text: `#ecf9ff`, muted `#9ac6d8`
- Accent: `#3de6d4`; accent-alt `#ff9f5a`; warm `#ffcf5a`
- Display font: Space Grotesk, fallback `Inter, ui-sans-serif, system-ui, sans-serif`
- Body font: IBM Plex Mono for code/terminal lines, fallback `ui-monospace, monospace`
- Visual references from the project: `dashboard/src/styles.css` tokens; the README query block

## Storyboard
Use the storyboard in `brag-output/brag-plan.md` as the creative contract.

Scene summary:
1. Hook — 0–2.5 s — "Your agent just read 2,000 tokens to understand one file."
2. Reveal — 2.5–6.0 s — Wordmark "LeIndex" + tagline
3. Ask — 6.0–10.0 s — terminal-style query/result block, labeled "example"
4. Stat — 10.0–14.0 s — "~12,000 tokens" struck → "~340 tokens", cross-file rename
5. App — 14.0–18.0 s — dashboard mockup: Stage Changes → Apply All
6. Punchline — 18.0–20.0 s — re-index stat, then repo URL

## Audio
- Audio role: sparse professional accents
- Audio arc: silent start → low bed enters at 2.5 s → one accent at the stat landing → bed fades out over the final 0.8 s
- Music: none — no bundled track was supplied and no cue source was verified this run. Music is intentionally silent for this build; document it in the final report.
- Music treatment: n/a
- Music cue guidance: unavailable
- Audio-reactive treatment: none (no music)
- Audio-coupled moments:
  - 14.0–14.5 s — stat/click — one soft key/click SFX on Stage Changes and Apply All
  - 10.0 s — stat landing — optional single accent SFX, skip if it clutters
- SFX selection guidance: ≤4 SFX total, short and dry; no whooshes
- SFX analysis guidance: none supplied; choose low-frequency-risk sounds only
- Exact SFX choice: Hyperframes/author chooses at implementation time. SFX files, if any, copied to `composition/assets/`.
- Audio files: none yet

## Hyperframes Instructions
Composition contract from the scaffolded `AGENTS.md` (Hyperframes CLI 0.8.154):
- Every timed element: `data-start`, duration, `class="clip"`.
- One paused root GSAP timeline registered on `window.__timelines["main"]`.
- No `Date.now()`, `Math.random()`, or network fetches.
- GSAP is vendored at `assets/gsap.min.js` (3.14.2), not loaded from the CDN.
- Run `npx hyperframes check` before render (single gate). WCAG contrast failures are errors.
- Keep creation and rendering local.

Domain skills `hyperframes-core`/`-animation`/`-creative`/`-cli` are not in this catalog; the CLI help and the scaffold's `AGENTS.md` are the authority used.

## Self-review checklist
- [ ] Every on-screen factual claim appears in the README or dashboard source
- [ ] Example query/result labeled "example"
- [ ] Total duration 15–25 s (target 20 s)
- [ ] `hyperframes check` passes
- [ ] Text readable at 1920x1080; contrast passes
