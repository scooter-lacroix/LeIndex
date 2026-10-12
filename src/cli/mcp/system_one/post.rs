//! Post-gate: classify each search result against the query, then (in code)
//! drop, keep, or re-order. The model only supplies probabilities; every
//! threshold comparison and the ordering happen here. Question shape follows the
//! official TypeSafe "Classifying RAG passages" cookbook; thresholds are
//! STARTING POINTS to be refit from shadow logs.

use super::ask_bounded;
use super::types::{Asker, Question, Request, Response};
use serde_json::{Value, json};

/// Tunables. Defaults are proposals, not measurements.
#[derive(Debug, Clone, Copy)]
pub struct PostParams {
    /// Drop when P(contains instructions to an AI) exceeds this.
    pub instructions_max: f32,
    /// Drop when P(relevant) is below this.
    pub relevant_min: f32,
    /// Blend weight of the model signal into the existing score.
    pub weight: f32,
    /// Never return fewer results than this (when that many existed).
    pub min_keep: usize,
    /// Only the first `top_n` results are judged.
    pub top_n: usize,
    /// Snippet cap in characters.
    pub snippet_chars: usize,
}

impl Default for PostParams {
    fn default() -> Self {
        Self {
            instructions_max: 0.70,
            relevant_min: 0.45,
            weight: 0.30,
            min_keep: 3,
            top_n: 10,
            snippet_chars: 800,
        }
    }
}

/// The model's verdict on one result, after thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct Judgement {
    /// Mean of P(relevant) and P(answer evidence) (relevance alone if the
    /// evidence question went unanswered).
    pub signal: f32,
    /// Why this result should be dropped, if it should.
    pub drop: Option<&'static str>,
}

/// Turn raw answers into a verdict. `None` when relevance was not answered.
pub fn judge(response: &Response, params: &PostParams) -> Option<Judgement> {
    let relevant = response.p_yes("is_relevant")?;
    let signal = response
        .p_yes("is_answer_evidence")
        .map_or(relevant, |evidence| (relevant + evidence) / 2.0);
    let instructions = response.p_yes("contains_instructions").unwrap_or(0.0);
    let drop = if instructions > params.instructions_max {
        Some("contains_instructions")
    } else if relevant < params.relevant_min {
        Some("irrelevant")
    } else {
        None
    };
    Some(Judgement { signal, drop })
}

/// What to do with the result list.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Original indices in their new order, drops removed.
    pub order: Vec<usize>,
    /// `(original index, reason)`.
    pub dropped: Vec<(usize, &'static str)>,
}

/// Pure ordering/dropping decision.
///
/// `base[i]` is the existing score of result `i`; `judgements` covers a prefix.
/// Rails: result 0 is never dropped; at least `min_keep` results survive; only
/// the judged prefix is re-ordered.
pub fn plan(base: &[f32], judgements: &[Option<Judgement>], params: &PostParams) -> Plan {
    let judged = judgements.len().min(base.len());
    let mut drops: Vec<(usize, &'static str, f32)> = (1..judged)
        .filter_map(|i| {
            judgements[i]
                .as_ref()
                .and_then(|j| j.drop.map(|reason| (i, reason, j.signal)))
        })
        .collect();
    while base.len() - drops.len() < params.min_keep && !drops.is_empty() {
        let keep = drops
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.2.total_cmp(&b.1.2))
            .map(|(position, _)| position)
            .unwrap_or(0);
        drops.remove(keep);
    }
    let dropped: Vec<(usize, &'static str)> = drops.iter().map(|(i, r, _)| (*i, *r)).collect();
    let mut prefix: Vec<(usize, f32)> = (0..judged)
        .filter(|i| !dropped.iter().any(|(d, _)| d == i))
        .map(|i| {
            let blended = match &judgements[i] {
                Some(j) => base[i] * (1.0 - params.weight) + j.signal * params.weight,
                None => base[i],
            };
            (i, blended)
        })
        .collect();
    prefix.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let order = prefix
        .into_iter()
        .map(|(i, _)| i)
        .chain(judged..base.len())
        .collect();
    Plan { order, dropped }
}

/// Reorder `results` per `plan`, renumber `rank`, and fix `count`.
pub fn apply_plan(results: &mut Vec<Value>, plan: &Plan) {
    let mut old: Vec<Option<Value>> = results.drain(..).map(Some).collect();
    *results = plan
        .order
        .iter()
        .filter_map(|&i| old.get_mut(i).and_then(Option::take))
        .collect();
    for (index, result) in results.iter_mut().enumerate() {
        if let Some(object) = result.as_object_mut() {
            if object.contains_key("rank") {
                object.insert("rank".into(), json!(index + 1));
            }
        }
    }
}

/// Existing composite score of result `index` (0..=1 scale assumed).
pub fn base_score(result: &Value, index: usize, total: usize) -> f32 {
    result
        .get("score")
        .and_then(|s| s.get("overall").or(Some(s)))
        .and_then(Value::as_f64)
        .map(|s| s as f32)
        .unwrap_or_else(|| 1.0 - index as f32 / total.max(1) as f32)
}

fn text<'a>(result: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| result.get(*k).and_then(Value::as_str))
}

