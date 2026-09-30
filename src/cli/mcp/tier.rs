// Response detail tiers.
//
// Every router accepts `tier`:
//   l0  identity card — a few lines: what ran, the headline numbers, the top hits
//   l1  bounded overview (default) — the rendered, trimmed payload models see today
//   l2  full detail — the complete, untrimmed handler result as JSON
//
// The tier is a transport concern: handlers are unaware of it, so every branch
// supports every tier and the tiers cannot drift from the handlers' output.

use super::protocol::JsonRpcError;
use serde_json::{Map, Value};

/// A response detail level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    /// Identity card.
    L0,
    /// Bounded overview (default).
    #[default]
    L1,
    /// Full detail.
    L2,
}

impl Tier {
    /// Remove `tier` from `args` and parse it. Absent means the default (`l1`).
    /// Accepts `l0|l1|l2` case-insensitively, plus `0|1|2`.
    pub fn take_from(args: &mut Value) -> Result<Self, JsonRpcError> {
        let Some(raw) = args
            .as_object_mut()
            .and_then(|object| object.remove("tier"))
        else {
            return Ok(Tier::default());
        };
        let text = match &raw {
            Value::String(text) => text.trim().to_ascii_lowercase(),
            Value::Number(number) => format!("l{number}"),
            Value::Null => return Ok(Tier::default()),
            _ => String::new(),
        };
        match text.as_str() {
            "l0" => Ok(Tier::L0),
            "l1" => Ok(Tier::L1),
            "l2" => Ok(Tier::L2),
            _ => Err(JsonRpcError::invalid_params_with_suggestion(
                format!("Invalid tier {raw}"),
                "Use tier: \"l0\" (identity card), \"l1\" (bounded overview, default) or \"l2\" (full detail)"
                    .to_string(),
            )),
        }
    }
}

const NAME_KEYS: [&str; 8] = [
    "name",
    "symbol",
    "symbol_name",
    "qualified_name",
    "file",
    "file_path",
    "path",
    "id",
];
const MAX_SCALAR_CHARS: usize = 100;
const MAX_HEADLINES: usize = 8;
const MAX_TOP: usize = 5;

fn short(text: &str) -> String {
    if text.chars().count() <= MAX_SCALAR_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_SCALAR_CHARS - 1).collect();
    format!("{cut}…")
}

fn label(item: &Value) -> Option<String> {
    let object = item.as_object()?;
    let name = NAME_KEYS
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str));
    let name = name?;
    // Pair a symbol with where it lives when both are present.
    let place = ["file_path", "file", "path"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .filter(|place| *place != name);
    Some(match place {
        Some(place) => format!("{} ({})", short(name), short(place)),
        None => short(name),
    })
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(short(text)),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Collect headline scalars and array sizes from `object`, one level deep
/// (plus a `summary`/`stats` sub-object, where handlers put their totals).
fn headlines(object: &Map<String, Value>, prefix: &str, out: &mut Vec<String>) {
    // Totals and paging state first, so a cap never drops the headline numbers.
    let priority = |key: &str| {
        !(key.starts_with("total") || key == "count" || key == "returned" || key == "has_more")
    };
    let mut entries: Vec<(&String, &Value)> = object.iter().collect();
    entries.sort_by_key(|(key, _)| priority(key));
    for (key, value) in entries {
        if out.len() >= MAX_HEADLINES {
            return;
        }
        if key.starts_with('_') || key == "content" || key == "patch" || key == "source" {
            continue;
        }
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Array(items) => out.push(format!("{name}={}", items.len())),
            Value::Object(inner)
                if prefix.is_empty()
                    && matches!(
                        key.as_str(),
                        "summary" | "stats" | "impact_summary" | "retrieval"
                    ) =>
            {
                headlines(inner, key, out);
            }
            other => {
                if let Some(text) = scalar(other) {
                    out.push(format!("{name}={text}"));
                }
            }
        }
    }
}

