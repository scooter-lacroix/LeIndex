//! Pre-gate: validate a resolved tool call against its handler schema in code,
//! repair what is unambiguous in code, and ask the System-One model only to
//! choose among candidates that code enumerated. Never generates a value.
//!
//! Cascade (requests are batched per stage; 0 requests on the happy path):
//!   Stage 0  code: schema validation + deterministic repair (no model)
//!   Stage 1  one request: `call_ok` + one `wrong:<param>` yes/no per parameter
//!            + `needs:project_path` / `fill:project_path` when it is absent
//!   Stage 2  one request: one `fix:<param>` pick per flagged parameter, over
//!            code-enumerated candidates plus `keep_as_is`

use super::ask_bounded;
use super::types::{Asker, Question, Request};
use crate::cli::mcp::grouped::edit_distance;
use serde_json::{Map, Value, json};

const KEEP: &str = "keep_as_is";

/// Tunables (proposals, not measurements).
#[derive(Debug, Clone)]
pub struct HealParams {
    /// Auto-apply a model-chosen repair only at or above this confidence.
    pub tau_apply: f32,
    /// A parameter is flagged when P(wrong) exceeds this.
    pub wrong_min: f32,
    /// Stage 2 runs only when P(call ok) is below this.
    pub ok_min: f32,
    /// `needs:project_path` must exceed this to fill it in.
    pub needs_min: f32,
    /// Per-request timeout.
    pub timeout_ms: u64,
}

impl Default for HealParams {
    fn default() -> Self {
        Self {
            tau_apply: 0.80,
            wrong_min: 0.60,
            ok_min: 0.50,
            needs_min: 0.60,
            timeout_ms: 150,
        }
    }
}

/// What the call is and where it may be repaired.
#[derive(Debug, Clone)]
pub struct CallContext<'a> {
    /// Underlying tool name, e.g. `leindex_search`.
    pub tool: &'a str,
    /// The handler's `argument_schema()`.
    pub schema: &'a Value,
    /// Mutating tools are never auto-repaired.
    pub read_only: bool,
    /// Candidates for a missing `project_path` (loaded projects).
    pub known_projects: &'a [String],
    /// True when the registry could NOT resolve a project for this call (no
    /// `project_path`, no startup project, CWD unusable). Only then is a
    /// missing `project_path` a failure worth a model request.
    pub project_unresolved: bool,
}

/// One schema violation found in code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueKind {
    /// Parameter is not in the schema.
    Unknown,
    /// Required parameter is absent.
    Missing,
    /// JSON type differs from the schema.
    WrongType,
    /// Value is not one of the schema's `enum` values.
    NotInEnum,
    /// Number outside the schema's `minimum`/`maximum`.
    OutOfRange,
}

/// A violation of the schema by one parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Parameter name.
    pub param: String,
    /// What is wrong.
    pub kind: IssueKind,
}

/// A change made (or proposed) to the call.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// Parameter name.
    pub param: String,
    /// Previous value (`null` when absent).
    pub from: Value,
    /// New value.
    pub to: Value,
    /// `code` or `model`.
    pub by: &'static str,
}

/// Result of healing a call.
#[derive(Debug, Clone, Default)]
pub struct Healed {
    /// The (possibly repaired) arguments.
    pub args: Value,
    /// Applied changes.
    pub changes: Vec<Change>,
    /// Advice for the calling model where nothing was applied.
    pub hints: Vec<String>,
    /// Model requests spent.
    pub requests: u32,
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    }
}

