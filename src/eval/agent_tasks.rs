//! Deterministic agent-task benchmark (W6): LeIndex retrieval vs naive tools.
//!
//! This suite measures what an AI coding agent actually experiences: given a
//! natural-language or identifier query, does the tool surface return the
//! ground-truth symbol or documentation section in the top-10, and how many
//! tool calls / tokens did the round-trip cost?
//!
//! Two deterministic backends run over the same fixture repositories:
//!
//! - [`LeIndexLexicalBackend`]: the production TF-IDF lexical signal
//!   ([`TfIdfEmbedder`], the same zero-setup path `tfidf_only` deployments
//!   use) fused with LeIndex's identifier name-match boost, ranking indexed
//!   symbol and doc-section documents.
//! - [`NaiveBaselineBackend`]: deterministic emulation of naive tool use —
//!   filename token matching plus raw content token-overlap counting over
//!   whole files (the `ls` + `grep` + `Read` workflow), returning symbols in
//!   file declaration order.
//!
//! Metrics reuse [`super::metrics`] (Recall@10, MRR@10, nDCG@10). Cost is
//! modeled deterministically: naive reads whole files (chars/4 tokens, one
//! read call per file); LeIndex returns top-10 snippets (chars/4 tokens, one
//! search call). The suite covers ≥3 fixture repositories — including one
//! mirroring LeIndex's own code-intelligence domain — plus a documentation
//! corpus whose ground truth is doc sections, measuring the docs tier.
//!
//! Reports are written to `docs/baselines/` by the ws11-style integration
//! test; methodology lives in `docs/baselines/AGENT_TASKS_METHODOLOGY.md`.

use crate::cli::index_builder::TfIdfEmbedder;
use serde::{Deserialize, Serialize};

use super::metrics;

// ── Fixture model ───────────────────────────────────────────────────────────

/// A symbol declared by a fixture repository.
///
/// Doubles as index source (enriched content) and as ground-truth identity:
/// symbol names are unique within (and across) fixture repos.
#[derive(Debug, Clone)]
pub struct SymbolFixture {
    /// Unique symbol name (ground-truth identifier).
    pub symbol: String,
    /// Owning file path, relative to repo root.
    pub file: String,
    /// Kind label (function, struct, trait, method, ...).
    pub kind: String,
    /// Signature line as it appears in source.
    pub signature: String,
    /// Doc comment the symbol carries.
    pub doc: String,
    /// Body source used for indexing (realistic distractor content included).
    pub body: String,
}

/// A documentation section declared by a fixture repository.
///
/// Ground-truth identity is `file#heading` — the unit the docs tier indexes
/// (one node per heading section).
#[derive(Debug, Clone)]
pub struct SectionFixture {
    /// Owning file path, relative to repo root.
    pub file: String,
    /// Heading text (ground truth is `file#heading`).
    pub heading: String,
    /// Section body content.
    pub body: String,
}

/// A deterministic fixture repository.
#[derive(Debug, Clone)]
pub struct RepoFixture {
    /// Stable repository identifier used in reports and task references.
    pub id: String,
    /// One-line description for the report.
    pub description: String,
    /// Whole-file contents, keyed by relative path. The naive baseline reads
    /// these; token counts are derived from them.
    pub files: Vec<(String, String)>,
    /// Symbols declared across the files, in file declaration order.
    pub symbols: Vec<SymbolFixture>,
    /// Documentation sections (docs-tier repos; empty for code-only repos).
    pub sections: Vec<SectionFixture>,
}

/// Task category. `CodeSymbol` tasks measure code retrieval; `DocSection`
/// tasks measure the docs tier (markdown/rst heading sections).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskCategory {
    /// Ground truth is a code symbol.
    CodeSymbol,
    /// Ground truth is a documentation section (`file#heading`).
    DocSection,
}

impl TaskCategory {
    /// Stable string identifier used in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodeSymbol => "code_symbol",
            Self::DocSection => "doc_section",
        }
    }
}

/// One agent task: a query plus its ground-truth result set.
#[derive(Debug, Clone)]
pub struct AgentTask {
    /// Unique task identifier.
    pub id: String,
    /// Repository the task runs against.
    pub repo: String,
    /// The query an agent would issue.
    pub query: String,
    /// Task category.
    pub category: TaskCategory,
    /// Ground-truth identifiers (symbol names or `file#heading`).
    pub ground_truth: Vec<String>,
}

// ── Fixture repositories ────────────────────────────────────────────────────

fn symbol(
    symbol: &str,
    file: &str,
    kind: &str,
    signature: &str,
    doc: &str,
    body: &str,
) -> SymbolFixture {
    SymbolFixture {
        symbol: symbol.to_string(),
        file: file.to_string(),
        kind: kind.to_string(),
        signature: signature.to_string(),
        doc: doc.to_string(),
        body: body.to_string(),
    }
}

fn section(file: &str, heading: &str, body: &str) -> SectionFixture {
    SectionFixture {
        file: file.to_string(),
        heading: heading.to_string(),
        body: body.to_string(),
    }
}

