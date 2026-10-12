//! Wire encoders/decoders. Formats were read from the vendors' own docs:
//!
//! * Jev / Laya: `POST /v1/systemone`, `{model, state, questions: {name: {...}}}`,
//!   answers `{answers: {name: {...}}, usage}`.
//! * OpenAI: `POST /v1/decisions`, `{model, input, questions: [{name, ...}]}`,
//!   answers `{answers: [{type, name, ...}]}`.

use super::types::{Answer, Provider, Question, Request, Response, SystemOneError};
use serde_json::{Map, Value, json};

/// Neutral keys for a Laya yes/no encoded as a two-option choice.
///
/// The Laya model card documents that its `noul` primitive can follow the
/// `false:`/`true:` labels instead of the state, and recommends a two-option
/// `choice` with neutral keys. Object keys serialize sorted, so option order
/// cannot be varied; instead the key that means "yes" alternates by question
/// name, so no fixed position is always "yes" (Jev documents a first-option bias).
pub fn laya_keys(name: &str) -> (&'static str, &'static str) {
    if name.bytes().map(u32::from).sum::<u32>() % 2 == 0 {
        ("A", "B")
    } else {
        ("B", "A")
    }
}

/// Encode `request` for `provider`.
pub fn encode(provider: Provider, model: &str, request: &Request) -> Value {
    match provider {
        Provider::OpenAi => encode_decisions(model, request),
        Provider::Laya | Provider::Jev => encode_systemone(provider, model, request),
    }
}

fn encode_systemone(provider: Provider, model: &str, request: &Request) -> Value {
    let mut questions = Map::new();
    for (name, question) in &request.questions {
        let body = match question {
            Question::YesNo { instructions } if provider == Provider::Laya => {
                let (yes, no) = laya_keys(name);
                let mut criteria = Map::new();
                criteria.insert(yes.to_string(), json!("yes"));
                criteria.insert(no.to_string(), json!("no"));
                json!({"type": "choice", "instructions": instructions, "criteria": criteria})
            }
            Question::YesNo { instructions } => {
                json!({"type": "noul", "instructions": instructions})
            }
            Question::Pick {
                instructions,
                options,
            } => {
                let criteria: Map<String, Value> = options
                    .iter()
                    .map(|(value, description)| (value.clone(), json!(description)))
                    .collect();
                json!({"type": "choice", "instructions": instructions, "criteria": criteria})
            }
        };
        questions.insert(name.clone(), body);
    }
    json!({"model": model, "state": request.state, "questions": questions})
}

fn encode_decisions(model: &str, request: &Request) -> Value {
    let questions: Vec<Value> = request
        .questions
        .iter()
        .map(|(name, question)| match question {
            Question::YesNo { instructions } => {
                json!({"type": "predicate", "name": name, "instructions": instructions})
            }
            Question::Pick {
                instructions,
                options,
            } => {
                let choices: Vec<Value> = options
                    .iter()
                    .map(|(value, description)| json!({"value": value, "description": description}))
                    .collect();
                json!({"type": "choice", "name": name, "instructions": instructions, "choices": choices})
            }
        })
        .collect();
    // `input` accepts a plain string; structured state is sent as compact JSON.
    let input = match &request.state {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    json!({"model": model, "input": input, "questions": questions})
}

/// Decode a provider response body. Unanswerable questions are omitted, never
/// guessed; a structurally wrong body is an error.
pub fn decode(
    provider: Provider,
    request: &Request,
    body: &Value,
) -> Result<Response, SystemOneError> {
    let mut response = match provider {
        Provider::OpenAi => decode_decisions(request, body)?,
        Provider::Laya | Provider::Jev => decode_systemone(provider, request, body)?,
    };
    response.input_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64);
    Ok(response)
}

fn probability(value: &Value) -> Option<f32> {
    value
        .as_f64()
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .map(|p| p as f32)
}