/// Stage 0a: find every schema violation. Pure.
pub fn validate(schema: &Value, args: &Value) -> Vec<Issue> {
    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let provided = args.as_object().unwrap_or(&empty);
    let mut issues = Vec::new();
    for (key, value) in provided {
        let Some(spec) = properties.get(key) else {
            issues.push(Issue {
                param: key.clone(),
                kind: IssueKind::Unknown,
            });
            continue;
        };
        if let Some(expected) = spec.get("type").and_then(Value::as_str) {
            if !type_matches(expected, value) {
                issues.push(Issue {
                    param: key.clone(),
                    kind: IssueKind::WrongType,
                });
                continue;
            }
        }
        if let Some(options) = spec.get("enum").and_then(Value::as_array) {
            if !options.contains(value) {
                issues.push(Issue {
                    param: key.clone(),
                    kind: IssueKind::NotInEnum,
                });
                continue;
            }
        }
        if let Some(n) = value.as_f64() {
            let below = spec
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|m| n < m);
            let above = spec
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|m| n > m);
            if below || above {
                issues.push(Issue {
                    param: key.clone(),
                    kind: IssueKind::OutOfRange,
                });
            }
        }
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !provided.contains_key(name) {
                issues.push(Issue {
                    param: name.to_string(),
                    kind: IssueKind::Missing,
                });
            }
        }
    }
    issues.sort_by(|a, b| a.param.cmp(&b.param));
    issues
}

/// Stage 0b: repairs that have exactly one correct answer. Returns the
/// issues left over.
pub fn repair_in_code(
    schema: &Value,
    args: &mut Value,
    issues: Vec<Issue>,
) -> (Vec<Issue>, Vec<Change>) {
    let mut left = Vec::new();
    let mut changes = Vec::new();
    for issue in issues {
        let spec = schema.get("properties").and_then(|p| p.get(&issue.param));
        let current = args.get(&issue.param).cloned();
        let fixed = match (&issue.kind, spec, &current) {
            (IssueKind::OutOfRange, Some(spec), Some(Value::Number(n))) => {
                let mut n = n.as_f64().unwrap_or(0.0);
                if let Some(min) = spec.get("minimum").and_then(Value::as_f64) {
                    n = n.max(min);
                }
                if let Some(max) = spec.get("maximum").and_then(Value::as_f64) {
                    n = n.min(max);
                }
                Some(json!(n as i64))
            }
            (IssueKind::NotInEnum, Some(spec), Some(Value::String(text))) => spec
                .get("enum")
                .and_then(Value::as_array)
                .and_then(|options| {
                    let mut hits = options.iter().filter(|o| {
                        o.as_str()
                            .is_some_and(|o| o.eq_ignore_ascii_case(text.trim()))
                    });
                    match (hits.next(), hits.next()) {
                        (Some(one), None) => Some(one.clone()),
                        _ => None,
                    }
                }),
            (IssueKind::WrongType, Some(spec), Some(Value::String(text))) => {
                match spec.get("type").and_then(Value::as_str) {
                    Some("integer") => text.trim().parse::<i64>().ok().map(|n| json!(n)),
                    Some("boolean") => match text.trim().to_ascii_lowercase().as_str() {
                        "true" => Some(json!(true)),
                        "false" => Some(json!(false)),
                        _ => None,
                    },
                    _ => None,
                }
            }
            _ => None,
        };
        match (fixed, current) {
            (Some(to), from) => {
                let from = from.unwrap_or(Value::Null);
                if let Some(object) = args.as_object_mut() {
                    object.insert(issue.param.clone(), to.clone());
                }
                changes.push(Change {
                    param: issue.param,
                    from,
                    to,
                    by: "code",
                });
            }
            _ => left.push(issue),
        }
    }
    (left, changes)
}