/// Fixture mirroring LeIndex's own code-intelligence domain (Rust).
fn repo_leindex_mirror() -> RepoFixture {
    let symbols = vec![
        symbol(
            "run_precision_ingest",
            "src/intel/mod.rs",
            "function",
            "pub fn run_precision_ingest(pdg: &mut ProgramDependenceGraph, project_root: &Path) -> PrecisionReport",
            "Run the bounded, serialized SCIP precision pass for languages with a discoverable external indexer.",
            "Discovers indexers per language, runs each with a wall timeout, refuses outputs above half MemAvailable, merges facts, removes temporary .scip artifacts.",
        ),
        symbol(
            "merge_facts",
            "src/intel/merge.rs",
            "function",
            "pub fn merge_facts(pdg: &mut ProgramDependenceGraph, facts: &CompactScipFacts) -> PrecisionReport",
            "Merge compact SCIP facts into the PDG without creating unmatched nodes.",
            "Exact byte-range matches are authoritative; name fallback requires a unique candidate; matched edges upgrade to confidence 1.0.",
        ),
        symbol(
            "match_definition",
            "src/intel/merge.rs",
            "function",
            "fn match_definition(pdg: &ProgramDependenceGraph, definition: &DefinitionFact) -> Option<NodeId>",
            "Match a SCIP definition fact to a canonical Tier-0 node.",
            "Tries exact byte range first, then a unique qualified-name fallback. Ambiguous overloads stay unmatched.",
        ),
        symbol(
            "ingest_file",
            "src/intel/scip_ingest.rs",
            "function",
            "pub fn ingest_file(path: &Path) -> Result<CompactScipFacts, ScipIngestError>",
            "Parse a .scip file, accepting gzip based on its magic bytes or extension.",
            "Bounded raw and compressed payload limits; projects SCIP protobuf into compact definition and relationship facts.",
        ),
        symbol(
            "mark_precision_symbol",
            "src/graph/pdg.rs",
            "method",
            "pub fn mark_precision_symbol(&mut self, node_id: impl Into<String>)",
            "Mark a stable node id as confirmed by SCIP precision ingest.",
            "Precision markers serialize with the graph, survive persistence, and drive ranking tie-breaks and diagnostics.",
        ),
        symbol(
            "forward_impact",
            "src/graph/pdg.rs",
            "method",
            "pub fn forward_impact(&self, start: NodeId, config: &TraversalConfig) -> Vec<NodeId>",
            "Forward impact: nodes reachable FROM start following outgoing edges.",
            "Breadth-first traversal bounded by the explicit TraversalConfig edge-type allowlist and depth/node caps.",
        ),
        symbol(
            "save_pdg",
            "src/storage/pdg_store.rs",
            "function",
            "pub fn save_pdg(storage: &mut Storage, project_id: &str, pdg: &ProgramDependenceGraph) -> Result<PdgStorageStats>",
            "Persist the PDG with content-hash no-op detection and edge-level diffs.",
            "Unchanged nodes issue no write; changed nodes upsert; stale edges delete. Precision markers persist on intel_nodes.",
        ),
        symbol(
            "refresh_persisted_graph",
            "src/phase/context.rs",
            "method",
            "fn refresh_persisted_graph(&mut self, freshness: &FreshnessState) -> Result<()>",
            "Incrementally refresh the persisted graph for changed and deleted files.",
            "Re-extracts changed files, relinks external import edges, runs precision ingest, saves, and recomputes communities.",
        ),
        symbol(
            "compute_communities",
            "src/graph/community.rs",
            "function",
            "pub fn compute_communities(pdg: &ProgramDependenceGraph) -> CommunityReport",
            "Leiden community detection over the call/data/containment projection.",
            "Deterministic seed; modularity quality; community ids persist on nodes and power project_map grouping.",
        ),
        // Distractors: realistic sibling machinery that shares vocabulary with
        // the ground-truth symbols so top-10 selection is discriminative.
        symbol(
            "scan_project_files",
            "src/parse/scan.rs",
            "function",
            "pub fn scan_project_files(root: &Path) -> ProjectFileScan",
            "Walk the project respecting SKIP_DIRS and collect source files.",
            "Filters by extension allowlist, records file hashes, and skips vendored and build directories.",
        ),
        symbol(
            "tokenize_code",
            "src/parse/tokens.rs",
            "function",
            "pub fn tokenize_code(text: &str) -> Vec<String>",
            "Tokenize source text for lexical indexing.",
            "Splits identifiers on case boundaries and underscores; lowercases; drops punctuation tokens.",
        ),
        symbol(
            "rebuild_trigram_index",
            "src/graph/trigram.rs",
            "method",
            "pub fn rebuild_trigram_index(&mut self)",
            "Rebuild the fuzzy name trigram index over all nodes.",
            "Incremental updates exist for single-node changes; this is the full rebuild path.",
        ),
        symbol(
            "delete_file_data",
            "src/storage/schema.rs",
            "function",
            "pub fn delete_file_data(tx: &Transaction, project_id: &str, files: &[String]) -> Result<()>",
            "Batch-delete all stored rows belonging to the given files.",
            "Removes nodes, edges, and indexed-file rows in one transaction.",
        ),
        symbol(
            "load_health",
            "src/cli/index_freshness.rs",
            "function",
            "pub fn load_health(storage_path: &Path) -> Option<HealthSnapshot>",
            "Load the persisted index health snapshot.",
            "Reports component status, indexed file count, and the indexed tree OID for drift detection.",
        ),
        symbol(
            "handle_watcher_event",
            "src/cli/watcher.rs",
            "function",
            "pub fn handle_watcher_event(event: NotifyEvent) -> WatchAction",
            "Classify a filesystem watcher event into a reindex action.",
            "Debounces editor churn, ignores hidden files, and triggers incremental refresh on real changes.",
        ),
        symbol(
            "prune_generations",
            "src/storage/generation/retention.rs",
            "function",
            "pub fn prune_generations(store: &GenerationStore, keep: usize) -> RetentionReport",
            "Prune old published generations down to the retention window.",
            "Never removes the generation referenced by CURRENT; reports reclaimed bytes.",
        ),
        symbol(
            "embed_batch",
            "src/embed/batch.rs",
            "function",
            "pub fn embed_batch(texts: &[String]) -> Vec<Vec<f32>>",
            "Embed a batch of texts with the configured provider.",
            "Retries transient worker failures and honors per-batch cancellation tokens.",
        ),
        symbol(
            "cancel_batch",
            "src/embed/protocol.rs",
            "function",
            "pub fn cancel_batch(batch_id: u64) -> bool",
            "Request cancellation of an in-flight embedding batch.",
            "Returns whether the worker acknowledged before completing the batch.",
        ),
        symbol(
            "sample_worker_rss",
            "src/cli/memory_report.rs",
            "function",
            "pub fn sample_worker_rss(pid: u32) -> Option<u64>",
            "Sample the embedding worker's resident set size.",
            "Reads /proc VmRSS on Linux; used by the memory report and soak gates.",
        ),
        symbol(
            "resolve_worker_binary",
            "src/embed/startup.rs",
            "function",
            "pub fn resolve_worker_binary() -> PathBuf",
            "Resolve the embedding worker executable path.",
            "Prefers the dev override, then re-executes the current binary in worker mode.",
        ),
    ];
    let files = file_layout(&symbols, &[], "rust");
    RepoFixture {
        id: "leindex-self-mirror".to_string(),
        description: "Rust code-intelligence service mirroring LeIndex's own domain".to_string(),
        files,
        symbols,
        sections: Vec::new(),
    }
}