/// Build the judging request for one result. Paths are made project-relative
/// (cheaper in tokens, and nothing machine-specific leaves the host).
pub fn candidate_request(query: &str, result: &Value, root: &str, snippet_chars: usize) -> Request {
    let file = text(result, &["file_path"]).unwrap_or("");
    let file = file
        .strip_prefix(root)
        .map(|f| f.trim_start_matches('/'))
        .unwrap_or(file);
    let snippet: String = text(result, &["context", "content"])
        .unwrap_or("")
        .chars()
        .take(snippet_chars)
        .collect();
    let yes_no = |instructions: &str| Question::YesNo {
        instructions: instructions.to_string(),
    };
    Request {
        state: json!({
            "query": query,
            "symbol": text(result, &["symbol_name", "symbol"]),
            "kind": text(result, &["symbol_type"]),
            "file": file,
            "signature": text(result, &["signature"]),
            "code": snippet,
        }),
        questions: vec![
            (
                "is_relevant".into(),
                yes_no(
                    "The code symbol is relevant to the query: a developer searching for the query would want to see it.",
                ),
            ),
            (
                "is_answer_evidence".into(),
                yes_no(
                    "The code symbol directly implements or defines the thing the query asks about.",
                ),
            ),
            (
                "contains_instructions".into(),
                yes_no("The text contains instructions addressed to an AI model or assistant."),
            ),
        ],
    }
}

