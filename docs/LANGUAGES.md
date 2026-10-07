# LeIndex Language Support

**Goal: 100+ languages.** Tier-0 is the universal floor — tree-sitter parsing
plus heuristic PDG for every language we can load a grammar for. This table
is the live inventory; the count is enforced by
`test_tier0_active_language_floor` so breadth can never silently regress.

## Active languages (37)

| Language | Extensions | Parser | Notes |
|---|---|---|---|
| Python | .py | bespoke | |
| JavaScript | .js .jsx .mjs .cjs | bespoke | |
| TypeScript | .ts .tsx .mts .cts | bespoke | |
| Rust | .rs | bespoke | Tier-1 SCIP pilot target |
| Go | .go | bespoke | |
| Java | .java | bespoke | Tier-1 via scip-java |
| C++ | .cpp .cc .cxx .hpp .hxx | bespoke | |
| C | .c .h | bespoke | |
| C# | .cs | bespoke | Tier-1 via scip-dotnet |
| Ruby | .rb | bespoke | Tier-1 via scip-ruby |
| PHP | .php | bespoke | Tier-1 via scip-php |
| Lua | .lua | bespoke | |
| Scala | .scala .sc | bespoke | |
| Bash | .sh .bash | bespoke | |
| JSON | .json | bespoke | |
| Swift | .swift | bespoke | unblocked 2026-08-20 |
| Kotlin | .kt .kts | bespoke | via `tree-sitter-kotlin-ng` fork |
| Dart | .dart | bespoke | unblocked 2026-08-20 |
| HTML | .html .htm | generic | |
| CSS | .css | generic | |
| SCSS | .scss | generic | legacy `language()` API |
| YAML | .yaml .yml | generic | names via fallback |
| CMake | .cmake | generic | |
| Elixir | .ex .exs | generic | |
| Erlang | .erl .hrl | generic | |
| Haskell | .hs | generic | |
| Perl | .pl .pm | generic | |
| R | .r | generic | |
| Zig | .zig | generic | |
| GraphQL | .graphql .gql | generic | |
| HCL / Terraform | .hcl .tf .tfvars | generic | |
| Make | Makefile .mak .mk | generic | |
| Emacs Lisp | .el | generic | |
| Julia | .jl | generic | |
| D | .d .di | generic | |
| GLSL | .glsl .vert .frag .comp | generic | legacy API, `LANGUAGE_GLSL` |
| Embedded Template | .ejs .erb .liquid | generic | parse-only |
| Markdown / RST / TXT | .md .rst .txt | doc parser | dedicated heading/section parser (docs tier) |

## Blocked, with rationale (2026-08-20 probe)

Verified against tree-sitter `0.26` in an isolated crate probe
(single-copy link, runtime `set_language` load test):

| Grammar | Blocker |
|---|---|
| tree-sitter-kotlin (original) | pins an incompatible tree-sitter major; `tree-sitter-kotlin-ng` 1.1 serves instead |
| tree-sitter-bibtex 0.1 | requires tree-sitter ^0.22.6 — duplicate C symbols |
| tree-sitter-latex 0.1 | external scanner symbols never built — link failure |
| tree-sitter-ocaml, tree-sitter-csv, tree-sitter-agda, tree-sitter-commonlisp | `cc` crate version conflict with the active set |
| tree-sitter-protobuf, tree-sitter-nu, tree-sitter-awk, tree-sitter-purescript | not published on crates.io |
| tree-sitter-tsq 0.19 | requires tree-sitter ^0.19 — duplicate C symbols |
| markdown via tree-sitter | dedicated doc parser instead (heading sections, doc tier) |

## Adding a language (the three-line pipeline)

1. `Cargo.toml`: add the optional grammar dep + its `dep:` entry in the
   `parse` feature (must resolve against the single tree-sitter copy —
   verify with the probe pattern above first).
2. `src/parse/traits.rs`: one `tier0_language!` line (name, display,
   extensions, loader).
3. Registry: `LanguageId` variant + arms in `src/parse/grammar.rs`, a
   `parser_for_language` arm in `src/parse/languages.rs` (bespoke parser or
   `GenericParser::new("<name>")` + a `GENERIC_LANGUAGE_TABLE` row with real
   node kinds — dump them with a scratch parse before trusting any list),
   extensions in `SOURCE_EXTENSIONS`, and a fixture row in
   `src/parse/generic_test.rs`.

Bump the floor in `test_tier0_active_language_floor` when the count grows.

## Tiers

- **Tier 0** (this table): parse + heuristic graph + lexical/neural search.
- **Tier 1** (SCIP precision): compiler-grade definitions/references merged
  onto the Tier-0 graph. Rust first (`rust-analyzer scip`); rollout per the
  precision-tier plan.
- **Docs tier**: markdown/rest/text indexed as first-class content with
  heading-section nodes.