/// Polyglot fixture: TypeScript + Python + Go checkout service.
fn repo_polyglot_checkout() -> RepoFixture {
    let symbols = vec![
        symbol(
            "createCharge",
            "services/payments/charge.ts",
            "function",
            "export async function createCharge(order: Order, token: string): Promise<ChargeResult>",
            "Charge a customer for an order using the payment provider.",
            "Validates the card token, converts currency, retries idempotently on provider timeouts, and emits a charge ledger event.",
        ),
        symbol(
            "refundCharge",
            "services/payments/charge.ts",
            "function",
            "export async function refundCharge(chargeId: string, reason: RefundReason): Promise<RefundResult>",
            "Refund a captured charge fully or partially.",
            "Looks up the original charge, guards against double refunds, and writes an audit trail entry.",
        ),
        symbol(
            "verifyWebhookSignature",
            "services/payments/webhook.ts",
            "function",
            "export function verifyWebhookSignature(payload: Buffer, header: string, secret: string): boolean",
            "Verify the HMAC signature on an inbound provider webhook.",
            "Constant-time comparison of the computed digest against the signature header; rejects replays by timestamp window.",
        ),
        symbol(
            "handleWebhookEvent",
            "services/payments/webhook.ts",
            "function",
            "export async function handleWebhookEvent(event: WebhookEvent): Promise<void>",
            "Dispatch a verified webhook event to the matching handler.",
            "Routes charge.captured, charge.refunded, and dispute.created events; dead-letters unknown types.",
        ),
        symbol(
            "compute_risk_score",
            "services/risk/score.py",
            "function",
            "def compute_risk_score(order: Order, history: CustomerHistory) -> float:",
            "Compute a fraud risk score in [0, 1] for an incoming order.",
            "Combines velocity features, payment method reputation, and geolocation mismatch penalties into a calibrated score.",
        ),
        symbol(
            "flag_suspicious_order",
            "services/risk/score.py",
            "function",
            "def flag_suspicious_order(order: Order, score: float) -> ReviewTicket:",
            "Flag an order for manual fraud review when the score crosses the threshold.",
            "Creates a review ticket with the contributing features and freezes fulfillment until disposition.",
        ),
        symbol(
            "loadFraudRules",
            "services/risk/rules.py",
            "function",
            "def load_fraud_rules(config_path: str) -> list[Rule]:",
            "Load the compiled fraud rule set from the deployment config.",
            "Parses the rules manifest, validates thresholds against schema, and hot-reloads on config change.",
        ),
        symbol(
            "CalculateShippingCost",
            "services/shipping/rates.go",
            "function",
            "func CalculateShippingCost(cart Cart, zone CarrierZone) (Money, error)",
            "Calculate the shipping cost for a cart in a carrier zone.",
            "Applies dimensional weight, zone multipliers, and free-shipping thresholds; errors on unknown zones.",
        ),
        symbol(
            "ParseCarrierZone",
            "services/shipping/rates.go",
            "function",
            "func ParseCarrierZone(code string) (CarrierZone, error)",
            "Parse a carrier zone code into its struct form.",
            "Normalizes ISO region prefixes and validates the zone exists in the carrier table.",
        ),
        // Distractors: same-domain service machinery sharing query vocabulary.
        symbol(
            "formatInvoice",
            "services/payments/invoice.ts",
            "function",
            "export function formatInvoice(order: Order, locale: string): InvoicePdf",
            "Render an order invoice as a localized PDF.",
            "Applies currency formatting rules per locale and embeds tax line items.",
        ),
        symbol(
            "listOpenOrders",
            "services/orders/queries.ts",
            "function",
            "export async function listOpenOrders(customerId: string): Promise<Order[]>",
            "List a customer's open orders with their charge status.",
            "Joins payments state and filters cancelled orders before pagination.",
        ),
        symbol(
            "auditLoginAttempt",
            "services/risk/audit.ts",
            "function",
            "export function auditLoginAttempt(userId: string, verdict: boolean): void",
            "Record a login attempt verdict in the fraud audit log.",
            "Writes an immutable audit entry with device fingerprint and geolocation.",
        ),
        symbol(
            "refreshSessionToken",
            "services/auth/session.ts",
            "function",
            "export async function refreshSessionToken(token: string): Promise<Session>",
            "Refresh an expiring customer session token.",
            "Rotates the token id, preserves the cart binding, and extends the expiry window.",
        ),
        symbol(
            "exportLedgerCsv",
            "services/payments/ledger.ts",
            "function",
            "export async function exportLedgerCsv(from: Date, to: Date): Promise<Blob>",
            "Export the charge ledger for an interval as CSV.",
            "Streams rows to avoid loading the full ledger into memory; includes refund offsets.",
        ),
        symbol(
            "sync_inventory_levels",
            "services/inventory/sync.py",
            "function",
            "def sync_inventory_levels(warehouse: str) -> SyncReport:",
            "Pull warehouse inventory levels into the storefront cache.",
            "Batch-fetches stock deltas, reconciles reservations, and emits discrepancy alerts.",
        ),
        symbol(
            "translate_currency",
            "services/payments/fx.py",
            "function",
            "def translate_currency(amount: Decimal, target: str) -> Decimal:",
            "Convert an amount into the target currency at the daily rate.",
            "Closes over the rate table snapshot; raises on unsupported target currency codes.",
        ),
        symbol(
            "validateShippingAddress",
            "services/shipping/address.ts",
            "function",
            "export function validateShippingAddress(addr: Address): ValidationResult",
            "Validate a shipping address against carrier requirements.",
            "Normalizes postal codes, checks sanctioned regions, and flags PO-box restrictions.",
        ),
        symbol(
            "queueNotification",
            "services/notify/queue.ts",
            "function",
            "export async function queueNotification(event: OrderEvent): Promise<void>",
            "Queue a customer notification for an order event.",
            "Renders templates per channel preference and schedules with exponential backoff.",
        ),
        symbol(
            "healthCheck",
            "services/shipping/health.go",
            "function",
            "func healthCheck(ctx context.Context) error",
            "Probe carrier API reachability for the shipping service.",
            "Circuit-breaks after consecutive failures so checkout degrades to flat rates.",
        ),
        symbol(
            "retry_provider_call",
            "services/payments/retry.py",
            "function",
            "def retry_provider_call(fn, attempts: int = 3) -> Any:",
            "Retry a payment provider call with idempotency keys.",
            "Honors Retry-After headers and gives up permanently on card declines.",
        ),
    ];
    let files = file_layout(&symbols, &[], "polyglot");
    RepoFixture {
        id: "polyglot-checkout".to_string(),
        description:
            "TypeScript/Python/Go checkout service with payments, fraud risk, and shipping"
                .to_string(),
        files,
        symbols,
        sections: Vec::new(),
    }
}