/// Candidates for repairing `issue`, enumerated from the schema and context.
pub fn candidates(ctx: &CallContext<'_>, args: &Value, issue: &Issue) -> Vec<Value> {
    let properties = ctx.schema.get("properties").and_then(Value::as_object);
    match issue.kind {
        IssueKind::NotInEnum => properties
            .and_then(|p| p.get(&issue.param))
            .and_then(|spec| spec.get("enum"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        IssueKind::Unknown => {
            let given = args.as_object();
            let mut near: Vec<(usize, &String)> = properties
                .into_iter()
                .flatten()
                .map(|(name, _)| name)
                .filter(|name| !given.is_some_and(|g| g.contains_key(*name)))
                .map(|name| (edit_distance(&issue.param, name), name))
                .filter(|(distance, name)| *distance <= 3.max(name.len() / 3))
                .collect();
            near.sort();
            near.into_iter()
                .take(4)
                .map(|(_, name)| json!(name))
                .collect()
        }
        IssueKind::Missing if issue.param == "project_path" => {
            ctx.known_projects.iter().map(|p| json!(p)).collect()
        }
        _ => Vec::new(),
    }
}

fn describe(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string)
}

fn pick_question(param: &str, current: &Value, candidates: &[Value]) -> Question {
    let mut options: Vec<(String, String)> = candidates
        .iter()
        .map(|c| (describe(c), format!("use {} for `{param}`", describe(c))))
        .collect();
    options.push((
        KEEP.into(),
        format!("leave `{param}` as it is ({})", describe(current)),
    ));
    Question::Pick {
        instructions: format!(
            "Which value of `{param}` makes this tool call match what the caller is trying to do?"
        ),
        options,
    }
}

/// Run the cascade. Fails open: any unanswered request leaves the call as is.
pub async fn heal(
    asker: &impl Asker,
    ctx: &CallContext<'_>,
    args: Value,
    intent: Option<&str>,
    params: &HealParams,
) -> Healed {
    let mut healed = Healed {
        args,
        ..Healed::default()
    };
    let issues = validate(ctx.schema, &healed.args);
    let (issues, code_changes) = if ctx.read_only {
        repair_in_code(ctx.schema, &mut healed.args, issues)
    } else {
        (issues, Vec::new())
    };
    healed.changes.extend(code_changes);

    let project_in_schema = ctx
        .schema
        .get("properties")
        .is_some_and(|p| p.get("project_path").is_some());
    let unresolved = ctx.project_unresolved && project_in_schema;
    if unresolved && ctx.known_projects.is_empty() {
        healed.hints.push(
            "no project could be resolved and none is loaded: pass project_path (an absolute project directory)"
                .into(),
        );
    }
    let wants_project = unresolved && !ctx.known_projects.is_empty();
    if issues.is_empty() && !wants_project {
        return healed;
    }

    // Stage 1: one batched request.
    let supplied: Vec<String> = healed
        .args
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let yes_no = |text: String| Question::YesNo { instructions: text };
    let mut questions = vec![(
        "call_ok".to_string(),
        yes_no("The tool call is well formed: every parameter value is valid and suitable for the stated intent.".into()),
    )];
    for param in &supplied {
        questions.push((
            format!("wrong:{param}"),
            yes_no(format!(
                "The value given for parameter `{param}` is wrong or unsuitable for this tool call."
            )),
        ));
    }
    if wants_project {
        questions.push((
            "needs:project_path".into(),
            yes_no("This tool call needs an explicit `project_path` because several projects are known.".into()),
        ));
        let options: Vec<(String, String)> = ctx
            .known_projects
            .iter()
            .map(|p| (p.clone(), format!("the project at {p}")))
            .chain(std::iter::once((
                KEEP.to_string(),
                "no project_path".to_string(),
            )))
            .collect();
        questions.push((
            "fill:project_path".into(),
            Question::Pick {
                instructions: "Which known project does this tool call refer to?".into(),
                options,
            },
        ));
    }
    let stage1 = Request {
        state: json!({"tool": ctx.tool, "arguments": healed.args, "intent": intent}),
        questions,
    };
    healed.requests += 1;
    let Some(answers) = ask_bounded(asker, &stage1, params.timeout_ms).await else {
        return healed;
    };

    if wants_project && answers.p_yes("needs:project_path").unwrap_or(0.0) > params.needs_min {
        if let Some((choice, confidence)) = answers.pick("fill:project_path") {
            let is_known = ctx.known_projects.iter().any(|p| p == choice);
            apply_choice(
                &mut healed,
                ctx,
                params,
                "project_path",
                Value::Null,
                json!(choice),
                confidence,
                is_known,
            );
        }
    }

    let call_ok = answers.p_yes("call_ok").unwrap_or(1.0);
    let flagged: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.kind != IssueKind::Missing || i.param != "project_path" || !wants_project)
        .filter(|i| {
            // Hard violations are known-bad by code; soft flags need the model's vote.
            call_ok < params.ok_min
                || answers.p_yes(&format!("wrong:{}", i.param)).unwrap_or(1.0) > params.wrong_min
        })
        .collect();
    let mut asks: Vec<(String, Question)> = Vec::new();
    let mut offered: Vec<(&Issue, Vec<Value>)> = Vec::new();
    for issue in flagged {
        let options = candidates(ctx, &healed.args, issue);
        if options.is_empty() {
            healed.hints.push(format!(
                "parameter `{}` violates the tool schema ({:?}); no safe candidates exist",
                issue.param, issue.kind
            ));
            continue;
        }
        let current = healed
            .args
            .get(&issue.param)
            .cloned()
            .unwrap_or(Value::Null);
        asks.push((
            format!("fix:{}", issue.param),
            pick_question(&issue.param, &current, &options),
        ));
        offered.push((issue, options));
    }
    if asks.is_empty() {
        return healed;
    }

    // Stage 2: one batched pick request.
    let stage2 = Request {
        state: json!({"tool": ctx.tool, "arguments": healed.args, "intent": intent}),
        questions: asks,
    };
    healed.requests += 1;
    let Some(picks) = ask_bounded(asker, &stage2, params.timeout_ms).await else {
        return healed;
    };
    for (issue, options) in offered {
        let Some((choice, confidence)) = picks.pick(&format!("fix:{}", issue.param)) else {
            continue;
        };
        if choice == KEEP {
            continue;
        }
        let Some(to) = options.iter().find(|o| describe(o) == choice).cloned() else {
            continue;
        };
        let from = healed
            .args
            .get(&issue.param)
            .cloned()
            .unwrap_or(Value::Null);
        apply_choice(
            &mut healed,
            ctx,
            params,
            &issue.param,
            from,
            to,
            confidence,
            true,
        );
    }
    healed
}

