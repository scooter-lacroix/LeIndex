//! System-One gate: a typed-decision model (Laya, TypeSafe Jev, OpenAI
//! Decisions) checks tool calls before they run and classifies search results
//! before they reach the calling model.
//!
//! Invariants (enforced here and in `pre`/`post`):
//! * fail open: any provider problem leaves the call/result untouched;
//! * transport level only: handlers and the persisted search cache are unaware;
//! * repair by selection among code-enumerated candidates, never by generation;
//! * mutating tools are never rewritten, only annotated with hints;
//! * `shadow` mode computes and logs decisions but changes nothing.

pub mod client;
pub mod post;
pub mod pre;
mod scrub;
pub mod types;
pub mod wire;

#[cfg(test)]
pub(crate) mod testing;

use crate::cli::registry::ProjectRegistry;
use crate::config::{LeIndexConfig, SystemOneConfig};
use client::{HttpAsker, default_base_url, default_model};
use post::{PostParams, apply_plan, base_score, judge_results, plan, verdicts_complete};
use pre::{CallContext, Change, HealParams, heal};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tracing::{debug, info, warn};
use types::{Asker, Provider, Request, Response};

/// Run-time behaviour of the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Gate disabled (default).
    Off,
    /// Decide and log; never change a call or a result.
    Shadow,
    /// Apply decisions.
    Enforce,
}

impl Mode {
    /// Parse the `[system_one] mode` config value.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "" => Some(Self::Off),
            "shadow" => Some(Self::Shadow),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }
}

static WARNED: AtomicBool = AtomicBool::new(false);

/// One bounded request. `None` on timeout or any error (fail open). The first
/// failure per process is logged at `warn`, later ones at `debug`, so a
/// misconfigured provider is visible without flooding the log.
pub async fn ask_bounded(
    asker: &impl Asker,
    request: &Request,
    timeout_ms: u64,
) -> Option<Response> {
    let outcome = tokio::time::timeout(Duration::from_millis(timeout_ms), asker.ask(request)).await;
    let reason = match outcome {
        Ok(Ok(response)) => return Some(response),
        Ok(Err(error)) => error.to_string(),
        Err(_) => format!("timed out after {timeout_ms} ms"),
    };
    if WARNED.swap(true, Ordering::Relaxed) {
        debug!(%reason, "system-one request failed; failing open");
    } else {
        warn!(%reason, "system-one request failed; failing open (further failures logged at debug)");
    }
    None
}

/// Tools that never change project state. Anything not listed is treated as
/// mutating, so a new tool is safe by default.
pub fn is_read_only(tool: &str) -> bool {
    matches!(
        tool,
        "leindex_search"
            | "leindex_structural"
            | "leindex_deep_analyze"
            | "leindex_context"
            | "leindex_symbol_lookup"
            | "leindex_find"
            | "leindex_read_file"
            | "leindex_read_symbol"
            | "leindex_project_map"
            | "leindex_file_summary"
            | "leindex_impact_analysis"
            | "leindex_diagnostics"
            | "leindex_git_status"
            | "leindex_git_diff"
            | "leindex_phase_analysis"
            | "phase_analysis"
            | "leindex_edit_preview"
    )
}

/// What the gate did to one call; attached to the result as `_system_one`
/// (top-level `_` keys survive `trim_llm_payload` via `merge_meta`).
#[derive(Debug, Default)]
pub struct Notes {
    changes: Vec<Change>,
    hints: Vec<String>,
    dropped: Vec<&'static str>,
    reordered: bool,
}

impl Notes {
    fn is_empty(&self) -> bool {
        self.changes.is_empty()
            && self.hints.is_empty()
            && self.dropped.is_empty()
            && !self.reordered
    }

    /// Attach the annotation. Adds nothing (zero tokens) when the gate did nothing.
    pub fn attach(&self, value: &mut Value) {
        if self.is_empty() {
            return;
        }
        let Some(object) = value.as_object_mut() else {
            return;
        };
        let mut note = serde_json::Map::new();
        if !self.changes.is_empty() {
            let changes: Vec<Value> = self
                .changes
                .iter()
                .map(|c| json!({"param": c.param, "from": c.from, "to": c.to, "by": c.by}))
                .collect();
            note.insert("healed".into(), json!(changes));
        }
        if !self.hints.is_empty() {
            note.insert("hints".into(), json!(self.hints));
        }
        if !self.dropped.is_empty() {
            note.insert(
                "filtered".into(),
                json!({"count": self.dropped.len(), "reasons": self.dropped}),
            );
        }
        if self.reordered {
            note.insert("reordered".into(), json!(true));
        }
        object.insert("_system_one".into(), Value::Object(note));
    }
}