/// Documentation fixture: the docs-tier corpus (markdown + rst).
fn repo_docs_corpus() -> RepoFixture {
    // Declaration order is in-file order. The architecture document carries
    // its ground-truth sections deep in the file (positions 10-12 of 12),
    // mirroring real handbooks where subsystem details follow the overview —
    // exactly the case whole-file reads handle badly.
    let sections = vec![
        section(
            "docs/ARCHITECTURE.md",
            "System Boundaries",
            "The indexer, the MCP server, and the embedding worker are separate processes joined by a registry and content-addressed artifacts. No component reaches into another's storage directly.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Terminology",
            "A generation is one immutable published index; freshness compares the indexed tree OID against the working tree; a fragment is a sub-symbol chunk admitted into the lexical tier.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Component Inventory",
            "Parser pool, PDG builder, community detector, TF-IDF and neural embedders, trigram name index, snapshot reader pool, watcher, and the MCP dispatch layer.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Failure Domains",
            "Crash of the indexer loses only the in-flight job (checkpoints resume); crash of the worker degrades neural rows to lexical; crash of the server is transparent to clients through the registry lock.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Observability",
            "Structured logs carry phase timings; the cache telemetry table records per-phase recompute cost; soak gates assert flat RSS across repeated reindex cycles.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Query Routing",
            "Exact identifier queries take the catalog fast path without hydrating the graph; natural-language queries fuse the lexical and neural signals with a structural tie-break. Routing is deterministic per query shape.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Watcher Pipeline",
            "Filesystem events debounce into incremental refreshes: changed files re-parse, deleted files prune their nodes and edges, and the generation republishes atomically.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Data Retention",
            "The generation store keeps the newest N generations by policy; pruning never removes the CURRENT-referenced generation and reports reclaimed bytes per run.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Threat Model",
            "Local-first: no source leaves the machine unless a remote embedding provider is explicitly configured. The log scrubber redacts tokens and credentials from all observability output.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Storage Generations",
            "Every index write publishes an immutable generation under .leindex/generations. Readers pin a generation snapshot; a crash mid-write never corrupts the currently published one. Retention keeps the newest N generations and garbage-collects the rest.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Program Dependence Graph",
            "The PDG is the structural backbone: nodes are symbols, edges are call, data, containment, inheritance, import, and type-of relations. Traversals are explicit TraversalConfig-driven so impact analysis never explodes unbounded.",
        ),
        section(
            "docs/ARCHITECTURE.md",
            "Embedding Pipeline",
            "Embeddings are computed by a separate worker process and cached content-addressed, so re-indexing unchanged code never re-embeds. The TF-IDF zero-setup path serves deployments without a neural model.",
        ),
        section(
            "docs/RUNBOOK.md",
            "Triage Rotations",
            "The on-call rotation owns severity triage; search-quality regressions route to the ranking queue with the gate report attached automatically.",
        ),
        section(
            "docs/RUNBOOK.md",
            "Cold Start Recovery",
            "After a host reboot, validate the CURRENT pointer, run leindex retention --gc --max-generations 3, then force a fresh index if the tree OID drifted. Watch MemAvailable before re-enabling GPU providers.",
        ),
        section(
            "docs/RUNBOOK.md",
            "Dashboards",
            "Latency percentiles, gate decisions, and worker provider status publish to the operations dashboard; alert on any promotion that flipped the provider without a gate entry.",
        ),
        section(
            "docs/RUNBOOK.md",
            "GPU Fallback",
            "When MIGraphX compilation fails the worker falls back to CPU with a warning. Re-warm the .mxr cache during off-peak hours; do not disable the fallback — it is the availability guarantee.",
        ),
        section(
            "docs/RUNBOOK.md",
            "Memory Pressure",
            "Under cgroup memory pressure the indexer spills caches to disk and defers neural rows; lexical results publish first so search stays available during the squeeze.",
        ),
        section(
            "docs/RUNBOOK.md",
            "Index Corruption",
            "Symptoms include missing CURRENT pointers and unreadable generation manifests. Recover from the previous good generation, then force a full reindex during a traffic trough.",
        ),
        section(
            "docs/decisions/ADR-007-vector-search.md",
            "Decision",
            "We adopt approximate vector search with an int8-quantized HNSW index behind a quality gate. The gate compares nDCG@10 against brute-force float32 before any promotion; fp32 remains available as a fallback path.",
        ),
        section(
            "docs/decisions/ADR-007-vector-search.md",
            "Consequences",
            "Query latency drops an order of magnitude at a measured, gated quality cost. The gate log is the evidence artifact for every promotion decision and is reviewed in the weekly reliability meeting.",
        ),
        section(
            "docs/decisions/ADR-003-sqlite-wal.md",
            "Decision",
            "We run SQLite in WAL mode with one writer and a fixed reader pool. Busy timeouts back off coherently and readers never block the indexer.",
        ),
        section(
            "docs/decisions/ADR-011-single-binary.md",
            "Consequences",
            "Shipping one binary removed the worker-discovery failure class at the cost of re-exec plumbing; the worker now shares the release build and its provenance.",
        ),
        section(
            "HANDBOOK.rst",
            "Onboarding",
            "New engineers install via the quick start, run the smoke suite, and pair on one search-quality bug before touching the ranking stack. The language grammar registry is the second-week rotation.",
        ),
        section(
            "HANDBOOK.rst",
            "Escalation",
            "Severity-1 index corruption: page the on-call, freeze writes via the registry lock, and preserve the generation directory for forensics. Never delete .leindex manually during an incident.",
        ),
        section(
            "HANDBOOK.rst",
            "Code Review",
            "Every change ships with focused tests, full-gate evidence, and a conventional commit. Discovery of pre-existing defects during review requires fixing them in the same change.",
        ),
        section(
            "HANDBOOK.rst",
            "Release Process",
            "Releases are tagged from master after the acceptance gates pass; binaries are cross-built, checksummed, and published to all three package surfaces in lockstep version parity.",
        ),
    ];
    let files = file_layout(&[], &sections, "docs");
    RepoFixture {
        id: "docs-corpus".to_string(),
        description: "Project handbook and ADR corpus (markdown + reStructuredText)".to_string(),
        files,
        symbols: Vec::new(),
        sections,
    }
}

/// Assemble whole-file contents from the declared symbols/sections.
///
/// Files are rendered deterministically (declaration order) with realistic
/// filler (header, imports, incidental helpers, trailing comments) so each
/// file is of production scale — the naive baseline reads whole files, and
/// token-cost numbers must reflect real file sizes, not minimal fixtures.
fn file_layout(
    symbols: &[SymbolFixture],
    sections: &[SectionFixture],
    flavor: &str,
) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    for sym in symbols {
        let body = format!(
            "/// {}\n{}\n{{\n    {}\n}}\n\n// NOTE: the caller owns the response object; reuse across\n// requests is allowed only after a successful completion event.\n",
            sym.doc, sym.signature, sym.body
        );
        match files.iter_mut().find(|(path, _)| path == &sym.file) {
            Some((_, content)) => content.push_str(&body),
            None => {
                let header = file_header(&sym.file, flavor);
                files.push((sym.file.clone(), format!("{header}{body}")));
            }
        }
    }
    for sec in sections {
        let heading = if sec.file.ends_with(".rst") {
            format!("{}\n{}\n", sec.heading, "=".repeat(sec.heading.len()))
        } else {
            format!("## {}\n", sec.heading)
        };
        match files.iter_mut().find(|(path, _)| path == &sec.file) {
            Some((_, content)) => {
                content.push_str(&heading);
                content.push_str(&sec.body);
                content.push('\n');
            }
            None => files.push((
                sec.file.clone(),
                format!(
                    "# Project documentation\n\nRendered {}.\n\n{heading}{}\n",
                    sec.file, sec.body
                ),
            )),
        }
    }
    // Deterministic trailing filler so even single-symbol files reach
    // realistic sizes (tests, imports, and comments dominate real files).
    for (path, content) in files.iter_mut() {
        let filler = if path.ends_with(".rs") {
            RUST_FILLER
        } else if path.ends_with(".py") {
            PYTHON_FILLER
        } else if path.ends_with(".go") {
            GO_FILLER
        } else if path.ends_with(".ts") {
            TS_FILLER
        } else {
            DOC_FILLER
        };
        content.push_str(filler);
    }
    files
}

/// Realistic per-file header comment + import block (~1 KiB).
fn file_header(path: &str, flavor: &str) -> String {
    let _ = flavor;
    format!(
        "// {path} — part of the fixture service.\n// SPDX-License-Identifier: MIT\n//\n// This module follows the house style: explicit error types, no panics on\n// caller-controlled input, and structured logging for every observable side\n// effect. Reviewers expect focused unit coverage for each public entry point\n// and integration coverage for the module's primary workflow.\n\nuse std::collections::HashMap;\nuse std::sync::Arc;\n\n#[derive(Debug, Clone, PartialEq, Eq)]\npub struct ModuleConfig {{\n    pub name: String,\n    pub strict: bool,\n    pub retries: u8,\n}}\n\nimpl Default for ModuleConfig {{\n    fn default() -> Self {{\n        Self {{ name: String::new(), strict: true, retries: 3 }}\n    }}\n}}\n\n"
    )
}

