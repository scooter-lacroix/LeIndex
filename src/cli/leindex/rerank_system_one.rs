// System-One LLM-judge reranker for search results.
//
// Reuses the MCP gate's post-ranking kernel (`system_one::post`) unchanged:
// `judge_results` chunks candidates into provider requests (cached,
// wave-limited), `judge_raw` turns verdicts into drop/keep decisions, and
// `plan` blends the judge signal (0.7 base / 0.3 signal) and selects the
// final order. Judged results that the judge dropped are demoted to the tail
// by the caller's keep-missing remap — the same contract as the ONNX
// cross-encoder path.
//
// The judge is independent of the MCP gate's mode: `[system_one] mode = off`
// disables the gate, not this backend. Only the feature flag and the
// provider configuration matter. Every failure (no asker, transport,
// incomplete verdicts) fails open to the original retrieval order.

use crate::cli::mcp::system_one::post::{PostParams, judge_results, plan, verdicts_complete};
use crate::cli::mcp::system_one::types::Asker;
use crate::config::LeIndexConfig;
use crate::search::search::SearchResult;
use serde_json::{Value, json};

/// Judge signal weight in the blended score (base retrieval keeps 0.7).
const SIGNAL_WEIGHT: f32 = 0.30;
/// Snippet characters per candidate passed to the judge.
const SNIPPET_CHARS: usize = 800;
/// Never return fewer results than this when that many existed.
const MIN_KEEP: usize = 3;

/// Build candidate JSON in the shape `candidate_state` reads: `file_path`,
/// `symbol_name`, `symbol_type`, `signature`, and `context` (the code span
/// already extracted by the dispatch's file-cache path).
fn candidate_values(results: &[SearchResult], docs: &[(String, String, f32)]) -> Vec<Value> {
    results
        .iter()
        .zip(docs)
        .map(|(result, (_, content, _))| {
            json!({
                "file_path": result.file_path,
                "symbol_name": result.symbol_name,
                "symbol_type": result.symbol_type,
                "signature": result.signature,
                "context": content,
            })
        })
        .collect()
}

/// Core judge flow, generic over the asker so tests can inject doubles.
/// Returns the judged (kept) candidates in blended-score order; candidates
/// the judge dropped are absent — the dispatch's keep-missing append demotes
/// them to the tail of the result list. `None` fails open (the caller keeps
/// the original order).
async fn judge_with<A: Asker>(
    asker: &A,
    query: &str,
    results: &[SearchResult],
    docs: &[(String, String, f32)],
    project_root: &str,
    timeout_ms: u64,
    params: &PostParams,
) -> Option<Vec<(String, f32)>> {
    let candidates = candidate_values(results, docs);
    let (verdicts, _tokens) =
        judge_results(asker, query, project_root, &candidates, params, timeout_ms).await;

    // Fail open: partial verdicts mean the judge did not see every candidate,
    // so a reorder would be built on missing evidence.
    if !verdicts_complete(&verdicts) {
        tracing::debug!("system-one rerank: incomplete verdicts; keeping original order");
        return None;
    }
    let base: Vec<f32> = docs.iter().map(|(_, _, score)| *score).collect();
    let judged = plan(&base, &verdicts, params);
    Some(
        judged
            .order
            .into_iter()
            .filter_map(|i| docs.get(i).map(|(id, _, score)| (id.clone(), *score)))
            .collect(),
    )
}

