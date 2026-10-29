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
use super::types::{Asker, Question, Request, Response};
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

/// Whether `value` satisfies the schema's `type`, recursing into `items` for arrays.
fn fits_type(spec: &Value, value: &Value) -> bool {
    let Some(expected) = spec.get("type").and_then(Value::as_str) else {
        return true;
    };
    if !type_matches(expected, value) {
        return false;
    }
    match (value.as_array(), spec.get("items")) {
        (Some(elements), Some(items)) => elements.iter().all(|element| fits_type(items, element)),
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
        if !fits_type(spec, value) {
            issues.push(Issue {
                param: key.clone(),
                kind: IssueKind::WrongType,
            });
            continue;
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

/// Clamp `n` into the schema's `minimum`/`maximum` (either bound optional).
fn clamp_to_spec(spec: &Value, mut n: f64) -> f64 {
    if let Some(min) = spec.get("minimum").and_then(Value::as_f64) {
        n = n.max(min);
    }
    if let Some(max) = spec.get("maximum").and_then(Value::as_f64) {
        n = n.min(max);
    }
    n
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
                let n = clamp_to_spec(spec, n.as_f64().unwrap_or(0.0));
                // Integer params round; number params keep their fraction. A
                // blanket `as i64` truncated floats (clamped 2.5 became 2) and
                // could emit values below a fractional minimum (0.5 became 0).
                Some(
                    if spec.get("type").and_then(Value::as_str) == Some("integer") {
                        json!(n.round() as i64)
                    } else {
                        json!(n)
                    },
                )
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
                    Some("integer") => text
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .map(|n| json!(clamp_to_spec(spec, n as f64).round() as i64)),
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

/// Escape a bare token for interpolation into a JSON-pointer path (`~` and
/// `/` are the two reserved characters).
fn escape_pointer(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
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
    let original = healed.args.clone();
    let issues = validate(ctx.schema, &healed.args);
    let (issues, code_changes) = if ctx.read_only {
        repair_in_code(ctx.schema, &mut healed.args, issues)
    } else {
        (issues, Vec::new())
    };
    healed.changes.extend(code_changes);

    let wants_project = wants_project_hint(ctx, &original, &mut healed);
    if issues.is_empty() && !wants_project {
        return healed;
    }

    let Some(answers) = stage1(asker, ctx, &mut healed, intent, wants_project, params).await else {
        // Provider failed: fail open with the caller's original call, not a
        // half-repaired one (Stage 0 edits would otherwise leak through).
        healed.args = original;
        healed.changes.clear();
        healed.hints.clear();
        return healed;
    };
    apply_project_fill(&answers, ctx, &mut healed, params, wants_project);
    let flagged = flagged_issues(&answers, &issues, wants_project, params);
    if stage2(asker, ctx, &mut healed, flagged, intent, params)
        .await
        .is_none()
    {
        // Stage 2 failed: roll back the entire cascade, including Stage 1's
        // project fill and advice, while retaining the request accounting.
        healed.args = original;
        healed.changes.clear();
        healed.hints.clear();
    }
    healed
}

/// Whether the call should be offered a model-chosen `project_path`; emits the
/// no-projects hint as a side effect.
fn wants_project_hint(ctx: &CallContext<'_>, args: &Value, healed: &mut Healed) -> bool {
    let project_in_schema = ctx
        .schema
        .get("properties")
        .is_some_and(|p| p.get("project_path").is_some());
    // Offer a project only when the caller gave none. A present-but-invalid
    // `project_path` is a different failure and must not be silently replaced.
    let missing = args.get("project_path").is_none();
    let unresolved = ctx.project_unresolved && project_in_schema && missing;
    if unresolved && ctx.known_projects.is_empty() {
        healed.hints.push(
            "no project could be resolved and none is loaded: pass project_path (an absolute project directory)"
                .into(),
        );
    }
    unresolved && !ctx.known_projects.is_empty()
}

/// Apply schema string limits to evidence, not execution arguments: mutating
/// tools must still receive exactly the caller's original arguments.
fn bound_evidence(spec: &Value, value: &mut Value) {
    if let (Some(limit), Some(text)) = (
        spec.get("maxLength").and_then(Value::as_u64),
        value.as_str(),
    ) {
        *value = json!(text.chars().take(limit as usize).collect::<String>());
        return;
    }
    match value {
        Value::Array(items) => {
            if let Some(item_spec) = spec.get("items") {
                for item in items {
                    bound_evidence(item_spec, item);
                }
            }
        }
        Value::Object(fields) => {
            if let Some(properties) = spec.get("properties") {
                for (name, field) in fields {
                    if let Some(field_spec) = properties.get(name) {
                        bound_evidence(field_spec, field);
                    }
                }
            }
        }
        _ => {}
    }
}

fn request_state(ctx: &CallContext<'_>, args: &Value, intent: Option<&str>) -> Value {
    let mut evidence = args.clone();
    bound_evidence(ctx.schema, &mut evidence);
    // Gate::pre derives intent from task_context or query; cap its duplicate
    // by the same schema limit rather than transmitting the original text.
    let intent_spec = ctx.schema.get("properties").and_then(|p| {
        p.get(if args.get("task_context").is_some() {
            "task_context"
        } else {
            "query"
        })
    });
    let mut intent = json!(intent);
    if let Some(spec) = intent_spec {
        bound_evidence(spec, &mut intent);
    }
    json!({"tool": ctx.tool, "arguments": evidence, "intent": intent})
}

/// Stage 1: one batched request (`call_ok` + one `wrong:<param>` yes/no per
/// parameter + the project questions when needed).
async fn stage1(
    asker: &impl Asker,
    ctx: &CallContext<'_>,
    healed: &mut Healed,
    intent: Option<&str>,
    wants_project: bool,
    params: &HealParams,
) -> Option<Response> {
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
        state: request_state(ctx, &healed.args, intent),
        questions,
    };
    healed.requests += 1;
    let answers = ask_bounded(asker, &stage1, params.timeout_ms).await?;
    // Missing/refused verdicts are provider failures, not evidence of a bad
    // argument. Propagate failure so heal rolls back the entire cascade.
    for (name, question) in &stage1.questions {
        match question {
            Question::YesNo { .. } => {
                answers.p_yes(name)?;
            }
            Question::Pick { .. } => {
                answers.pick(name)?;
            }
        }
    }
    Some(answers)
}

/// Fill in `project_path` when Stage 1 said the call needs one.
fn apply_project_fill(
    answers: &Response,
    ctx: &CallContext<'_>,
    healed: &mut Healed,
    params: &HealParams,
    wants_project: bool,
) {
    if !wants_project || answers.p_yes("needs:project_path").unwrap_or(0.0) <= params.needs_min {
        return;
    }
    let Some((choice, confidence)) = answers.pick("fill:project_path") else {
        return;
    };
    let is_known = ctx.known_projects.iter().any(|p| p == choice);
    apply_choice(
        healed,
        ctx,
        params,
        Choice {
            param: "project_path",
            from: Value::Null,
            to: json!(choice),
            confidence,
            offered: is_known,
        },
    );
}

/// Hard violations are known-bad by code; soft flags need the model's vote.
fn flagged_issues<'i>(
    answers: &Response,
    issues: &'i [Issue],
    wants_project: bool,
    params: &HealParams,
) -> Vec<&'i Issue> {
    let call_ok = answers.p_yes("call_ok").unwrap_or(1.0);
    issues
        .iter()
        .filter(|i| i.kind != IssueKind::Missing || i.param != "project_path" || !wants_project)
        .filter(|i| {
            call_ok < params.ok_min
                || answers.p_yes(&format!("wrong:{}", i.param)).unwrap_or(0.0) > params.wrong_min
        })
        .collect()
}

/// Stage 2: one batched pick request per flagged parameter, over
/// code-enumerated candidates plus `keep_as_is`. `None` signals a provider
/// failure so the caller can roll back the entire cascade.
async fn stage2(
    asker: &impl Asker,
    ctx: &CallContext<'_>,
    healed: &mut Healed,
    flagged: Vec<&Issue>,
    intent: Option<&str>,
    params: &HealParams,
) -> Option<()> {
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
        return Some(());
    }
    let stage2 = Request {
        state: request_state(ctx, &healed.args, intent),
        questions: asks,
    };
    healed.requests += 1;
    let picks = ask_bounded(asker, &stage2, params.timeout_ms).await?;
    for (issue, options) in offered {
        let (choice, confidence) = picks.pick(&format!("fix:{}", issue.param))?;
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
            healed,
            ctx,
            params,
            Choice {
                param: &issue.param,
                from,
                to,
                confidence,
                offered: true,
            },
        );
    }
    Some(())
}