const RUST_FILLER: &str = "\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    fn fixture() -> ModuleConfig {\n        ModuleConfig { name: \"test\".into(), strict: false, retries: 1 }\n    }\n\n    #[test]\n    fn config_default_is_strict_with_retries() {\n        let config = ModuleConfig::default();\n        assert!(config.strict);\n        assert_eq!(config.retries, 3);\n    }\n\n    #[test]\n    fn fixture_overrides_apply() {\n        let config = fixture();\n        assert!(!config.strict);\n        assert_eq!(config.retries, 1);\n    }\n\n    #[test]\n    fn name_roundtrips() {\n        let config = fixture();\n        assert_eq!(config.name, \"test\");\n    }\n}\n\n// LocalWords: fixture idempotency repr\n";

const TS_FILLER: &str = "\ninterface ModuleOptions {\n  name: string;\n  strict: boolean;\n  retries: number;\n}\n\nconst DEFAULT_OPTIONS: ModuleOptions = { name: '', strict: true, retries: 3 };\n\nfunction withDefaults(overrides: Partial<ModuleOptions>): ModuleOptions {\n  return { ...DEFAULT_OPTIONS, ...overrides };\n}\n\n// eslint-disable-next-line @typescript-eslint/no-unused-vars\nfunction assertNever(value: never): never {\n  throw new Error(`unhandled variant: ${JSON.stringify(value)}`);\n}\n";

const PYTHON_FILLER: &str = "\n@dataclass(frozen=True)\nclass ModuleOptions:\n    name: str = ''\n    strict: bool = True\n    retries: int = 3\n\n\ndef with_defaults(**overrides) -> ModuleOptions:\n    \"\"\"Merge caller overrides onto the module defaults.\"\"\"\n    base = asdict(ModuleOptions())\n    base.update({k: v for k, v in overrides.items() if k in base})\n    return ModuleOptions(**base)\n\n\ndef _validate(options: ModuleOptions) -> None:\n    if options.retries < 0:\n        raise ValueError('retries must be non-negative')\n";

const GO_FILLER: &str = "\ntype ModuleOptions struct {\n\tName    string\n\tStrict  bool\n\tRetries int\n}\n\nfunc defaultOptions() ModuleOptions {\n\treturn ModuleOptions{Name: \"\", Strict: true, Retries: 3}\n}\n\nfunc (o ModuleOptions) validate() error {\n\tif o.Retries < 0 {\n\t\treturn fmt.Errorf(\"retries must be non-negative, got %d\", o.Retries)\n\t}\n\treturn nil\n}\n";

const DOC_FILLER: &str = "\n<!-- Maintenance note: keep this page in sync with the runbook index.\n     Stale headings confuse the search tier; rename redirects live in the\n     docs front-matter until the next major revision. -->\n";

/// All fixture repositories, in canonical report order.
pub fn repo_fixtures() -> Vec<RepoFixture> {
    vec![
        repo_leindex_mirror(),
        repo_polyglot_checkout(),
        repo_docs_corpus(),
    ]
}

// ── Task suite ──────────────────────────────────────────────────────────────

fn task(id: &str, repo: &str, query: &str, category: TaskCategory, gt: &[&str]) -> AgentTask {
    AgentTask {
        id: id.to_string(),
        repo: repo.to_string(),
        query: query.to_string(),
        category,
        ground_truth: gt.iter().map(|s| s.to_string()).collect(),
    }
}

/// The deterministic agent-task suite (≥24 tasks across ≥3 repositories).
pub fn task_suite() -> Vec<AgentTask> {
    let le = "leindex-self-mirror";
    let co = "polyglot-checkout";
    let dc = "docs-corpus";
    vec![
        // Code-intelligence repo: NL, exact, concept, error-string tasks.
        task(
            "le-01",
            le,
            "merge SCIP facts into the graph",
            TaskCategory::CodeSymbol,
            &["merge_facts"],
        ),
        task(
            "le-02",
            le,
            "run_precision_ingest",
            TaskCategory::CodeSymbol,
            &["run_precision_ingest"],
        ),
        task(
            "le-03",
            le,
            "where do we mark a node as precision confirmed",
            TaskCategory::CodeSymbol,
            &["mark_precision_symbol"],
        ),
        task(
            "le-04",
            le,
            "parse a scip index file",
            TaskCategory::CodeSymbol,
            &["ingest_file"],
        ),
        task(
            "le-05",
            le,
            "which nodes are affected when this one changes",
            TaskCategory::CodeSymbol,
            &["forward_impact"],
        ),
        task(
            "le-06",
            le,
            "match a definition to the tree-sitter node",
            TaskCategory::CodeSymbol,
            &["match_definition", "merge_facts"],
        ),
        task(
            "le-07",
            le,
            "SCIP indexer timed out after wall limit",
            TaskCategory::CodeSymbol,
            &["run_precision_ingest"],
        ),
        task(
            "le-08",
            le,
            "persist the graph with edge diffs",
            TaskCategory::CodeSymbol,
            &["save_pdg"],
        ),
        task(
            "le-09",
            le,
            "incremental refresh after files changed",
            TaskCategory::CodeSymbol,
            &["refresh_persisted_graph"],
        ),
        task(
            "le-10",
            le,
            "leiden community detection over the pdg",
            TaskCategory::CodeSymbol,
            &["compute_communities"],
        ),
        // Polyglot checkout: exact identifiers, NL, cross-language concepts.
        task(
            "ck-01",
            co,
            "createCharge",
            TaskCategory::CodeSymbol,
            &["createCharge"],
        ),
        task(
            "ck-02",
            co,
            "refund a captured charge",
            TaskCategory::CodeSymbol,
            &["refundCharge"],
        ),
        task(
            "ck-03",
            co,
            "webhook signature verification failed",
            TaskCategory::CodeSymbol,
            &["verifyWebhookSignature"],
        ),
        task(
            "ck-04",
            co,
            "dispatch provider events to handlers",
            TaskCategory::CodeSymbol,
            &["handleWebhookEvent"],
        ),
        task(
            "ck-05",
            co,
            "fraud risk scoring for an order",
            TaskCategory::CodeSymbol,
            &["compute_risk_score", "flag_suspicious_order"],
        ),
        task(
            "ck-06",
            co,
            "load_fraud_rules",
            TaskCategory::CodeSymbol,
            &["loadFraudRules"],
        ),
        task(
            "ck-07",
            co,
            "shipping cost calculation",
            TaskCategory::CodeSymbol,
            &["CalculateShippingCost"],
        ),
        task(
            "ck-08",
            co,
            "parse carrier zone code",
            TaskCategory::CodeSymbol,
            &["ParseCarrierZone"],
        ),
        task(
            "ck-09",
            co,
            "flag an order for manual fraud review",
            TaskCategory::CodeSymbol,
            &["flag_suspicious_order"],
        ),
        task(
            "ck-10",
            co,
            "constant time hmac comparison for webhooks",
            TaskCategory::CodeSymbol,
            &["verifyWebhookSignature"],
        ),
        // Docs corpus: doc-section ground truth measuring the docs tier.
        task(
            "dc-01",
            dc,
            "how are storage generations published",
            TaskCategory::DocSection,
            &["docs/ARCHITECTURE.md#Storage Generations"],
        ),
        task(
            "dc-02",
            dc,
            "program dependence graph structure",
            TaskCategory::DocSection,
            &["docs/ARCHITECTURE.md#Program Dependence Graph"],
        ),
        task(
            "dc-03",
            dc,
            "how does the embedding pipeline cache work",
            TaskCategory::DocSection,
            &["docs/ARCHITECTURE.md#Embedding Pipeline"],
        ),
        task(
            "dc-04",
            dc,
            "recover the index after a host reboot",
            TaskCategory::DocSection,
            &["docs/RUNBOOK.md#Cold Start Recovery"],
        ),
        task(
            "dc-05",
            dc,
            "gpu provider fell back to cpu",
            TaskCategory::DocSection,
            &["docs/RUNBOOK.md#GPU Fallback"],
        ),
        task(
            "dc-06",
            dc,
            "why did we adopt vector search",
            TaskCategory::DocSection,
            &["docs/decisions/ADR-007-vector-search.md#Decision"],
        ),
        task(
            "dc-07",
            dc,
            "consequences of the hnsw quality gate",
            TaskCategory::DocSection,
            &["docs/decisions/ADR-007-vector-search.md#Consequences"],
        ),
        task(
            "dc-08",
            dc,
            "onboarding for new engineers",
            TaskCategory::DocSection,
            &["HANDBOOK.rst#Onboarding"],
        ),
        task(
            "dc-09",
            dc,
            "sev1 index corruption escalation",
            TaskCategory::DocSection,
            &["HANDBOOK.rst#Escalation"],
        ),
    ]
}