/// Re-rank the top-N search candidates through the System-One provider.
///
/// `results` and `docs` are positionally aligned (docs were built from
/// `results[..n]` in order). Returns the judged order as `(node_id, score)`
/// tuples, or `None` to keep the original order (no provider configured,
/// transport failure, incomplete verdicts — all fail open).
pub(crate) fn rerank(
    query: &str,
    results: &[SearchResult],
    docs: &[(String, String, f32)],
    project_root: &str,
) -> Option<Vec<(String, f32)>> {
    let config = LeIndexConfig::load_cached();
    let asker =
        crate::cli::mcp::system_one::asker_from_config(&config.system_one, &config.services)?;
    let timeout_ms = config.system_one.timeout_ms.max(1);
    let params = PostParams {
        weight: SIGNAL_WEIGHT,
        snippet_chars: SNIPPET_CHARS,
        min_keep: MIN_KEEP,
        relevant_min: config.system_one.relevant_min,
        instructions_max: config.system_one.instructions_max,
        top_n: config.search.rerank_top_n as usize,
        ..PostParams::default()
    };
    let fut = judge_with(
        &asker,
        query,
        results,
        docs,
        project_root,
        timeout_ms,
        &params,
    );
    // Sync bridge: drive the judged future on a thread that owns a scratch
    // current-thread runtime. This is the shape used by
    // `index_builder::hybrid::block_on_remote`, and it is the only one that is
    // correct in every context `rerank` is called from:
    //   * outside any runtime (sync callers) — trivially fine;
    //   * inside the CLI's multi-threaded runtime — `block_on` directly panics
    //     ("Cannot start a runtime from within a runtime") and `block_in_place`
    //     would work, but only on the multi-threaded flavor;
    //   * inside a CURRENT-THREAD runtime, e.g. any `#[tokio::test]` caller —
    //     `Handle::try_current()` still returns `Ok`, so the `block_in_place`
    //     branch was taken, and it panics because a current-thread runtime has
    //     no blocking pool to hop to (Codex Major).
    // A scoped thread sidesteps the flavor question entirely; the judge future
    // borrows its inputs, so `thread::scope` (not `spawn`) avoids cloning them.
    // Any failure to build the runtime fails open like every other judge error.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    std::thread::scope(|scope| {
        scope
            .spawn(move || runtime.block_on(fut))
            .join()
            .ok()
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::system_one::testing::FnAsker;
    use crate::cli::mcp::system_one::types::{Answer, Request, Response, SystemOneError};

    /// A minimal search hit; only the identity + score fields matter here.
    fn result(id: &str, file: &str, symbol: &str, overall: f32) -> SearchResult {
        SearchResult {
            rank: 1,
            node_id: id.to_string(),
            file_path: file.to_string(),
            symbol_name: symbol.to_string(),
            symbol_type: Some("function".to_string()),
            signature: Some(format!("fn {symbol}()")),
            complexity: 1,
            caller_count: None,
            dependency_count: None,
            language: "rust".to_string(),
            score: crate::search::ranking::Score {
                overall,
                ..Default::default()
            },
            context: None,
            byte_range: (0, 0),
            fragment_byte_range: None,
            line_number: None,
        }
    }

    fn docs_of(results: &[SearchResult]) -> Vec<(String, String, f32)> {
        results
            .iter()
            .map(|r| {
                (
                    r.node_id.clone(),
                    format!("code for {}", r.symbol_name),
                    r.score.overall,
                )
            })
            .collect()
    }

    /// Slot-keyed yes/no answers; unanswered names are simply omitted.
    fn slot_answers(pairs: &[(usize, &str, f32)]) -> Response {
        let mut response = Response::default();
        for (slot, name, p_yes) in pairs {
            response
                .answers
                .insert(format!("c{slot}_{name}"), Answer::YesNo { p_yes: *p_yes });
        }
        response
    }

    /// Judge everything relevant with the given per-slot relevance; evidence
    /// defaults to the same value so no candidate sits on a threshold edge.
    fn relevance_asker(relevances: &[f32]) -> impl Asker {
        FnAsker(move |_request: &Request| {
            let answers = (0..relevances.len())
                .map(|slot| (slot, "is_relevant", relevances[slot]))
                .chain(
                    (0..relevances.len())
                        .map(|slot| (slot, "is_answer_evidence", relevances[slot])),
                )
                .collect::<Vec<_>>();
            Ok(slot_answers(&answers))
        })
    }

    /// Same relevance everywhere: `plan` keeps the base order verbatim.
    fn default_params() -> PostParams {
        PostParams {
            weight: SIGNAL_WEIGHT,
            min_keep: MIN_KEEP,
            relevant_min: 0.45,
            instructions_max: 0.85,
            top_n: 10,
            ..PostParams::default()
        }
    }

    #[tokio::test]
    async fn test_judged_order_keeps_base_order_when_relevance_is_uniform() {
        // Fixture names are globally unique: `judge_results` caches verdicts
        // process-wide keyed by (query, file, symbol, …), so tests sharing a
        // fixture namespace inherit each other's verdicts in any order.
        let results = vec![
            result("u0", "uniform_a.rs", "uniform_alpha", 0.9),
            result("u1", "uniform_b.rs", "uniform_beta", 0.8),
            result("u2", "uniform_c.rs", "uniform_gamma", 0.7),
        ];
        let docs = docs_of(&results);
        let asker = relevance_asker(&[0.6, 0.6, 0.6]);
        let ordered = judge_with(
            &asker,
            "uniform relevance",
            &results,
            &docs,
            "/root",
            1_000,
            &default_params(),
        )
        .await
        .expect("uniform relevance should produce an order");
        let ids: Vec<&str> = ordered.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["u0", "u1", "u2"]);
    }

    #[tokio::test]
    async fn test_judge_promotes_highly_relevant_candidate_over_base_leader() {
        // Same candidate SHAPE as the uniform-relevance test, different
        // answers — so the fixtures must live in their own cache namespace.
        let results = vec![
            result("p0", "promote_a.rs", "promote_alpha", 0.9),
            result("p1", "promote_b.rs", "promote_beta", 0.8),
            result("p2", "promote_c.rs", "promote_gamma", 0.7),
        ];
        let docs = docs_of(&results);
        let asker = relevance_asker(&[0.0, 0.0, 1.0]);
        // min_keep: 1 so the drop rail doesn't restore every candidate; the
        // kernel's production MIN_KEEP=3 would keep all three on this page.
        let params = PostParams {
            min_keep: 1,
            ..default_params()
        };
        let ordered = judge_with(
            &asker,
            "promote relevant",
            &results,
            &docs,
            "/root",
            1_000,
            &params,
        )
        .await
        .expect("complete verdicts should produce an order");
        let ids: Vec<&str> = ordered.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.first(), Some(&"p2"), "judged hit p2 must lead: {ids:?}");
        // Dropped (below relevant_min) candidates are ABSENT from the order —
        // the dispatch remap demotes them to the tail. Slot 0 is never dropped
        // (`plan` rail), so p0 stays at position 2.
        assert_eq!(ids, ["p2", "p0"], "p1 demoted by the judge: {ids:?}");
    }

    #[tokio::test]
    async fn test_incomplete_verdicts_fail_open_to_original_order() {
        let results = vec![
            result("i0", "incomplete_a.rs", "incomplete_alpha", 0.9),
            result("i1", "incomplete_b.rs", "incomplete_beta", 0.8),
        ];
        let docs = docs_of(&results);
        // Asker that answers nothing: every verdict stays None.
        let asker = FnAsker(|_request: &Request| Ok(Response::default()));
        let ordered = judge_with(
            &asker,
            "incomplete verdicts",
            &results,
            &docs,
            "/root",
            1_000,
            &default_params(),
        )
        .await;
        assert!(
            ordered.is_none(),
            "no verdicts must fail open, got {ordered:?}"
        );
    }

    #[tokio::test]
    async fn test_provider_error_fails_open_to_original_order() {
        let results = vec![result("e0", "error_a.rs", "error_alpha", 0.9)];
        let docs = docs_of(&results);
        let asker = FnAsker(|_request: &Request| -> Result<Response, SystemOneError> {
            Err(SystemOneError::Transport(
                "stub transport failure".to_string(),
            ))
        });
        let ordered = judge_with(
            &asker,
            "provider error",
            &results,
            &docs,
            "/root",
            1_000,
            &default_params(),
        )
        .await;
        assert!(ordered.is_none(), "transport failure must fail open");
    }

    #[test]
    fn test_candidates_carry_dispatch_fields_for_the_judge_wire() {
        let results = vec![result("n0", "src/a.rs", "alpha", 0.9)];
        let docs = docs_of(&results);
        let values = candidate_values(&results, &docs);
        let value = &values[0];
        assert_eq!(value["file_path"], "src/a.rs");
        assert_eq!(value["symbol_name"], "alpha");
        assert_eq!(value["symbol_type"], "function");
        assert_eq!(value["signature"], "fn alpha()");
        assert_eq!(value["context"], "code for alpha");
    }
}