/// One model-chosen repair to apply (or, for a mutating tool, hint at).
struct Choice<'a> {
    param: &'a str,
    from: Value,
    to: Value,
    confidence: f32,
    /// False when the model picked a value code never offered.
    offered: bool,
}

fn apply_choice(
    healed: &mut Healed,
    ctx: &CallContext<'_>,
    params: &HealParams,
    choice: Choice<'_>,
) {
    let Choice {
        param,
        from,
        to,
        confidence,
        offered,
    } = choice;
    if !offered {
        // The model picked a value code never offered (e.g. a `project_path`
        // that is not a loaded project). Reject it — but say so, so the
        // calling model learns the pick was refused instead of assuming the
        // fill succeeded.
        healed.hints.push(format!(
            "refusing to set `{param}` to {to}: not one of the code-enumerated candidates"
        ));
        return;
    }
    if ctx.read_only && confidence >= params.tau_apply {
        if let Some(object) = healed.args.as_object_mut() {
            // Unknown-key repair is a rename. The discriminator is the pair
            // (param unknown to the schema, `to` a string naming a real
            // schema property) — NOT the type of the value being carried
            // (a `topk: 5` repair must move the 5, not stringify it).
            if let Some(target) = to.as_str() {
                let param_known = ctx
                    .schema
                    .pointer(&format!("/properties/{}", escape_pointer(param)))
                    .is_some();
                let target_known = ctx
                    .schema
                    .pointer(&format!("/properties/{}", escape_pointer(target)))
                    .is_some();
                if !param_known && target_known && object.get(param).is_some() {
                    if let Some(value) = object.remove(param) {
                        object.insert(target.to_string(), value);
                    }
                    healed.changes.push(Change {
                        param: param.into(),
                        from,
                        to,
                        by: "model",
                    });
                    return;
                }
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
mod test {
    use super::*;
    use crate::cli::mcp::system_one::testing::{FnAsker, answers_of};
    use crate::cli::mcp::system_one::types::{Answer, SystemOneError};

    fn requested_answers(req: &Request, pairs: &[(&str, f32)]) -> Response {
        let mut response = answers_of(pairs);
        for (name, question) in &req.questions {
            if matches!(question, Question::YesNo { .. }) {
                response
                    .answers
                    .entry(name.clone())
                    .or_insert(Answer::YesNo { p_yes: 0.0 });
            }
        }
        response
    }

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
    fn test_project_autofill_only_when_project_path_is_absent() {
        // Regression: an invalid but present `project_path` used to be
        // offered a replacement project. Only a missing one may be filled.
        let schema = schema();
        let projects = vec!["/srv/a".to_string(), "/srv/b".to_string()];
        let context = ctx(&schema, true, &projects);
        let mut healed = Healed::default();
        let missing = json!({"query": "q"});
        assert!(wants_project_hint(&context, &missing, &mut healed));
        let given = json!({"query": "q", "project_path": "/nope"});
        assert!(!wants_project_hint(&context, &given, &mut healed));
    }

    #[test]
    fn test_validate_finds_each_violation_kind() {
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
    fn test_code_repairs_clamp_case_and_coercion_only() {
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
    fn test_unknown_key_rename_carries_the_original_value_type() {
        // Regression: `topk: 5` used to fall past the rename branch (which
        // tested `from.is_string()`) and produce `topk: "top_k"`, dropping
        // the value. The rename must be keyed on the unknown-key/target-key
        // pair, not on the carried value's JSON type.
        let schema = schema();
        let context = ctx(&schema, true, &[]);
        let mut healed = Healed {
            args: json!({"query": "q", "topk": 5}),
            ..Healed::default()
        };
        apply_choice(
            &mut healed,
            &context,
            &HealParams::default(),
            Choice {
                param: "topk",
                from: json!(5),
                to: json!("top_k"),
                confidence: 0.95,
                offered: true,
            },
        );
        assert_eq!(healed.args, json!({"query": "q", "top_k": 5}));
        assert_eq!(healed.changes.len(), 1);
        assert_eq!(healed.changes[0].param, "topk");
        assert_eq!(healed.changes[0].to, json!("top_k"));
    }

    #[test]
    fn test_known_key_repair_is_a_set_not_a_rename() {
        // When the flagged param IS in the schema, the model's choice is a
        // value replacement — the rename branch must not swallow it even
        // when `to` happens to name another property.
        let schema = schema();
        let context = ctx(&schema, true, &[]);
        let mut healed = Healed {
            args: json!({"query": "q", "search_mode": "fuzzy"}),
            ..Healed::default()
        };
        apply_choice(
            &mut healed,
            &context,
            &HealParams::default(),
            Choice {
                param: "search_mode",
                from: json!("fuzzy"),
                to: json!("auto"),
                confidence: 0.95,
                offered: true,
            },
        );
        assert_eq!(healed.args, json!({"query": "q", "search_mode": "auto"}));
    }

    #[test]
    fn test_ambiguous_enum_case_is_not_guessed() {
        let s = json!({"properties": {"m": {"type": "string", "enum": ["Ab", "aB"]}}});
        let mut args = json!({"m": "ab"});
        let issues = validate(&s, &args);
        let (left, _) = repair_in_code(&s, &mut args, issues);
        assert_eq!(left.len(), 1);
        assert_eq!(args["m"], "ab");
    }

    #[tokio::test]
    async fn test_clean_call_costs_zero_requests() {
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
    async fn test_model_picks_candidate_and_read_only_call_is_repaired() {
        let asker = FnAsker(|req: &Request| {
            let mut r = requested_answers(req, &[("call_ok", 0.1), ("wrong:search_mode", 0.9)]);
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
            let mut r = requested_answers(req, &[("call_ok", 0.0), ("wrong:search_mode", 1.0)]);
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
    async fn test_low_confidence_and_keep_as_is_change_nothing() {
        let asker = FnAsker(|req: &Request| {
            let mut r = requested_answers(req, &[("call_ok", 0.0), ("wrong:search_mode", 1.0)]);
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
    async fn test_provider_failure_fails_open() {
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
    async fn test_stage2_failure_rolls_back_code_repairs_project_fill_and_hints() {
        let projects = vec!["/a".to_string()];
        let args = json!({"query": "q", "top_k": 500, "search_mode": "fuzzy", "colour": 1});
        let s = schema();
        for read_only in [true, false] {
            let asker = FnAsker(|req: &Request| {
                if req
                    .questions
                    .iter()
                    .any(|(name, _)| name == "fix:search_mode")
                {
                    // Stage 0 repairs and Stage 1 fill (or hint for a mutating
                    // tool) have already happened before the provider fails.
                    if read_only {
                        assert_eq!(req.state["arguments"]["top_k"], 100);
                        assert_eq!(req.state["arguments"]["project_path"], "/a");
                    }
                    return Err(SystemOneError::Status(529));
                }
                let mut response =
                    requested_answers(req, &[("call_ok", 0.0), ("needs:project_path", 1.0)]);
                response.answers.insert(
                    "fill:project_path".into(),
                    Answer::Pick {
                        choice: "/a".into(),
                        probabilities: vec![],
                        confidence: 1.0,
                    },
                );
                Ok(response)
            });
            let healed = heal(
                &asker,
                &ctx(&s, read_only, &projects),
                args.clone(),
                None,
                &HealParams::default(),
            )
            .await;
            assert_eq!(healed.requests, 2);
            assert_eq!(healed.args, args);
            assert!(healed.changes.is_empty());
            assert!(healed.hints.is_empty());
        }
    }

    #[tokio::test]
    async fn test_stage1_failure_rolls_back_code_repairs_and_hints() {
        let asker = FnAsker(|_: &Request| Err(SystemOneError::Status(529)));
        let s = schema();
        let mut context = ctx(&s, true, &[]);
        context.project_unresolved = true;
        let args = json!({"query": "q", "top_k": 500, "search_mode": "fuzzy"});
        let healed = heal(&asker, &context, args.clone(), None, &HealParams::default()).await;
        assert_eq!(healed.requests, 1);
        assert_eq!(healed.args, args);
        assert!(healed.changes.is_empty());
        assert!(healed.hints.is_empty());
    }

    #[tokio::test]
    async fn test_missing_project_path_is_filled_only_from_known_projects() {
        let projects = vec!["/a".to_string(), "/b".to_string()];
        let asker = FnAsker(|req: &Request| {
            let mut r = requested_answers(req, &[("call_ok", 0.9), ("needs:project_path", 0.95)]);
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
    async fn test_invented_project_path_is_rejected() {
        let projects = vec!["/a".to_string(), "/b".to_string()];
        let asker = FnAsker(|req: &Request| {
            let mut r = requested_answers(req, &[("call_ok", 0.9), ("needs:project_path", 0.95)]);
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
        // The refusal must be visible to the checking agent, not a silent drop.
        assert_eq!(healed.hints.len(), 1);
        assert!(healed.hints[0].contains("refusing to set `project_path`"));
        assert!(healed.hints[0].contains("/etc"));
    }

    #[tokio::test]
    async fn test_stage1_missing_parameter_verdict_rolls_back_every_repair() {
        let s = schema();
        let original = json!({"query": "q", "top_k": 500, "search_mode": "fuzzy"});
        let asker = FnAsker(|req: &Request| {
            if req.questions.iter().any(|(n, _)| n == "fix:search_mode") {
                let mut response = Response::default();
                response.answers.insert(
                    "fix:search_mode".into(),
                    Answer::Pick {
                        choice: "exact".into(),
                        probabilities: vec![],
                        confidence: 1.0,
                    },
                );
                return Ok(response);
            }
            Ok(answers_of(&[
                ("call_ok", 0.0),
                ("wrong:query", 0.0),
                ("wrong:top_k", 0.0),
            ]))
        });
        let healed = heal(
            &asker,
            &ctx(&s, true, &[]),
            original.clone(),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.args, original);
        assert_eq!(healed.requests, 1);
        assert!(healed.changes.is_empty());
        assert!(healed.hints.is_empty());
    }

    #[tokio::test]
    async fn test_schema_string_limits_bound_both_provider_stages_and_intent() {
        let mut s = schema();
        s["properties"]["task_context"] = json!({"type": "string", "maxLength": 3});
        let original =
            json!({"query": "q", "search_mode": "fuzzy", "task_context": "é".repeat(10000)});
        for read_only in [true, false] {
            let asker = FnAsker(|req: &Request| {
                assert_eq!(req.state["arguments"]["task_context"], "ééé");
                assert_eq!(req.state["intent"], "ééé");
                let mut response = answers_of(&[
                    ("call_ok", 0.0),
                    ("wrong:query", 0.0),
                    ("wrong:task_context", 0.0),
                    ("wrong:search_mode", 1.0),
                ]);
                response.answers.insert(
                    "fix:search_mode".into(),
                    Answer::Pick {
                        choice: "keep_as_is".into(),
                        probabilities: vec![],
                        confidence: 1.0,
                    },
                );
                Ok(response)
            });
            let healed = heal(
                &asker,
                &ctx(&s, read_only, &[]),
                original.clone(),
                original["task_context"].as_str(),
                &HealParams::default(),
            )
            .await;
            assert_eq!(healed.requests, 2);
            assert_eq!(healed.args, original);
        }
    }

    #[test]
    fn test_float_bounds_and_string_integer_coercion_respect_schema() {
        let s = json!({"properties": {
            "ratio": {"type": "number", "minimum": 0.5, "maximum": 2.5},
            "top_k": {"type": "integer", "minimum": 1, "maximum": 100}
        }});
        for (given, expected) in [(json!(0.1), json!(0.5)), (json!(9.5), json!(2.5))] {
            let mut args = json!({"ratio": given, "top_k": "500"});
            let (remaining, changes) = repair_in_code(
                &s,
                &mut args,
                validate(&s, &json!({"ratio": given, "top_k": "500"})),
            );
            assert!(remaining.is_empty());
            assert_eq!(args["ratio"], expected);
            assert_eq!(args["top_k"], 100);
            assert_eq!(changes.len(), 2);
            assert!(validate(&s, &args).is_empty());
        }
    }

    #[test]
    fn test_array_item_types_are_checked_recursively() {
        let s = json!({"properties": {
            "symbols": {"type": "array", "items": {"type": "string"}},
            "matrix": {"type": "array", "items": {"type": "array", "items": {"type": "integer"}}}
        }});
        let issues = validate(
            &s,
            &json!({"symbols": [123, "valid"], "matrix": [[1], ["bad"]]}),
        );
        assert_eq!(issues.len(), 2);
        assert!(issues.iter().all(|i| i.kind == IssueKind::WrongType));
        assert!(validate(&s, &json!({"symbols": ["valid"], "matrix": [[1], [2, 3]]})).is_empty());
    }

    #[tokio::test]
    async fn test_stage2_missing_answer_rolls_back_prior_changes() {
        let s = schema();
        let original = json!({"query": "q", "top_k": 500, "search_mode": "fuzzy"});
        let asker = FnAsker(|req: &Request| {
            Ok(requested_answers(
                req,
                &[("call_ok", 0.0), ("wrong:search_mode", 1.0)],
            ))
        });
        let healed = heal(
            &asker,
            &ctx(&s, true, &[]),
            original.clone(),
            None,
            &HealParams::default(),
        )
        .await;
        assert_eq!(healed.requests, 2);
        assert_eq!(healed.args, original);
        assert!(healed.changes.is_empty());
        assert!(healed.hints.is_empty());
    }

    #[test]
    fn test_unknown_key_candidates_are_near_schema_names() {
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