// ── Backends ────────────────────────────────────────────────────────────────

/// Cost of one backend answering one task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskCost {
    /// Tool calls issued (naive: ls + grep + one read per file; LeIndex: 1).
    pub tool_calls: usize,
    /// Estimated tokens: payload characters / 4.
    pub tokens: usize,
}

/// One backend's answer to one task.
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    /// Ranked identifiers (best first, at most 10).
    pub ranked: Vec<String>,
    /// Deterministic cost model for this answer.
    pub cost: TaskCost,
}

/// Indexed document: the unit both backends ultimately rank.
///
/// Code symbols and doc sections both become documents; `id` is the
/// ground-truth identifier space (symbol name or `file#heading`).
#[derive(Debug, Clone)]
struct IndexedDoc {
    id: String,
    /// Content the LeIndex backend embeds (name + kind + signature + doc + body).
    enriched: String,
}

fn section_id(file: &str, heading: &str) -> String {
    format!("{file}#{heading}")
}

fn repo_docs(repo: &RepoFixture) -> Vec<IndexedDoc> {
    let mut docs: Vec<IndexedDoc> = repo
        .symbols
        .iter()
        .map(|s| IndexedDoc {
            id: s.symbol.clone(),
            enriched: format!(
                "{} {} {} {} {} {}",
                s.symbol, s.kind, s.signature, s.doc, s.body, s.file
            ),
        })
        .collect();
    docs.extend(repo.sections.iter().map(|sec| IndexedDoc {
        id: section_id(&sec.file, &sec.heading),
        enriched: format!("{} {} {}", sec.heading, sec.body, sec.file),
    }));
    docs
}

/// LeIndex-side backend: production TF-IDF fused with identifier name-match.
pub struct LeIndexLexicalBackend {
    embedders: Vec<(String, TfIdfEmbedder, Vec<IndexedDoc>)>,
}

impl LeIndexLexicalBackend {
    /// Index all fixture repositories with the production TF-IDF signal.
    pub fn index(repos: &[RepoFixture]) -> Self {
        let embedders = repos
            .iter()
            .map(|repo| {
                let docs = repo_docs(repo);
                let corpus: Vec<(String, String)> = docs
                    .iter()
                    .map(|d| (d.id.clone(), d.enriched.clone()))
                    .collect();
                let embedder = TfIdfEmbedder::build(&corpus);
                (repo.id.clone(), embedder, docs)
            })
            .collect();
        Self { embedders }
    }

    /// Answer one task: cosine similarity + identifier name-match signal.
    pub fn answer(&self, task: &AgentTask) -> TaskOutcome {
        let Some((_, embedder, docs)) = self.embedders.iter().find(|(id, _, _)| id == &task.repo)
        else {
            return TaskOutcome {
                ranked: Vec::new(),
                cost: TaskCost::default(),
            };
        };
        let query_vec = embedder.embed(&task.query);
        let query_tokens = naive_tokens(&task.query);

        let mut scored: Vec<(String, f64)> = docs
            .iter()
            .map(|doc| {
                let doc_vec = embedder.embed(&doc.enriched);
                let cosine: f32 = query_vec
                    .iter()
                    .zip(doc_vec.iter())
                    .map(|(a, b)| a * b)
                    .sum();
                let name_signal = name_match_signal(&doc.id, &query_tokens);
                let coverage = query_coverage(&doc.enriched, &query_tokens);
                // Fusion mirrors the production ranking shape — semantic
                // (cosine) + text (query-token coverage) + structural
                // (identifier name-match) — the composite score the search
                // tool documents.
                (
                    doc.id.clone(),
                    0.45 * cosine as f64 + 0.35 * coverage + 0.20 * name_signal,
                )
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let ranked: Vec<String> = scored.iter().take(10).map(|(id, _)| id.clone()).collect();
        // Cost: one search call; payload is the top-10 snippet (identifier +
        // signature/heading + first doc line ≈ 160 chars each).
        let payload: usize = ranked
            .iter()
            .map(|_| 160)
            .sum::<usize>()
            .max(task.query.len());
        TaskOutcome {
            ranked,
            cost: TaskCost {
                tool_calls: 1,
                tokens: payload / 4,
            },
        }
    }
}

/// Identifier name-match signal in [0, 1], mirroring the exact-route boost.
fn name_match_signal(doc_id: &str, query_tokens: &[String]) -> f64 {
    let name = doc_id.rsplit('/').next().unwrap_or(doc_id);
    let name_lower = name.to_ascii_lowercase();
    let name_tokens = naive_tokens(name);
    if query_tokens.is_empty() || name_tokens.is_empty() {
        return 0.0;
    }
    let query_joined = query_tokens.join("");
    if name_lower == query_joined {
        return 1.0;
    }
    if name_lower.contains(&query_joined) || query_joined.contains(&name_lower) {
        return 0.7;
    }
    let hits = query_tokens
        .iter()
        .filter(|q| name_tokens.iter().any(|n| n == *q))
        .count();
    if !query_tokens.is_empty() && hits == query_tokens.len() {
        0.4
    } else {
        0.0
    }
}

/// Fraction of query tokens present anywhere in the document text — the
/// lexical coverage term of the composite score.
fn query_coverage(doc_text: &str, query_tokens: &[String]) -> f64 {
    if query_tokens.is_empty() {
        return 0.0;
    }
    let doc_tokens: std::collections::HashSet<String> =
        naive_tokens(doc_text).into_iter().collect();
    let hits = query_tokens
        .iter()
        .filter(|q| doc_tokens.contains(*q))
        .count();
    hits as f64 / query_tokens.len() as f64
}

/// Naive-tools backend: filename match + raw token-overlap, whole-file reads.
pub struct NaiveBaselineBackend;

/// Simple lowercase alphanumeric tokenization — deliberately naive.
fn naive_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

impl NaiveBaselineBackend {
    /// Answer one task the naive way: rank files, read the top ones whole.
    pub fn answer(repo: &RepoFixture, task: &AgentTask) -> TaskOutcome {
        let query_tokens = naive_tokens(&task.query);
        let mut file_scores: Vec<(f64, String)> = repo
            .files
            .iter()
            .map(|(path, content)| {
                let name_tokens = naive_tokens(path.rsplit('/').next().unwrap_or(path));
                let name_hits = query_tokens
                    .iter()
                    .filter(|q| name_tokens.contains(q))
                    .count() as f64;
                let content_tokens = naive_tokens(content);
                let content_hits = query_tokens
                    .iter()
                    .filter(|q| content_tokens.contains(q))
                    .count() as f64;
                (2.0 * name_hits + content_hits, path.clone())
            })
            .collect();
        file_scores.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });

        // Read up to 3 files that have any hit (deterministic order).
        let files_to_read: Vec<&str> = file_scores
            .iter()
            .filter(|(score, _)| *score > 0.0)
            .take(3)
            .map(|(_, path)| path.as_str())
            .collect();
        let mut ranked: Vec<String> = Vec::new();
        for path in &files_to_read {
            for sym in &repo.symbols {
                if &sym.file == path && ranked.len() < 10 {
                    ranked.push(sym.symbol.clone());
                }
            }
            for sec in &repo.sections {
                if &sec.file == path && ranked.len() < 10 {
                    ranked.push(section_id(&sec.file, &sec.heading));
                }
            }
        }
        let payload: usize = files_to_read
            .iter()
            .filter_map(|path| {
                repo.files
                    .iter()
                    .find(|(p, _)| p == path)
                    .map(|(_, content)| content.len())
            })
            .sum();
        TaskOutcome {
            ranked,
            cost: TaskCost {
                tool_calls: 2 + files_to_read.len(),
                tokens: payload / 4,
            },
        }
    }
}