/// Render the identity card for a tool result.
pub fn identity_card(tool: &str, value: &Value) -> String {
    let mut lines = Vec::new();
    let mut facts = Vec::new();
    if let Some(object) = value.as_object() {
        headlines(object, "", &mut facts);
        // First array of objects that carry a name/path: the "top hits".
        let top = object.values().find_map(|candidate| {
            let items = candidate.as_array()?;
            let labels: Vec<String> = items.iter().filter_map(label).take(MAX_TOP).collect();
            (!labels.is_empty()).then_some(labels)
        });
        lines.push(format!(
            "{tool} · {}",
            if facts.is_empty() {
                "ok".to_string()
            } else {
                facts.join(" ")
            }
        ));
        if let Some(top) = top {
            lines.push(format!("top: {}", top.join(", ")));
        }
    } else if let Some(items) = value.as_array() {
        lines.push(format!("{tool} · items={}", items.len()));
        let labels: Vec<String> = items.iter().filter_map(label).take(MAX_TOP).collect();
        if !labels.is_empty() {
            lines.push(format!("top: {}", labels.join(", ")));
        }
    } else {
        lines.push(format!(
            "{tool} · {}",
            scalar(value).unwrap_or_else(|| "ok".to_string())
        ));
    }
    lines.push(
        "(tier l0 — pass tier=\"l1\" for the overview or \"l2\" for full detail)".to_string(),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tier_defaults_to_l1_and_is_consumed() {
        let mut args = json!({"query": "x"});
        assert_eq!(Tier::take_from(&mut args).unwrap(), Tier::L1);
        let mut args = json!({"query": "x", "tier": "L2"});
        assert_eq!(Tier::take_from(&mut args).unwrap(), Tier::L2);
        assert_eq!(args, json!({"query": "x"}));
        let mut args = json!({"tier": 0});
        assert_eq!(Tier::take_from(&mut args).unwrap(), Tier::L0);
    }

    #[test]
    fn test_invalid_tier_is_rejected_with_guidance() {
        let mut args = json!({"tier": "l9"});
        let error = Tier::take_from(&mut args).unwrap_err();
        assert!(error.message_with_hint().contains("l0"), "{error}");
    }

    #[test]
    fn test_identity_card_has_headlines_and_top_hits() {
        let value = json!({
            "count": 3,
            "has_more": true,
            "results": [
                {"symbol_name": "alpha", "file_path": "src/a.rs", "score": {"overall": 0.9}},
                {"symbol_name": "beta", "file_path": "src/b.rs"},
            ],
            "_meta": {"noise": 1},
        });
        let card = identity_card("leindex_search", &value);
        assert!(card.contains("count=3"), "{card}");
        assert!(card.contains("results=2"), "{card}");
        assert!(card.contains("alpha (src/a.rs), beta (src/b.rs)"), "{card}");
        assert!(!card.contains('?'), "no placeholder glyphs: {card}");
        assert!(!card.contains("noise"), "{card}");
        assert!(card.lines().count() <= 4);
    }

    #[test]
    fn test_identity_card_leads_with_totals_even_when_keys_sort_late() {
        let value = json!({
            "a1": 1, "a2": 2, "a3": 3, "a4": 4, "a5": 5, "a6": 6, "a7": 7, "a8": 8, "a9": 9,
            "total_matches": 42, "has_more": true,
        });
        let card = identity_card("t", &value);
        assert!(
            card.contains("total_matches=42") && card.contains("has_more=true"),
            "{card}"
        );
    }

    #[test]
    fn test_identity_card_is_small_for_huge_results() {
        let results: Vec<Value> = (0..5_000)
            .map(|i| json!({"name": format!("symbol_{i}"), "file": "f.rs"}))
            .collect();
        let card = identity_card("t", &json!({"results": results, "total": 5000}));
        assert!(card.len() < 500, "{} bytes", card.len());
    }
}
