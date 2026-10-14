use super::PathScrub;
use crate::cli::mcp::system_one::types::{Answer, Question, Request, Response};
use serde_json::{Value, json};
use std::collections::HashMap;

fn pick_request(state: Value, options: Vec<(&str, &str)>) -> Request {
    Request {
        state,
        questions: vec![(
            "dept".into(),
            Question::Pick {
                instructions: "Which?".into(),
                options: options
                    .into_iter()
                    .map(|(v, d)| (v.to_string(), d.to_string()))
                    .collect(),
            },
        )],
    }
}

fn option_values(request: &Request) -> Vec<String> {
    match &request.questions[0].1 {
        Question::Pick { options, .. } => options.iter().map(|(v, _)| v.clone()).collect(),
        Question::YesNo { .. } => Vec::new(),
    }
}

#[test]
fn test_scrub_removes_unix_windows_home_and_outside_home_paths() {
    let request = pick_request(
        json!({
            "unix": "/home/alice/proj/src/a.rs",
            "win": "C:\\Users\\bob\\app\\b.rs",
            "home": "~/code/c.rs",
            "outside": "/srv/data/d.rs"
        }),
        vec![("keep", "fine")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let text = out.state.to_string();
    for leak in ["alice", "bob", "C:\\", "~/code", "/srv/data", "/home/"] {
        assert!(!text.contains(leak), "leaked {leak}: {text}");
    }
}

#[test]
fn test_scrub_gives_same_path_same_alias_and_same_basename_distinct_aliases() {
    let request = pick_request(
        json!({
            "a": "/work/one/app",
            "b": "/work/one/app",
            "c": "/work/two/app"
        }),
        vec![("keep", "fine")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let a = out.state["a"].as_str().unwrap();
    let b = out.state["b"].as_str().unwrap();
    let c = out.state["c"].as_str().unwrap();
    assert_eq!(a, b, "same path must keep one alias");
    assert_ne!(
        a, c,
        "same basename in different projects must stay distinct"
    );
}

#[test]
fn test_scrub_rewrites_pick_values_and_descriptions() {
    let request = pick_request(
        json!({}),
        vec![("/home/alice/proj", "use /home/alice/proj for search")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let Question::Pick { options, .. } = &out.questions[0].1 else {
        panic!("pick expected");
    };
    assert!(
        !options[0].0.contains("alice"),
        "value leaked: {:?}",
        options
    );
    assert!(
        !options[0].1.contains("alice"),
        "description leaked: {:?}",
        options
    );
    assert!(
        options[0].1.starts_with("use "),
        "prose lost: {:?}",
        options
    );
}

#[test]
fn test_scrub_keeps_prose_around_a_path() {
    let request = pick_request(json!({"t": "see /home/a/x.rs now"}), vec![("k", "d")]);
    let out = PathScrub::new(&request).scrub_request(&request);
    let text = out.state["t"].as_str().unwrap();
    assert!(text.starts_with("see ") && text.ends_with(" now"), "{text}");
    assert!(!text.contains("/home"), "{text}");
}

#[test]
fn test_scrub_ignores_urls_and_project_relative_paths() {
    let request = pick_request(
        json!({"u": "https://example.com/a/b", "r": "src/lib/mod.rs"}),
        vec![("k", "d")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    assert_eq!(out.state["u"], "https://example.com/a/b");
    assert_eq!(out.state["r"], "src/lib/mod.rs");
}

#[test]
fn test_unscrub_restores_an_issued_pick_choice() {
    let request = pick_request(json!({}), vec![("/home/alice/proj", "d1"), ("keep", "d2")]);
    let mut scrub = PathScrub::new(&request);
    let out = scrub.scrub_request(&request);
    let alias = option_values(&out)[0].clone();
    let response = Response {
        answers: HashMap::from([(
            "dept".to_string(),
            Answer::Pick {
                choice: alias.clone(),
                probabilities: vec![(alias, 0.9), ("keep".to_string(), 0.1)],
                confidence: 0.8,
            },
        )]),
        input_tokens: None,
    };
    let restored = scrub.unscrub_response(response);
    assert_eq!(restored.pick("dept"), Some(("/home/alice/proj", 0.8)));
}

#[test]
fn test_unscrub_leaves_unknown_and_non_alias_choices_untouched() {
    let request = pick_request(json!({}), vec![("/home/alice/proj", "d1"), ("keep", "d2")]);
    let mut scrub = PathScrub::new(&request);
    let _ = scrub.scrub_request(&request);
    for choice in ["invented", "keep", "pathref-99"] {
        let response = Response {
            answers: HashMap::from([(
                "dept".to_string(),
                Answer::Pick {
                    choice: choice.to_string(),
                    probabilities: vec![],
                    confidence: 0.5,
                },
            )]),
            input_tokens: None,
        };
        assert_eq!(
            scrub.unscrub_response(response).pick("dept"),
            Some((choice, 0.5))
        );
    }
}

#[test]
fn test_alias_prefix_never_collides_with_input_text() {
    let request = pick_request(
        json!({"note": "pathref-1 is literal text", "p": "/home/alice/x"}),
        vec![("k", "d")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let alias = out.state["p"].as_str().unwrap();
    assert!(!alias.starts_with("pathref-1"), "alias collides: {alias}");
    assert!(alias.contains("pathrefx"), "prefix not extended: {alias}");
}

#[test]
fn test_scrub_covers_quoted_unix_path_with_spaces() {
    let request = pick_request(
        json!({"q": "open \"/home/Alice Smith/private\" now"}),
        vec![("keep", "fine")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let text = out.state["q"].as_str().unwrap();
    assert!(!text.contains("Alice") && !text.contains("Smith"), "{text}");
    assert!(
        text.starts_with("open ") && text.ends_with(" now"),
        "{text}"
    );
}

#[test]
fn test_scrub_covers_quoted_windows_path_with_spaces() {
    let request = pick_request(
        json!({"q": "open \"C:\\Users\\Bob Jones\\app\" now"}),
        vec![("keep", "fine")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let text = out.state["q"].as_str().unwrap();
    assert!(!text.contains("Bob") && !text.contains("Jones"), "{text}");
}

#[test]
fn test_scrub_covers_unc_paths() {
    let request = pick_request(
        json!({"u": "\\\\fileserver\\share\\proj\\a.rs", "v": "//fileserver/share/b.rs"}),
        vec![("keep", "fine")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let text = out.state.to_string();
    assert!(
        !text.contains("fileserver") && !text.contains("share"),
        "{text}"
    );
}

#[test]
fn test_scrub_aliases_whole_path_values_and_keys_with_spaces() {
    let request = pick_request(
        json!({"/home/Carol Ann/x": "/home/Alice Smith/private"}),
        vec![("/home/Alice Smith/proj", "d")],
    );
    let out = PathScrub::new(&request).scrub_request(&request);
    let text = format!("{} {:?}", out.state, option_values(&out));
    assert!(!text.contains("Carol") && !text.contains("Smith"), "{text}");
}

#[test]
fn test_scrub_aliases_bare_root_value() {
    let request = pick_request(json!({"r": "/"}), vec![("keep", "fine")]);
    let out = PathScrub::new(&request).scrub_request(&request);
    assert_ne!(out.state["r"], "/");
}

#[test]
fn test_scrub_leaves_bare_slash_in_prose_alone() {
    let request = pick_request(json!({"t": "a / b"}), vec![("keep", "fine")]);
    let out = PathScrub::new(&request).scrub_request(&request);
    assert_eq!(out.state["t"], "a / b");
}