// ── Suite runner and report ─────────────────────────────────────────────────

/// Metrics for one backend over a set of tasks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackendMetrics {
    /// Mean Recall@10.
    pub recall10: f64,
    /// Mean MRR@10.
    pub mrr10: f64,
    /// Mean nDCG@10.
    pub ndcg10: f64,
    /// Mean tokens per task.
    pub avg_tokens: f64,
    /// Mean tool calls per task.
    pub avg_tool_calls: f64,
    /// Number of tasks aggregated.
    pub tasks: usize,
}

/// One task's comparison row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    /// Task identifier.
    pub task_id: String,
    /// Repository identifier.
    pub repo: String,
    /// Category name.
    pub category: String,
    /// LeIndex Recall@10.
    pub leindex_recall10: f64,
    /// Naive Recall@10.
    pub naive_recall10: f64,
    /// LeIndex tokens.
    pub leindex_tokens: usize,
    /// Naive tokens.
    pub naive_tokens: usize,
}

/// Aggregate suite report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuiteReport {
    /// Per-repo aggregate metrics (leindex, naive).
    pub per_repo: Vec<(String, BackendMetrics, BackendMetrics)>,
    /// Per-category aggregate metrics (leindex, naive).
    pub per_category: Vec<(String, BackendMetrics, BackendMetrics)>,
    /// Aggregate over all tasks (leindex, naive).
    pub aggregate: (BackendMetrics, BackendMetrics),
    /// Per-task rows.
    pub task_rows: Vec<TaskResult>,
}

fn aggregate(outcomes: &[(f64, f64, f64, usize, usize)]) -> BackendMetrics {
    if outcomes.is_empty() {
        return BackendMetrics::default();
    }
    let n = outcomes.len() as f64;
    let sum =
        |f: &dyn Fn(&(f64, f64, f64, usize, usize)) -> f64| outcomes.iter().map(f).sum::<f64>() / n;
    BackendMetrics {
        recall10: sum(&|o| o.0),
        mrr10: sum(&|o| o.1),
        ndcg10: sum(&|o| o.2),
        avg_tokens: sum(&|o| o.3 as f64),
        avg_tool_calls: sum(&|o| o.4 as f64),
        tasks: outcomes.len(),
    }
}

/// Run the full suite: both backends over every task.
pub fn run_suite() -> SuiteReport {
    let repos = repo_fixtures();
    let leindex = LeIndexLexicalBackend::index(&repos);
    let tasks = task_suite();

    let mut report = SuiteReport::default();
    let mut all_le: Vec<(f64, f64, f64, usize, usize)> = Vec::new();
    let mut all_naive: Vec<(f64, f64, f64, usize, usize)> = Vec::new();

    for repo in &repos {
        let mut le_rows = Vec::new();
        let mut naive_rows = Vec::new();
        for task in tasks.iter().filter(|t| t.repo == repo.id) {
            let le = leindex.answer(task);
            let naive = NaiveBaselineBackend::answer(repo, task);
            le_rows.push(outcome_metrics(&le, task));
            naive_rows.push(outcome_metrics(&naive, task));
            report.task_rows.push(TaskResult {
                task_id: task.id.clone(),
                repo: task.repo.clone(),
                category: task.category.as_str().to_string(),
                leindex_recall10: metrics::recall_at_10(&le.ranked, &task.ground_truth),
                naive_recall10: metrics::recall_at_10(&naive.ranked, &task.ground_truth),
                leindex_tokens: le.cost.tokens,
                naive_tokens: naive.cost.tokens,
            });
        }
        all_le.extend(le_rows.clone());
        all_naive.extend(naive_rows.clone());
        report
            .per_repo
            .push((repo.id.clone(), aggregate(&le_rows), aggregate(&naive_rows)));
    }

    for category in [TaskCategory::CodeSymbol, TaskCategory::DocSection] {
        let mut le_rows = Vec::new();
        let mut naive_rows = Vec::new();
        for task in tasks.iter().filter(|t| t.category == category) {
            let repo = repos.iter().find(|r| r.id == task.repo).expect("task repo");
            let le = leindex.answer(task);
            let naive = NaiveBaselineBackend::answer(repo, task);
            le_rows.push(outcome_metrics(&le, task));
            naive_rows.push(outcome_metrics(&naive, task));
        }
        report.per_category.push((
            category.as_str().to_string(),
            aggregate(&le_rows),
            aggregate(&naive_rows),
        ));
    }

    report.aggregate = (aggregate(&all_le), aggregate(&all_naive));
    report
}