/// The configured gate.
pub struct Gate {
    mode: Mode,
    asker: HttpAsker,
    pre: bool,
    post: bool,
    heal: HealParams,
    post_params: PostParams,
    timeout_ms: u64,
}

/// The process-wide gate, built once from `[system_one]`. `None` when off or
/// misconfigured. The active preset overlay is applied first, so switching
/// presets takes effect on next process start.
pub fn global() -> Option<&'static Gate> {
    static GATE: OnceLock<Option<Gate>> = OnceLock::new();
    GATE.get_or_init(|| {
        let config = LeIndexConfig::load_cached();
        Gate::from_config(&config.system_one, &config.services)
    })
    .as_ref()
}

/// Build the provider asker from `[system_one]` — the same endpoint/key/model
/// ladder as [`Gate::from_config`] minus the gate's mode check: the search
/// reranker is independent of `mode` and needs only a configured provider
/// (plus the SystemOne feature flag). `None` when the flag is off, the
/// provider is unknown, a remote provider has no API key, or the HTTP client
/// cannot be built.
pub fn asker_from_config(
    config: &SystemOneConfig,
    services: &std::collections::BTreeMap<String, crate::config::ServiceConfig>,
) -> Option<HttpAsker> {
    let config = config.effective();
    // Progressive-rollout gate (AGENTS.md): same feature flag as the MCP gate.
    if !crate::feature_flags::FeatureFlag::SystemOne.is_enabled() {
        warn!(
            "system-one is experimental and disabled by feature flag; set LEINDEX_FEATURE_SYSTEM_ONE=1 to enable"
        );
        return None;
    }
    let Some(provider) = types::Provider::parse(&config.provider) else {
        warn!(provider = %config.provider, "unknown [system_one] provider; reranker disabled (use laya|jev|openai)");
        return None;
    };
    let service = config.service.as_deref().and_then(|name| {
        match services.get(name) {
            Some(svc) => Some(svc),
            None => {
                warn!(service = %name, "[system_one] service names no [services] section; ignoring");
                None
            }
        }
    });
    let api_key = config
        .api_key_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|key| !key.is_empty());
    if provider.is_remote() && api_key.is_none() {
        warn!(
            "[system_one] remote provider needs api_key_env naming a set environment variable; reranker disabled"
        );
        return None;
    }
    // base_url ladder: section (incl. preset) > referenced service > provider default.
    let base_url = config
        .base_url
        .clone()
        .or_else(|| service.map(|s| s.base_url()))
        .unwrap_or_else(|| default_base_url(provider).to_string());
    // model ladder: section > referenced service served name > provider default.
    let model = config
        .model
        .clone()
        .or_else(|| service.and_then(|s| s.served_name()))
        .unwrap_or_else(|| default_model(provider).to_string());
    let timeout_ms = config.timeout_ms.max(1);
    let asker = match HttpAsker::new(
        provider,
        &base_url,
        &model,
        api_key,
        Duration::from_millis(timeout_ms),
    ) {
        Ok(asker) => asker,
        Err(error) => {
            warn!(%error, "could not build the system-one HTTP client; reranker disabled");
            return None;
        }
    };
    // Managed self-healing: only for service sections that allow it.
    Some(match service {
        Some(svc) if svc.auto_start && svc.model.is_some() => {
            let name = config.service.clone().unwrap_or_default();
            asker.with_managed_service(name, svc.clone())
        }
        _ => asker,
    })
}