/// Build a pick answer; `choice` must be one of the offered options.
fn pick_answer(
    options: &[(String, String)],
    choice: Option<&str>,
    probabilities: Vec<(String, f32)>,
    confidence: Option<f32>,
) -> Option<Answer> {
    let choice = choice?;
    if !options.iter().any(|(value, _)| value == choice) {
        return None;
    }
    let top = probabilities
        .iter()
        .map(|(_, p)| *p)
        .fold(0.0_f32, f32::max);
    Some(Answer::Pick {
        choice: choice.to_string(),
        probabilities,
        confidence: confidence.unwrap_or(top),
    })
}

fn decode_systemone(
    provider: Provider,
    request: &Request,
    body: &Value,
) -> Result<Response, SystemOneError> {
    let answers = body
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| SystemOneError::Decode("missing `answers` object".into()))?;
    let mut out = Response::default();
    for (name, question) in &request.questions {
        let Some(answer) = answers.get(name) else {
            continue;
        };
        let decoded = match question {
            Question::YesNo { .. } if provider == Provider::Laya => {
                let (yes, _) = laya_keys(name);
                answer
                    .get("probabilities")
                    .and_then(|p| p.get(yes))
                    .and_then(probability)
            }
            Question::YesNo { .. } => answer.get("noul").and_then(probability),
            Question::Pick { .. } => None,
        }
        .map(|p_yes| Answer::YesNo { p_yes });
        let decoded = match question {
            Question::YesNo { .. } => decoded,
            Question::Pick { options, .. } => {
                let probabilities = answer
                    .get("probabilities")
                    .and_then(Value::as_object)
                    .map(|map| {
                        map.iter()
                            .filter_map(|(k, v)| probability(v).map(|p| (k.clone(), p)))
                            .collect()
                    })
                    .unwrap_or_default();
                pick_answer(
                    options,
                    answer.get("choice").and_then(Value::as_str),
                    probabilities,
                    answer.get("confidence").and_then(probability),
                )
            }
        };
        if let Some(decoded) = decoded {
            out.answers.insert(name.clone(), decoded);
        }
    }
    Ok(out)
}

