# /brag plan — LeIndex (run 2026-10-13-161406)

## Rubric answers

1. **App:** LeIndex — semantic code intelligence for humans and AI agents: PDG, hybrid search, structural tools behind one daemon. Dashboard = operational view over indexed codebases (`dashboard/`).
2. **Funniest / most impressive claim:** "Cross-file symbol rename: ~12,000 tokens → ~340 tokens (97%)" (README, methodology in `docs/TOOL_SUPREMACY_BENCHMARKS.md`). Also: re-index tail "85.7 s → 5.0 s (17x)" (README v2.0.0 table).
3. **Visual hook:** the README's terminal-style query block (`you: "where is request validation enforced?"`) on the `#061423` / `#3de6d4` / `#ff9f5a` palette from `dashboard/src/styles.css`.
4. **Actual UI to show:** dashboard flow — Codebases list (`routes/index.tsx` → `CodebaseList`), Graph (`routes/graph.tsx`), Search (`routes/search.tsx`, "Search Code"), Edit (`routes/edit/$codebaseId/$path.tsx`: Stage Changes → Apply All). Rebuild as HTML mockups from `styles.css` tokens (no live screenshot taken).
5. **Length:** 20 s, landscape. Fits 15–25.
6. **Tone:** preset `deadpan` — direction: "a stat slide that is weirdly proud of saving your context window." Restraint fits the product's own dry, numbers-first README.
7. **Audio:** music bed low, tasteful SFX only on the two stat reveals and the Stage/Apply click. `--no-music`/`--no-sfx` not passed → audio on. `--voice` not passed → no narration.
8. **Share caption:** "Your AI agent just read 2,000 tokens to find one function. LeIndex returns the answer in 380. Open source, local-first."
9. **User flow:** entry (codebases list) → key action (search or impact/graph) → result (edit staged, applied).

## Claim hygiene (only these numbers appear on screen)

- "~12,000 → ~340 tokens, 97%" — README table, rename row.
- "~2,000 → ~380 tokens, 81%" — README table, file-understanding row.
- "85.7 s → 5.0 s (17x)" — README v2.0.0 table.
- "37 active language grammars" — README.
- Example query/result `src/http/request_validator.rs validate_session (0.92)` is **illustrative per README**; label it "example" on screen. Do not present as a real result from this repo.
- Do NOT use dashboard/README.md's "v1.5.2" (stale) or any version number.
- Repo/CTA: `github.com/scooter-lacroix/LeIndex` (public, from repo metadata).

## Storyboard (20 s)

| # | Time | Beat | On screen | Motion / audio |
|---|---|---|---|---|
| 1 | 0.0–2.5 | **Hook** | Black `#061423`. Big line: "Your agent just read 2,000 tokens to find one function." | Fast-in type, hold ~1.2 s. Key ticks on type (subtle). |
| 2 | 2.5–6.0 | **Reveal** | Wordmark "LeIndex" + tagline "Semantic code intelligence for humans and AI agents." | Crossfade. Music bed enters at 2.5 s. |
| 3 | 6.0–10.0 | **Highlight 1: ask** | Terminal-style block: `you: "where is request validation enforced?"` → `src/http/request_validator.rs  validate_session (0.92)`, `callers: 3 · callees: 7 · blast radius: 12 files`. Label "example". | Lines appear one per 0.6 s. Hard cut in. |
| 4 | 10.0–14.0 | **Highlight 2: stat** | Two-column stat: `~12,000 tokens` struck → `~340 tokens` / "cross-file rename, 97% less". | Number count-down; one stat SFX on the landing number. |
| 5 | 14.0–18.0 | **Highlight 3: the app** | Dashboard mockup flow: Codebases card → Graph → Search result → Edit panel with "Stage Changes" → "Apply All" clicking through. | Simulate the clicks: cursor clicks Stage, then Apply; button SFX on each. Slow crossfade between panels. |
| 6 | 18.0–20.0 | **Punchline / outro** | "Re-index tail: 85.7 s → 5.0 s." then "github.com/scooter-lacroix/LeIndex" | Final stat lands on the beat; music fades out last 0.8 s. |

Duration sum: 2.5 + 3.5 + 4 + 4 + 4 + 2 = 20 s. ✓

## Readability

- Stat lines hold ≥ 1.5 s after settle.
- Text-line hold: short label ~0.8 s settled; sentence ~0.3 s/word (beat 1 sentence = 9 words ≈ 2.7 s hold — allow beat 1 to run its full 2.5 s with a fast-in).
- Beat 3 lines appear sequentially at 0.6 s intervals; each stays until the final line lands.

## Music cue guidance

- Track: default bundled bed (no custom track supplied). Cue preset: none verified in this run — cues to be detected at composition time.
- Targets: strong cue at 6.0 s (reveal), 10.0 s (stat landing), 18.0 s (punchline stat).
- Restraint note: tone is `deadpan` — keep the music bed low (−18 dB ballpark), SFX count ≤ 4 total.

## Palette / type (from `dashboard/src/styles.css`)

- Background `#061423` (gradient to `#12263a`), panel `rgba(12,37,57,0.78)`, border `rgba(82,208,255,0.28)`.
- Text `#ecf9ff`, muted `#9ac6d8`, accent `#3de6d4`, accent-alt `#ff9f5a`, warm `#ffcf5a`.
- Display: Space Grotesk (Google Fonts). Mono: IBM Plex Mono.

## Format

- Landscape 1920×1080, 30 fps, 20 s. Vertical/square not requested.

## Risks / open items

- Hyperframes CLI not installed locally (`npx --no-install hyperframes` failed); Step 3 needs network install.
- Dashboard not served/screenshotted in this run; UI beats are mockups built from CSS tokens and component structure, not captures.
- Semantic search through `leindex_explore` returned "Project not indexed" repeatedly even after `leindex_manage` index completed (gen 187 complete). Used read_file / find / project_map instead. Worth reporting as a bug.