impl Gate {
    /// Build from config; `None` (with a warning) when disabled or invalid.
    /// `services` is the `[services.<name>]` map the section's `service` key
    /// may reference for endpoint derivation and managed start.
    pub fn from_config(
        config: &SystemOneConfig,
        services: &std::collections::BTreeMap<String, crate::config::ServiceConfig>,
    ) -> Option<Self> {
        let config = config.effective();
        let Some(mode) = Mode::parse(&config.mode) else {
            warn!(mode = %config.mode, "unknown [system_one] mode; gate disabled (use off|shadow|enforce)");
            return None;
        };
        if mode == Mode::Off {
            return None;
        }
        // Progressive-rollout gate (AGENTS.md): experimental capabilities are
        // default-off behind LEINDEX_FEATURE_* so fleet rollout/rollback uses
        // the repo's standard mechanism, independent of user TOML.
        if !crate::feature_flags::FeatureFlag::SystemOne.is_enabled() {
            warn!(
                "system-one gate is experimental and disabled by feature flag; set LEINDEX_FEATURE_SYSTEM_ONE=1 to enable"
            );
            return None;
        }
        let Some(provider) = Provider::parse(&config.provider) else {
            warn!(provider = %config.provider, "unknown [system_one] provider; gate disabled (use laya|jev|openai)");
            return None;
        };
        let service = config.service.as_deref().and_then(|name| {
            match services.get(name) {
                Some(svc) => Some(svc),
                None => {
                    warn!(service = %name, "[system_one] service names no [services] section; ignoring");
                    None
                }
            }
        });
        let api_key = config
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|key| !key.is_empty());
        if provider.is_remote() && api_key.is_none() {
            warn!(
                "[system_one] remote provider needs api_key_env naming a set environment variable; gate disabled"
            );
            return None;
        }
        // base_url ladder: section (incl. preset) > referenced service > provider default.
        let base_url = config
            .base_url
            .clone()
            .or_else(|| service.map(|s| s.base_url()))
            .unwrap_or_else(|| default_base_url(provider).to_string());
        // model ladder: section > referenced service served name > provider default.
        let model = config
            .model
            .clone()
            .or_else(|| service.and_then(|s| s.served_name()))
            .unwrap_or_else(|| default_model(provider).to_string());
        let timeout_ms = config.timeout_ms.max(1);
        let asker = match HttpAsker::new(
            provider,
            &base_url,
            &model,
            api_key,
            Duration::from_millis(timeout_ms),
        ) {
            Ok(asker) => asker,
            Err(error) => {
                warn!(%error, "could not build the system-one HTTP client; gate disabled");
                return None;
            }
        };
        // Managed self-healing: only for service sections that allow it.
        let asker = match service {
            Some(svc) if svc.auto_start && svc.model.is_some() => {
                let name = config.service.clone().unwrap_or_default();
                asker.with_managed_service(name, svc.clone())
            }
            _ => asker,
        };
        Some(Self {
            mode,
            asker,
            pre: config.pre_gate,
            post: config.post_gate,
            heal: HealParams {
                tau_apply: config.tau_apply,
                timeout_ms,
                ..HealParams::default()
            },
            post_params: PostParams {
                relevant_min: config.relevant_min,
                instructions_max: config.instructions_max,
                top_n: config.top_n,
                ..PostParams::default()
            },
            timeout_ms,
        })
    }

    /// Pre-gate: validate and (when enforcing) repair the call arguments.
    pub async fn pre(
        &self,
        registry: &Arc<ProjectRegistry>,
        tool: &str,
        schema: &Value,
        args: Value,
        notes: &mut Notes,
    ) -> Value {
        if !self.pre {
            return args;
        }
        let started = std::time::Instant::now();
        let project_arg = args
            .get("project_path")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let unresolved = registry
            .resolve_project_root(project_arg.as_deref())
            .await
            .is_err();
        let known: Vec<String> = registry
            .loaded_projects()
            .await
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        let ctx = CallContext {
            tool,
            schema,
            read_only: is_read_only(tool),
            known_projects: &known,
            project_unresolved: unresolved,
        };
        let intent = args
            .get("task_context")
            .or_else(|| args.get("query"))
            .and_then(Value::as_str);
        let healed = heal(&self.asker, &ctx, args.clone(), intent, &self.heal).await;
        if healed.requests > 0 || !healed.changes.is_empty() || !healed.hints.is_empty() {
            info!(
                target: "leindex::system_one",
                tool,
                mode = ?self.mode,
                requests = healed.requests,
                elapsed_ms = started.elapsed().as_millis() as u64,
                changes = healed.changes.len(),
                hints = healed.hints.len(),
                "pre-gate"
            );
        }
        if self.mode != Mode::Enforce {
            return args;
        }
        notes.changes.extend(healed.changes);
        notes.hints.extend(healed.hints);
        healed.args
    }

    /// Post-gate: judge `leindex_search` results and, when enforcing, drop
    /// and re-order them. (`leindex_deep_analyze` is deliberately excluded —
    /// see the comment at the guard below.)
    pub async fn post(
        &self,
        registry: &Arc<ProjectRegistry>,
        tool: &str,
        args: &Value,
        value: &mut Value,
        notes: &mut Notes,
    ) {
        if !self.post || tool != "leindex_search" {
            // deep_analyze is deliberately NOT post-gated: its top-level
            // `context` blob is pre-rendered from the unfiltered entry set,
            // so filtering `results` would annotate "filtered" while the
            // blob still delivered every dropped entry verbatim. Rebuilding
            // the blob requires PDG re-expansion in the handler, which a
            // transport-level gate cannot do.
            return;
        }
        let Some(results) = value.get("results").and_then(Value::as_array) else {
            return;
        };
        if results.len() < 2 {
            return;
        }
        let started = std::time::Instant::now();
        let query = value
            .get("query")
            .or_else(|| args.get("query"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Root must resolve before any result text is sent to the provider:
        // `candidate_state` relativizes paths against it, and an empty root
        // would pass absolute host paths through ("nothing machine-specific
        // leaves the host"). Fail open instead — the post-gate is an
        // optimization, not a correctness requirement.
        let root = match registry
            .resolve_project_root(args.get("project_path").and_then(Value::as_str))
            .await
        {
            Ok(path) => path.display().to_string(),
            Err(_) => {
                debug!("system_one post-gate skipped: project root unresolved");
                return;
            }
        };
        let total = results.len();
        let base: Vec<f32> = results
            .iter()
            .enumerate()
            .map(|(index, result)| base_score(result, index, total))
            .collect();
        let judged: Vec<Value> = results
            .iter()
            .take(self.post_params.top_n)
            .cloned()
            .collect();
        let (verdicts, tokens) = judge_results(
            &self.asker,
            &query,
            &root,
            &judged,
            &self.post_params,
            self.timeout_ms,
        )
        .await;
        if !verdicts_complete(&verdicts) {
            // Partial answers are not actionable: a mix of answered and
            // unanswered verdicts would drop/reorder against an
            // incomparable signal set. Fail open for the whole list.
            return;
        }
        let decision = plan(&base, &verdicts, &self.post_params);
        let kept: Vec<usize> = (0..total)
            .filter(|i| !decision.dropped.iter().any(|(d, _)| d == i))
            .collect();
        let reordered = decision.order != kept;
        info!(
            target: "leindex::system_one",
            tool,
            mode = ?self.mode,
            judged = verdicts.len(),
            answered = verdicts.iter().flatten().count(),
            dropped = decision.dropped.len(),
            reordered,
            input_tokens = tokens,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "post-gate"
        );
        if self.mode != Mode::Enforce || (!reordered && decision.dropped.is_empty()) {
            return;
        }
        if let Some(Value::Array(array)) = value.get_mut("results") {
            apply_plan(array, &decision);
            let remaining = array.len();
            if let Some(count) = value.get_mut("count") {
                *count = json!(remaining);
            }
        }
        notes
            .dropped
            .extend(decision.dropped.iter().map(|(_, reason)| *reason));
        notes.reordered |= reordered;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cli::mcp::system_one::testing::{FnAsker, yes_answers};
    use crate::cli::mcp::system_one::types::SystemOneError;

    #[test]
    fn test_mode_parse_defaults_unknown_to_none() {
        assert_eq!(Mode::parse(""), Some(Mode::Off));
        assert_eq!(Mode::parse(" Shadow "), Some(Mode::Shadow));
        assert_eq!(Mode::parse("enforce"), Some(Mode::Enforce));
        assert_eq!(Mode::parse("on"), None);
    }

    #[test]
    fn test_unlisted_tools_are_treated_as_mutating() {
        assert!(is_read_only("leindex_search"));
        assert!(is_read_only("leindex_edit_preview"));
        assert!(is_read_only("leindex_phase_analysis"));
        assert!(is_read_only("phase_analysis"));
        // Structural search only reads and matches; listing it is what lets the
        // pre-gate apply deterministic repairs instead of hints-only.
        assert!(is_read_only("leindex_structural"));
        assert!(!is_read_only("leindex_edit_apply"));
        assert!(!is_read_only("leindex_write"));
        assert!(!is_read_only("leindex_rename_symbol"));
        assert!(!is_read_only("leindex_brand_new_tool"));
    }

    #[test]
    fn test_notes_attach_nothing_when_gate_did_nothing() {
        let mut value = json!({"results": []});
        Notes::default().attach(&mut value);
        assert!(value.get("_system_one").is_none());
    }

    #[test]
    fn test_notes_attach_reports_changes_hints_and_filtering() {
        let notes = Notes {
            changes: vec![Change {
                param: "top_k".into(),
                from: json!(500),
                to: json!(100),
                by: "code",
            }],
            hints: vec!["pass project_path".into()],
            dropped: vec!["irrelevant", "irrelevant"],
            reordered: true,
        };
        let mut value = json!({"results": []});
        notes.attach(&mut value);
        let note = &value["_system_one"];
        assert_eq!(note["healed"][0]["to"], 100);
        assert_eq!(note["hints"][0], "pass project_path");
        assert_eq!(note["filtered"]["count"], 2);
        assert_eq!(note["reordered"], true);
    }

    #[test]
    fn test_gate_annotation_survives_llm_payload_trimming() {
        // The design relies on `merge_meta` copying top-level `_` keys.
        let mut value = json!({"count": 1, "results": [{"file_path": "a.rs", "symbol_name": "f"}]});
        Notes {
            hints: vec!["h".into()],
            ..Notes::default()
        }
        .attach(&mut value);
        let trimmed = crate::cli::mcp::output::trim_llm_payload("leindex_search", &value);
        assert_eq!(trimmed["_system_one"]["hints"][0], "h");
    }

    #[tokio::test]
    async fn test_ask_bounded_times_out_and_fails_open() {
        struct Slow;
        impl Asker for Slow {
            async fn ask(&self, _: &Request) -> Result<Response, SystemOneError> {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(Response::default())
            }
        }
        let request = Request {
            state: json!({}),
            questions: vec![],
        };
        assert!(ask_bounded(&Slow, &request, 20).await.is_none());
        let ok = FnAsker(|_: &Request| Ok(yes_answers(&[("a", 0.5)])));
        assert_eq!(
            ask_bounded(&ok, &request, 500).await.unwrap().p_yes("a"),
            Some(0.5)
        );
    }

    #[test]
    fn test_from_config_is_off_by_default_and_rejects_bad_values() {
        let no_services = std::collections::BTreeMap::new();
        assert!(Gate::from_config(&SystemOneConfig::default(), &no_services).is_none());
        let remote_without_key = SystemOneConfig {
            mode: "shadow".into(),
            provider: "jev".into(),
            api_key_env: Some("LEINDEX_TEST_UNSET_KEY_VAR".into()),
            ..SystemOneConfig::default()
        };
        assert!(Gate::from_config(&remote_without_key, &no_services).is_none());
        let bad_provider = SystemOneConfig {
            mode: "shadow".into(),
            provider: "nope".into(),
            ..SystemOneConfig::default()
        };
        assert!(Gate::from_config(&bad_provider, &no_services).is_none());
        let _guard = crate::feature_flags::lock_flag_tests();
        crate::feature_flags::set_flag_override_for_test(
            crate::feature_flags::FeatureFlag::SystemOne,
            true,
        );
        let laya = SystemOneConfig {
            mode: "shadow".into(),
            provider: "laya".into(),
            ..SystemOneConfig::default()
        };
        assert!(Gate::from_config(&laya, &no_services).is_some());
        // The runtime rollout flag blocks even a fully valid config.
        crate::feature_flags::set_flag_override_for_test(
            crate::feature_flags::FeatureFlag::SystemOne,
            false,
        );
        assert!(Gate::from_config(&laya, &no_services).is_none());
    }

    #[test]
    fn test_from_config_derives_endpoint_from_service() {
        let _guard = crate::feature_flags::lock_flag_tests();
        crate::feature_flags::set_flag_override_for_test(
            crate::feature_flags::FeatureFlag::SystemOne,
            true,
        );
        let mut services = std::collections::BTreeMap::new();
        services.insert(
            "laya".to_string(),
            crate::config::ServiceConfig {
                host: "127.0.0.1".into(),
                port: 8181,
                model: Some("/models/laya-bf16.gguf".into()),
                auto_start: false,
                ..Default::default()
            },
        );
        // service reference derives base_url and model; gate builds.
        let cfg = SystemOneConfig {
            mode: "shadow".into(),
            provider: "laya".into(),
            service: Some("laya".into()),
            ..SystemOneConfig::default()
        };
        let gate = Gate::from_config(&cfg, &services).expect("gate from service-backed config");
        assert!(gate.asker.endpoint().starts_with("http://127.0.0.1:8181"));
    }
}