#[allow(clippy::too_many_arguments)]
fn apply_choice(
    healed: &mut Healed,
    ctx: &CallContext<'_>,
    params: &HealParams,
    param: &str,
    from: Value,
    to: Value,
    confidence: f32,
    is_candidate: bool,
) {
    if !is_candidate {
        return;
    }
    if ctx.read_only && confidence >= params.tau_apply {
        if let Some(object) = healed.args.as_object_mut() {
            // Unknown-key repair is a rename.
            if from.is_string()
                && object.get(param).is_some()
                && ctx
                    .schema
                    .pointer(&format!("/properties/{}", describe(&to)))
                    .is_some()
            {
                if let Some(value) = object.remove(param) {
                    object.insert(describe(&to), value);
                }
                healed.changes.push(Change {
                    param: param.into(),
                    from,
                    to,
                    by: "model",
                });
                return;
            }
            object.insert(param.into(), to.clone());
        }
        healed.changes.push(Change {
            param: param.into(),
            from,
            to,
            by: "model",
        });
    } else {
        healed.hints.push(format!(
            "consider setting `{param}` to {} (confidence {confidence:.2}; not applied{})",
            describe(&to),
            if ctx.read_only { "" } else { ": mutating tool" }
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::system_one::testing::{FnAsker, answers_of};
    use crate::cli::mcp::system_one::types::{Answer, SystemOneError};

    fn schema() -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string"},
            "project_path": {"type": "string"},
            "top_k": {"type": "integer", "minimum": 1, "maximum": 100},
            "search_mode": {"type": "string", "enum": ["code", "prose", "auto", "exact", "semantic"]},
            "allow_partial": {"type": "boolean"}
        }, "required": ["query"]})
    }

    fn ctx<'a>(schema: &'a Value, read_only: bool, projects: &'a [String]) -> CallContext<'a> {
        CallContext {
            tool: "leindex_search",
            schema,
            read_only,
            known_projects: projects,
            // `unresolved` is true only when a project IS loaded but this call
            // didn't say which one — that's the `missing_project_path` test's
            // scenario. Everywhere else the registry resolved a project fine.
            project_unresolved: !projects.is_empty(),
        }
    }

    #[test]
    fn validate_finds_each_violation_kind() {
        let issues = validate(
            &schema(),
            &json!({"top_k": 500, "search_mode": "fuzzy", "allow_partial": "yes", "colour": 1}),
        );
        let kinds: Vec<_> = issues
            .iter()
            .map(|i| (i.param.as_str(), i.kind.clone()))
            .collect();
        assert!(kinds.contains(&("top_k", IssueKind::OutOfRange)));
        assert!(kinds.contains(&("search_mode", IssueKind::NotInEnum)));
        assert!(kinds.contains(&("allow_partial", IssueKind::WrongType)));
        assert!(kinds.contains(&("colour", IssueKind::Unknown)));
        assert!(kinds.contains(&("query", IssueKind::Missing)));
        assert!(validate(&schema(), &json!({"query": "q", "top_k": 5})).is_empty());
    }

    #[test]
    fn code_repairs_clamp_case_and_coercion_only() {
        let mut args =
            json!({"query": "q", "top_k": 500, "search_mode": "SEMANTIC", "allow_partial": "true"});
        let issues = validate(&schema(), &args);
        let (left, changes) = repair_in_code(&schema(), &mut args, issues);
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(args["top_k"], 100);
        assert_eq!(args["search_mode"], "semantic");
        assert_eq!(args["allow_partial"], true);
        assert!(changes.iter().all(|c| c.by == "code"));
    }

    #[test]
    fn ambiguous_enum_case_is_not_guessed() {
        let s = json!({"properties": {"m": {"type": "string", "enum": ["Ab", "aB"]}}});
        let mut args = json!({"m": "ab"});
        let issues = validate(&s, &args);
        let (left, _) = repair_in_code(&s, &mut args, issues);
        assert_eq!(left.len(), 1);
        assert_eq!(args["m"], "ab");
    }

    #[tokio::test]
    async fn clean_call_costs_zero_requests() {
        let asker = FnAsker(|_: &Request| panic!("must not be called"));
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, true, &[]),
            json!({"query": "q"}),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.requests, 0);
        assert!(healed.changes.is_empty());
    }

    #[tokio::test]
    async fn test_read_only_call_applies_code_repairs_without_requests() {
        let asker = FnAsker(|_: &Request| panic!("must not be called"));
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, true, &[]),
            json!({"query": "q", "top_k": 500, "search_mode": "SEMANTIC", "allow_partial": "true"}),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(
            healed.args,
            json!({"query": "q", "top_k": 100, "search_mode": "semantic", "allow_partial": true})
        );
        assert_eq!(healed.requests, 0);
        assert_eq!(healed.changes.len(), 3);
        assert!(healed.changes.iter().all(|change| change.by == "code"));
    }

    #[tokio::test]
    async fn model_picks_candidate_and_read_only_call_is_repaired() {
        let asker = FnAsker(|req: &Request| {
            let mut r = answers_of(&[("call_ok", 0.1), ("wrong:search_mode", 0.9)]);
            if req.questions.iter().any(|(n, _)| n == "fix:search_mode") {
                r.answers.insert(
                    "fix:search_mode".into(),
                    Answer::Pick {
                        choice: "exact".into(),
                        probabilities: vec![],
                        confidence: 0.95,
                    },
                );
            }
            Ok(r)
        });
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, true, &[]),
            json!({"query": "q", "search_mode": "fuzzy"}),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.args["search_mode"], "exact");
        assert_eq!(healed.requests, 2);
        assert_eq!(healed.changes.len(), 1);
        assert_eq!(healed.changes[0].by, "model");
    }

    #[tokio::test]
    async fn test_mutating_tool_gets_hints_never_code_or_model_rewrites() {
        let args =
            json!({"query": "q", "top_k": 500, "search_mode": "SEMANTIC", "allow_partial": "true"});
        let asker = FnAsker(|req: &Request| {
            assert_eq!(req.state["arguments"], args);
            let mut r = answers_of(&[("call_ok", 0.0), ("wrong:search_mode", 1.0)]);
            if req.questions.iter().any(|(n, _)| n == "fix:search_mode") {
                r.answers.insert(
                    "fix:search_mode".into(),
                    Answer::Pick {
                        choice: "exact".into(),
                        probabilities: vec![],
                        confidence: 0.99,
                    },
                );
            }
            Ok(r)
        });
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, false, &[]),
            args.clone(),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.args, args);
        assert!(healed.changes.is_empty());
        assert_eq!(healed.requests, 2);
        assert_eq!(healed.hints.len(), 3);
        assert!(healed.hints.iter().any(|hint| hint.contains("`top_k`")));
        assert!(
            healed
                .hints
                .iter()
                .any(|hint| hint.contains("`allow_partial`"))
        );
    }

    #[tokio::test]
    async fn low_confidence_and_keep_as_is_change_nothing() {
        let asker = FnAsker(|req: &Request| {
            let mut r = answers_of(&[("call_ok", 0.0), ("wrong:search_mode", 1.0)]);
            if req.questions.iter().any(|(n, _)| n == "fix:search_mode") {
                r.answers.insert(
                    "fix:search_mode".into(),
                    Answer::Pick {
                        choice: "exact".into(),
                        probabilities: vec![],
                        confidence: 0.5,
                    },
                );
            }
            Ok(r)
        });
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, true, &[]),
            json!({"query": "q", "search_mode": "fuzzy"}),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.args["search_mode"], "fuzzy");
        assert_eq!(healed.hints.len(), 1);
    }

    #[tokio::test]
    async fn provider_failure_fails_open() {
        let asker = FnAsker(|_: &Request| Err(SystemOneError::Status(529)));
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, true, &[]),
            json!({"query": "q", "search_mode": "fuzzy"}),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.args["search_mode"], "fuzzy");
        assert!(healed.changes.is_empty());
    }

    #[tokio::test]
    async fn missing_project_path_is_filled_only_from_known_projects() {
        let projects = vec!["/a".to_string(), "/b".to_string()];
        let asker = FnAsker(|_: &Request| {
            let mut r = answers_of(&[("call_ok", 0.9), ("needs:project_path", 0.95)]);
            r.answers.insert(
                "fill:project_path".into(),
                Answer::Pick {
                    choice: "/b".into(),
                    probabilities: vec![],
                    confidence: 0.9,
                },
            );
            Ok(r)
        });
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, true, &projects),
            json!({"query": "q"}),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.args["project_path"], "/b");
    }

    #[tokio::test]
    async fn invented_project_path_is_rejected() {
        let projects = vec!["/a".to_string(), "/b".to_string()];
        let asker = FnAsker(|_: &Request| {
            let mut r = answers_of(&[("call_ok", 0.9), ("needs:project_path", 0.95)]);
            r.answers.insert(
                "fill:project_path".into(),
                Answer::Pick {
                    choice: "/etc".into(),
                    probabilities: vec![],
                    confidence: 0.99,
                },
            );
            Ok(r)
        });
        let s = schema();
        let healed = heal(
            &asker,
            &ctx(&s, true, &projects),
            json!({"query": "q"}),
            None,
            &HealParams::default(),
        )
        .await;
        assert!(healed.args.get("project_path").is_none());
    }

    #[test]
    fn unknown_key_candidates_are_near_schema_names() {
        let s = schema();
        let c = ctx(&s, true, &[]);
        let issue = Issue {
            param: "topk".into(),
            kind: IssueKind::Unknown,
        };
        let got = candidates(&c, &json!({"query": "q", "topk": 5}), &issue);
        assert!(got.contains(&json!("top_k")));
    }
}