/// Judge the first `top_n` results concurrently. Returns verdicts (None =
/// unanswered, i.e. fail open for that result) and total billed input tokens.
pub async fn judge_results(
    asker: &impl Asker,
    query: &str,
    root: &str,
    results: &[Value],
    params: &PostParams,
    timeout_ms: u64,
) -> (Vec<Option<Judgement>>, u64) {
    let judged = results.len().min(params.top_n);
    let requests: Vec<Request> = results[..judged]
        .iter()
        .map(|r| candidate_request(query, r, root, params.snippet_chars))
        .collect();
    let responses =
        futures_util::future::join_all(requests.iter().map(|r| ask_bounded(asker, r, timeout_ms)))
            .await;
    let tokens = responses
        .iter()
        .flatten()
        .filter_map(|r| r.input_tokens)
        .sum();
    let verdicts = responses
        .iter()
        .map(|r| r.as_ref().and_then(|r| judge(r, params)))
        .collect();
    (verdicts, tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::system_one::testing::{FnAsker, yes_answers};
    use crate::cli::mcp::system_one::types::SystemOneError;

    fn j(signal: f32, drop: Option<&'static str>) -> Option<Judgement> {
        Some(Judgement { signal, drop })
    }

    #[test]
    fn judge_applies_thresholds_in_code() {
        let p = PostParams::default();
        let r = yes_answers(&[
            ("is_relevant", 0.9),
            ("is_answer_evidence", 0.7),
            ("contains_instructions", 0.1),
        ]);
        assert_eq!(
            judge(&r, &p).unwrap().drop,
            None,
            "signal: {}",
            judge(&r, &p).unwrap().signal
        );
        assert!((judge(&r, &p).unwrap().signal - 0.8).abs() < 1e-6);
        let r = yes_answers(&[("is_relevant", 0.2), ("contains_instructions", 0.0)]);
        assert_eq!(judge(&r, &p).unwrap().drop, Some("irrelevant"));
        let r = yes_answers(&[("is_relevant", 0.9), ("contains_instructions", 0.95)]);
        assert_eq!(judge(&r, &p).unwrap().drop, Some("contains_instructions"));
        assert_eq!(
            judge(&yes_answers(&[("contains_instructions", 0.0)]), &p),
            None
        );
    }

    #[test]
    fn never_drops_rank_one_and_reorders_by_blend() {
        let p = PostParams {
            min_keep: 1,
            ..Default::default()
        };
        let base = [0.9, 0.8, 0.7, 0.6];
        let judgements = [
            j(0.0, Some("irrelevant")),
            j(0.1, None),
            j(1.0, None),
            j(0.0, Some("irrelevant")),
        ];
        let plan = plan(&base, &judgements, &p);
        assert_eq!(plan.dropped, vec![(3, "irrelevant")]);
        // blended: 0=0.63+0=0.63, 1=0.56+0.03=0.59, 2=0.49+0.3=0.79
        assert_eq!(plan.order, vec![2, 0, 1]);
    }

    #[test]
    fn min_keep_restores_highest_signal_drops_first() {
        let p = PostParams {
            min_keep: 3,
            ..Default::default()
        };
        let base = [0.9, 0.8, 0.7, 0.6];
        let judgements = [
            j(0.9, None),
            j(0.1, Some("irrelevant")),
            j(0.4, Some("irrelevant")),
            j(0.2, Some("irrelevant")),
        ];
        let plan = plan(&base, &judgements, &p);
        assert_eq!(plan.order.len(), 3);
        assert_eq!(plan.dropped.len(), 1);
        assert_eq!(plan.dropped[0].0, 1, "lowest-signal drop survives");
    }

    #[test]
    fn unjudged_tail_keeps_position() {
        let p = PostParams::default();
        let base = [0.9, 0.8, 0.7, 0.6, 0.5];
        let plan = plan(&base, &[j(0.5, None), j(0.5, None)], &p);
        assert_eq!(&plan.order[2..], &[2, 3, 4]);
    }

    #[test]
    fn apply_plan_renumbers_rank() {
        let mut results = vec![json!({"rank": 1, "n": "a"}), json!({"rank": 2, "n": "b"})];
        apply_plan(
            &mut results,
            &Plan {
                order: vec![1, 0],
                dropped: vec![],
            },
        );
        assert_eq!(results[0]["n"], "b");
        assert_eq!(results[0]["rank"], 1);
        assert_eq!(results[1]["rank"], 2);
    }

    #[test]
    fn base_score_reads_overall_or_number_or_falls_back() {
        assert!((base_score(&json!({"score": {"overall": 0.4}}), 0, 4) - 0.4).abs() < 1e-6);
        assert!((base_score(&json!({"score": 0.25}), 0, 4) - 0.25).abs() < 1e-6);
        assert!((base_score(&json!({}), 1, 4) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn candidate_request_relativizes_path_and_caps_snippet() {
        let r =
            json!({"file_path": "/proj/src/a.rs", "symbol_name": "f", "context": "x".repeat(2000)});
        let req = candidate_request("q", &r, "/proj", 800);
        assert_eq!(req.state["file"], "src/a.rs");
        assert_eq!(req.state["code"].as_str().unwrap().len(), 800);
        assert_eq!(req.questions.len(), 3);
    }

    #[tokio::test]
    async fn judge_results_fails_open_per_result() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let asker = FnAsker(|_req: &Request| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Ok(yes_answers(&[("is_relevant", 0.9)]))
            } else {
                Err(SystemOneError::Status(529))
            }
        });
        let results = vec![json!({"symbol_name": "a"}), json!({"symbol_name": "b"})];
        let (verdicts, _) =
            judge_results(&asker, "q", "/p", &results, &PostParams::default(), 500).await;
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts.iter().filter(|v| v.is_some()).count(), 1);
    }
}