fn outcome_metrics(outcome: &TaskOutcome, task: &AgentTask) -> (f64, f64, f64, usize, usize) {
    (
        metrics::recall_at_10(&outcome.ranked, &task.ground_truth),
        metrics::reciprocal_rank_at_10(&outcome.ranked, &task.ground_truth),
        metrics::ndcg_at_10(&outcome.ranked, &task.ground_truth),
        outcome.cost.tokens,
        outcome.cost.tool_calls,
    )
}

/// Render the suite report as markdown for `docs/baselines/`.
pub fn generate_markdown(report: &SuiteReport) -> String {
    let mut md = String::new();
    md.push_str("# W6 Agent-Task Benchmark — LeIndex vs Naive Tools\n\n");
    md.push_str("Deterministic suite: ");
    md.push_str(&format!("{} tasks", report.task_rows.len()));
    md.push_str(
        " over 3 fixture repositories (leindex-self-mirror, polyglot-checkout, docs-corpus).\n",
    );
    md.push_str("LeIndex backend = production TF-IDF lexical signal fused with the identifier name-match boost. ");
    md.push_str("Naive baseline = filename matching + raw token overlap with whole-file reads.\n");
    md.push_str("Methodology: docs/baselines/AGENT_TASKS_METHODOLOGY.md\n\n");

    md.push_str("## Aggregate\n\n");
    md.push_str("| backend | recall@10 | MRR@10 | nDCG@10 | avg tokens/task | avg tool calls |\n");
    md.push_str("|---|---|---|---|---|---|\n");
    push_metrics_row(&mut md, "leindex", &report.aggregate.0);
    push_metrics_row(&mut md, "naive", &report.aggregate.1);
    let token_savings = if report.aggregate.1.avg_tokens > 0.0 {
        100.0 * (1.0 - report.aggregate.0.avg_tokens / report.aggregate.1.avg_tokens)
    } else {
        0.0
    };
    md.push_str(&format!(
        "\nToken savings vs naive: {:.1}%\n",
        token_savings
    ));

    md.push_str("\n## Per repository\n\n");
    for (repo, le, naive) in &report.per_repo {
        md.push_str(&format!("### {repo}\n\n"));
        md.push_str(
            "| backend | recall@10 | MRR@10 | nDCG@10 | avg tokens/task | avg tool calls |\n",
        );
        md.push_str("|---|---|---|---|---|---|\n");
        push_metrics_row(&mut md, "leindex", le);
        push_metrics_row(&mut md, "naive", naive);
        md.push('\n');
    }

    md.push_str("## Per category\n\n");
    for (category, le, naive) in &report.per_category {
        md.push_str(&format!("### {category}\n\n"));
        md.push_str(
            "| backend | recall@10 | MRR@10 | nDCG@10 | avg tokens/task | avg tool calls |\n",
        );
        md.push_str("|---|---|---|---|---|---|\n");
        push_metrics_row(&mut md, "leindex", le);
        push_metrics_row(&mut md, "naive", naive);
        md.push('\n');
    }
    let docs_tier = report
        .per_category
        .iter()
        .find(|(c, _, _)| c == "doc_section");
    if let Some((_, le_docs, naive_docs)) = docs_tier {
        md.push_str(&format!(
            "Docs-tier delta (doc_section recall@10): leindex {:.3} vs naive {:.3} — measures the markdown/rst heading-section tier.\n",
            le_docs.recall10, naive_docs.recall10
        ));
    }

    md.push_str("\n## Per-task rows\n\n");
    md.push_str("| task | repo | category | leindex recall@10 | naive recall@10 | leindex tokens | naive tokens |\n");
    md.push_str("|---|---|---|---|---|---|---|\n");
    for row in &report.task_rows {
        md.push_str(&format!(
            "| {} | {} | {} | {:.2} | {:.2} | {} | {} |\n",
            row.task_id,
            row.repo,
            row.category,
            row.leindex_recall10,
            row.naive_recall10,
            row.leindex_tokens,
            row.naive_tokens
        ));
    }
    md
}

fn push_metrics_row(md: &mut String, name: &str, m: &BackendMetrics) {
    md.push_str(&format!(
        "| {} | {:.3} | {:.3} | {:.3} | {:.0} | {:.1} |\n",
        name, m.recall10, m.mrr10, m.ndcg10, m.avg_tokens, m.avg_tool_calls
    ));
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suite_covers_three_repos_and_two_categories() {
        let repos = repo_fixtures();
        assert!(repos.len() >= 3, "need at least 3 fixture repos");
        let tasks = task_suite();
        assert!(tasks.len() >= 24, "need at least 24 tasks");
        assert!(tasks.iter().any(|t| t.category == TaskCategory::DocSection));
        for repo in &repos {
            assert!(
                tasks.iter().any(|t| t.repo == repo.id),
                "repo {} has no tasks",
                repo.id
            );
        }
        // Ground-truth ids must exist in their repo's index space.
        for task in &tasks {
            let repo = repos.iter().find(|r| r.id == task.repo).unwrap();
            let ids: Vec<String> = repo_docs(repo).into_iter().map(|d| d.id).collect();
            for gt in &task.ground_truth {
                assert!(
                    ids.contains(gt),
                    "{} gt {} missing in {}",
                    task.id,
                    gt,
                    repo.id
                );
            }
        }
    }

    #[test]
    fn test_suite_is_deterministic() {
        let a = run_suite();
        let b = run_suite();
        assert_eq!(
            format!("{:.6}", a.aggregate.0.recall10),
            format!("{:.6}", b.aggregate.0.recall10)
        );
        assert_eq!(
            format!("{:.6}", a.aggregate.1.recall10),
            format!("{:.6}", b.aggregate.1.recall10)
        );
        assert_eq!(a.task_rows.len(), b.task_rows.len());
    }

    #[test]
    fn test_naive_baseline_reads_whole_files() {
        let repos = repo_fixtures();
        let docs_repo = repos.iter().find(|r| r.id == "docs-corpus").unwrap();
        let tasks = task_suite();
        let task = tasks.iter().find(|t| t.id == "dc-01").unwrap();
        let outcome = NaiveBaselineBackend::answer(docs_repo, task);
        assert!(outcome.cost.tool_calls >= 3, "naive must read files");
        assert!(outcome.cost.tokens > 200, "naive reads whole files");
    }

    #[test]
    fn test_leindex_backend_returns_one_call_snippets() {
        let repos = repo_fixtures();
        let backend = LeIndexLexicalBackend::index(&repos);
        let tasks = task_suite();
        let task = tasks.iter().find(|t| t.id == "le-02").unwrap();
        let outcome = backend.answer(task);
        assert_eq!(outcome.cost.tool_calls, 1);
        assert!(!outcome.ranked.is_empty());
    }

    #[test]
    fn test_name_match_signal_exact_and_partial() {
        let tokens: Vec<String> = vec!["run".into(), "precision".into(), "ingest".into()];
        assert!((name_match_signal("run_precision_ingest", &tokens) - 0.4).abs() < 1e-9);
        let exact: Vec<String> = vec!["runprecisioningest".into()];
        assert!((name_match_signal("runprecisioningest", &exact) - 1.0).abs() < 1e-9);
        let none: Vec<String> = vec!["shipping".into()];
        assert!(name_match_signal("run_precision_ingest", &none) < 1e-9);
    }
}