fn decode_decisions(request: &Request, body: &Value) -> Result<Response, SystemOneError> {
    let answers = body
        .get("answers")
        .and_then(Value::as_array)
        .ok_or_else(|| SystemOneError::Decode("missing `answers` array".into()))?;
    let mut out = Response::default();
    for (name, question) in &request.questions {
        // A `refusal` answer has a different `type`, so it falls out here.
        let Some(answer) = answers
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str) == Some(name.as_str()))
        else {
            continue;
        };
        let kind = answer.get("type").and_then(Value::as_str);
        let decoded = match (question, kind) {
            (Question::YesNo { .. }, Some("predicate")) => answer
                .get("probability")
                .and_then(probability)
                .map(|p_yes| Answer::YesNo { p_yes }),
            (Question::Pick { options, .. }, Some("choice")) => {
                let probabilities = answer
                    .get("probabilities")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|row| {
                                let value = row.get("value").and_then(Value::as_str)?;
                                let p = row.get("probability").and_then(probability)?;
                                Some((value.to_string(), p))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                pick_answer(
                    options,
                    answer.get("choice").and_then(Value::as_str),
                    probabilities,
                    answer.get("confidence").and_then(probability),
                )
            }
            _ => None,
        };
        if let Some(decoded) = decoded {
            out.answers.insert(name.clone(), decoded);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Request {
        Request {
            state: json!({"query": "q"}),
            questions: vec![
                (
                    "ok".into(),
                    Question::YesNo {
                        instructions: "Is it ok?".into(),
                    },
                ),
                (
                    "dept".into(),
                    Question::Pick {
                        instructions: "Which?".into(),
                        options: vec![
                            ("billing".into(), "payments".into()),
                            ("other".into(), "else".into()),
                        ],
                    },
                ),
            ],
        }
    }

    #[test]
    fn jev_encodes_noul_and_choice_criteria_map() {
        let body = encode(Provider::Jev, "jev-latest", &sample());
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["ok"]["type"], "noul");
        assert_eq!(body["questions"]["dept"]["type"], "choice");
        assert_eq!(body["questions"]["dept"]["criteria"]["billing"], "payments");
    }

    #[test]
    fn laya_encodes_yes_no_as_neutral_two_option_choice() {
        let body = encode(Provider::Laya, "laya", &sample());
        let q = &body["questions"]["ok"];
        assert_eq!(q["type"], "choice");
        let (yes, no) = laya_keys("ok");
        assert_eq!(q["criteria"][yes], "yes");
        assert_eq!(q["criteria"][no], "no");
    }

    #[test]
    fn laya_yes_key_varies_with_question_name() {
        let keys: std::collections::HashSet<_> = ["a", "b", "ab", "wrong:search_mode"]
            .iter()
            .map(|n| laya_keys(n).0)
            .collect();
        assert_eq!(keys.len(), 2, "yes must not always be the same letter");
    }

    #[test]
    fn openai_encodes_predicate_array_with_names() {
        let body = encode(Provider::OpenAi, "gpt-6-luna", &sample());
        assert_eq!(body["model"], "gpt-6-luna");
        assert!(body["input"].is_string());
        assert_eq!(body["questions"][0]["type"], "predicate");
        assert_eq!(body["questions"][0]["name"], "ok");
        assert_eq!(body["questions"][1]["choices"][0]["value"], "billing");
    }

    #[test]
    fn jev_decodes_documented_response() {
        let body = json!({
            "model": "jev-1.13.0",
            "answers": {
                "ok": {"type": "noul", "noul": 0.91},
                "dept": {"type": "choice", "choice": "billing",
                         "probabilities": {"billing": 0.85, "other": 0.15}, "confidence": 0.78}
            },
            "usage": {"input_tokens": 412, "output_tokens": 67}
        });
        let r = decode(Provider::Jev, &sample(), &body).unwrap();
        assert_eq!(r.p_yes("ok"), Some(0.91));
        assert_eq!(r.pick("dept"), Some(("billing", 0.78)));
        assert_eq!(r.input_tokens, Some(412));
    }

    #[test]
    fn laya_decodes_yes_from_the_yes_key() {
        let (yes, no) = laya_keys("ok");
        let mut probs = Map::new();
        probs.insert(yes.into(), json!(0.8));
        probs.insert(no.into(), json!(0.2));
        let body =
            json!({"answers": {"ok": {"type": "choice", "choice": yes, "probabilities": probs}}});
        let r = decode(Provider::Laya, &sample(), &body).unwrap();
        assert_eq!(r.p_yes("ok"), Some(0.8));
    }

    #[test]
    fn openai_decodes_documented_response_and_skips_refusal() {
        let body = json!({"answers": [
            {"type": "refusal", "name": "ok"},
            {"type": "choice", "name": "dept", "choice": "billing",
             "probabilities": [{"value": "billing", "probability": 0.95},
                               {"value": "other", "probability": 0.05}],
             "confidence": 0.93}
        ]});
        let r = decode(Provider::OpenAi, &sample(), &body).unwrap();
        assert_eq!(r.p_yes("ok"), None, "refusal must yield no answer");
        assert_eq!(r.pick("dept"), Some(("billing", 0.93)));
    }

    #[test]
    fn rejects_choice_outside_offered_options_and_bad_probabilities() {
        let body = json!({"answers": {
            "ok": {"type": "noul", "noul": 1.7},
            "dept": {"type": "choice", "choice": "invented", "probabilities": {}}
        }});
        let r = decode(Provider::Jev, &sample(), &body).unwrap();
        assert!(r.answers.is_empty());
    }

    #[test]
    fn structurally_wrong_body_is_an_error() {
        assert!(decode(Provider::Jev, &sample(), &json!({"nope": 1})).is_err());
        assert!(decode(Provider::OpenAi, &sample(), &json!({"answers": {}})).is_err());
    }
}
