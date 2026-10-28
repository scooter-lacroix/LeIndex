//! Unit tests for `remote`, moved out of the parent file by pure code motion
//! (Large File Detection gate). Declared as `mod tests` there, so `super::*`
//! still reaches every private item.

use super::*;
// Request shaping now lives in `remote_budget`; it is reached through the
// parent's `super::*` only for the three names `remote.rs` itself re-exports,
// so the rest are imported at their new home.
use super::remote_budget::{estimate_tokens, request_width_for, truncate_to_token_budget};

#[test]
fn test_remote_provider_default_dimension() {
    let provider = RemoteProvider::OpenAI {
        model: "text-embedding-3-small".to_string(),
    };
    assert_eq!(provider.default_dimension(), 1536);
}

/// Connection reuse is a per-endpoint-class decision, not a global one.
///
/// A remote endpoint pays a TLS handshake per batch when the pool is disabled:
/// negligible against localhost, pure cross-internet latency against a hosted
/// endpoint — once per batch, so ~59 times for a 7,495-row project. This pins
/// BOTH halves, because the local stale-socket rule must never silently become
/// the rule for hosted endpoints again.
#[test]
fn test_pool_reuse_is_disabled_locally_and_enabled_remotely() {
    let mut local = RemoteEmbeddingConfig::openai("sk-not-used".to_string(), None);
    local.provider = RemoteProvider::Custom {
        endpoint: "http://127.0.0.1:8080/v1/embeddings".to_string(),
    };
    assert!(
        local.is_local_endpoint(),
        "127.0.0.1 must classify as local"
    );
    assert_eq!(
        pool_idle_timeout_for(&local),
        std::time::Duration::ZERO,
        "a local endpoint must not reuse idle sockets: one stale-socket reuse "
    );
    assert_eq!(
        pool_max_idle_per_host_for(&local),
        0,
        "no idle socket may be retained for a local endpoint"
    );

    let mut remote = RemoteEmbeddingConfig::openai("sk-not-used".to_string(), None);
    remote.provider = RemoteProvider::Custom {
        endpoint: "https://api.example.com/v1/embeddings".to_string(),
    };
    assert!(!remote.is_local_endpoint(), "a public host is not local");
    assert!(
        pool_idle_timeout_for(&remote) > std::time::Duration::ZERO,
        "a remote endpoint that never reuses a socket pays a TLS handshake per batch"
    );
    assert!(
        pool_max_idle_per_host_for(&remote) > 0,
        "a remote endpoint needs at least one retainable idle socket"
    );
}

#[test]
fn test_template_fingerprint_covers_substitution_semantics() {
    // A template-string-only hash is not enough: `apply_doc_template`
    // started replacing `{title}` without the template changing, so every
    // installation holding a `{title}` template kept its old vectors in
    // the current namespace while new rows were embedded from different
    // input — two vector spaces under one identity. Guard the property:
    // the fingerprint must not be reproducible from the template text
    // alone (the pre-fix formula), which is what folding in
    // TEMPLATE_SEMANTICS_VERSION buys.
    let doc = "title: {title} | text: {text}";
    let mut lexical: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [doc, "\u{0}", ""] {
        for byte in part.as_bytes() {
            lexical ^= u64::from(*byte);
            lexical = lexical.wrapping_mul(0x0010_0000_01b3);
        }
    }
    assert_ne!(
        template_fingerprint(Some(doc), None),
        Some(format!("tpl{lexical:016x}")),
        "the fingerprint must vary with substitution semantics, not only the text"
    );
}

#[test]
fn test_template_fingerprint_cannot_collide_across_part_boundaries() {
    // `("a\0", "b")` and `("a", "\0b")` are different template pairs — the
    // document template ends in a NUL vs the query template begins with one
    // — but a bare-separator join is not injective: both encode to the same
    // byte stream `a\0\0b`, so they shared a fingerprint and therefore a
    // cache namespace. Length-prefixing each part makes the encoding
    // injective, whatever bytes the templates contain.
    let left = template_fingerprint(Some("a\u{0}"), Some("b"));
    let right = template_fingerprint(Some("a"), Some("\u{0}b"));
    assert!(left.is_some() && right.is_some());
    assert_ne!(
        left, right,
        "templates that differ only in where a separator falls must not share a fingerprint"
    );
}

#[test]
fn test_remote_identity_names_model_and_never_leaks_credentials() {
    let secret = "sk-super-secret-value";
    let config = RemoteEmbeddingConfig::openai(secret.to_string(), None);
    let provider = GenericRemoteProvider::from_config(config).unwrap();
    let identity = provider.identity();
    assert!(identity.starts_with("remote|openai|text-embedding-3-small|"));
    assert!(identity.ends_with(&format!("|{}", provider.dimension())));
    assert!(!identity.contains(secret));

    let other = GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: RemoteProvider::OpenAI {
            model: "text-embedding-3-large".to_string(),
        },
        api_key: Some(secret.to_string()),
        ..Default::default()
    })
    .unwrap();
    assert_ne!(identity, other.identity());
}

#[test]
fn test_custom_remote_identity_includes_model_without_changing_legacy_identity() {
    let endpoint = "http://127.0.0.1:8080/v1/embeddings".to_string();
    let base_url = Some("http://127.0.0.1:8080/v1".to_string());
    let config = |model: Option<&str>| RemoteEmbeddingConfig {
        provider: RemoteProvider::Custom {
            endpoint: endpoint.clone(),
        },
        base_url: base_url.clone(),
        model: model.map(str::to_string),
        remote_dimension: Some(768),
        ..Default::default()
    };

    let model_a = GenericRemoteProvider::from_config(config(Some("model-a"))).unwrap();
    let model_b = GenericRemoteProvider::from_config(config(Some("model-b"))).unwrap();
    assert_ne!(model_a.identity(), model_b.identity());
    assert!(model_a.identity().contains("|mmodel-a|"));

    let legacy = GenericRemoteProvider::from_config(config(None)).unwrap();
    assert_eq!(
        legacy.identity(),
        format!(
            "remote|custom|{endpoint}|{}|768",
            base_url.as_deref().unwrap()
        )
    );
}

#[test]
fn test_remote_config_openai() {
    let config = RemoteEmbeddingConfig::openai("test-key".to_string(), None);
    assert!(config.api_key.is_some());
}

#[test]
fn test_reorder_embeddings_identity() {
    let entries = vec![(0, vec![1.0]), (1, vec![2.0]), (2, vec![3.0])];
    let result = reorder_embeddings_by_index(entries, 3).unwrap();
    assert_eq!(result, vec![vec![1.0], vec![2.0], vec![3.0]]);
}

#[test]
fn test_reorder_embeddings_shuffled() {
    let entries = vec![(2, vec![3.0]), (0, vec![1.0]), (1, vec![2.0])];
    let result = reorder_embeddings_by_index(entries, 3).unwrap();
    assert_eq!(result, vec![vec![1.0], vec![2.0], vec![3.0]]);
}

#[test]
fn test_reorder_embeddings_duplicate_index() {
    let entries = vec![(0, vec![1.0]), (0, vec![2.0])];
    let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
    assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
    assert!(err.to_string().contains("duplicate"));
}

#[test]
fn test_reorder_embeddings_gap_missing_index() {
    // Length 2, but indices are 0 and 2 (missing 1)
    let entries = vec![(0, vec![1.0]), (2, vec![3.0])];
    let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
    assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
}

#[test]
fn test_reorder_embeddings_out_of_range_extra() {
    let entries = vec![(0, vec![1.0]), (3, vec![4.0])];
    let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
    assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
    assert!(err.to_string().contains("out of range"));
}

#[test]
fn test_reorder_embeddings_length_mismatch() {
    let entries = vec![(0, vec![1.0])];
    let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
    assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
    assert!(err.to_string().contains("expected 2"));
}

/// A request's width must respect the endpoint's TOKEN budget, not just its
/// document count.
///
/// Regression: hosted EmbeddingGemma answers a request over 2,048 tokens with
/// `The prompt is too long for embeddinggemma-300m on the routes that remain.
/// Shorten it and retry.` even when the document count is far under
/// `batch_limit`. Sizing by documents alone sent 32 x 882 B (~7k tokens) and
/// every batch was rejected.
#[test]
fn test_request_width_packs_against_the_token_budget() {
    // ~7 tokens each (20 bytes / 3, floored at 1).
    let row = std::borrow::Cow::Borrowed("01234567890123456789");
    let pending: Vec<_> = (0..32).map(|_| row.clone()).collect();

    // 2048 budget / 7 tokens-per-row => 292 rows fit, so the DOCUMENT cap binds.
    assert_eq!(
        request_width_for(&pending, Some(32), Some(2048), false),
        32,
        "a generous token budget must not narrow the document cap"
    );

    // A 100-token budget fits 14 of these rows (14 * 7 = 98, adding a 15th
    // would reach 105 and overflow).
    assert_eq!(
        request_width_for(&pending, Some(32), Some(100), false),
        14,
        "the token budget must narrow the request below the document cap"
    );

    // Without a budget the document cap still applies unchanged.
    assert_eq!(request_width_for(&pending, Some(32), None, false), 32);
}

/// A single oversized document must still be sent rather than dropped.
///
/// When one input alone exceeds the budget there is no smaller legal request:
/// width 1 is the floor, and the shrink path is what makes it fit. Returning 0
/// here would stall the batching loop or silently lose the row.
#[test]
fn test_request_width_never_returns_zero_for_an_oversized_input() {
    let huge = std::borrow::Cow::Borrowed(&"x".repeat(9_000)[..]);
    let pending = vec![huge];
    assert_eq!(request_width_for(&pending, Some(32), Some(2048), false), 1);
}

/// Local endpoints with no configured limits keep the whole batch in one
/// request. This is the byte-identical safety net for the managed llama-server
/// the concurrency work deliberately leaves untouched.
#[test]
fn test_local_without_limits_keeps_one_request() {
    let row = std::borrow::Cow::Borrowed("01234567890123456789");
    let pending: Vec<_> = (0..200).map(|_| row.clone()).collect();
    assert_eq!(
        request_width_for(&pending, None, None, true),
        200,
        "local endpoints keep today's single-request behaviour"
    );
}

/// A 400 whose message names a length problem is a SIZE verdict, so the shrink
/// path runs instead of the row being written off as a content rejection.
#[test]
fn test_400_too_long_message_is_a_size_rejection() {
    assert!(is_oversize_rejection(
        400,
        "The prompt is too long for embeddinggemma-300m on the routes that remain. Shorten it and retry."
    ));
    assert!(is_oversize_rejection(
        400,
        "This model's maximum context length is 8192 tokens"
    ));
    // llama-server's 500 dialect must keep working.
    assert!(is_oversize_rejection(
        500,
        "input (5000 tokens) is too large to process. increase the physical batch size"
    ));
    // A genuine content rejection must NOT be treated as size.
    assert!(!is_oversize_rejection(400, "invalid model identifier"));
}

/// `is_too_long_message` owns the phrase table that decides whether a rejection
/// is a SIZE verdict. The test above proves three of its four dialects are
/// reachable, but only through `is_oversize_rejection`; what nothing pinned
/// before is:
///
/// * the Azure `reduce the length of the messages` dialect, which no test
///   exercised anywhere;
/// * case folding — every caller above feeds correctly cased input, so
///   deleting the `to_ascii_lowercase()` call would have broken no test while
///   silently un-shrinking every host that capitalizes;
/// * the negatives at the classifier layer, so the phrase table cannot drift
///   into matching a 401 or a rate-limit body and trigger a useless
///   shrink-and-resend loop.
#[test]
fn test_is_too_long_message_classifies_every_dialect_and_nothing_else() {
    for message in [
        // llama-server (HTTP 500), verbatim shape.
        "input (21846 tokens) is too large to process. increase the physical batch size",
        // Hosted OpenAI-compatible proxy.
        "The prompt is too long for embeddinggemma-300m on the routes that remain. Shorten it and retry.",
        // OpenAI itself.
        "This model's maximum context length is 8192 tokens",
        // Azure-style: the one dialect that had no coverage.
        "Please reduce the length of the messages",
        // Case must not matter: hosts capitalize inconsistently.
        "INPUT IS TOO LARGE TO PROCESS",
        "PROMPT IS TOO LONG",
    ] {
        assert!(
            is_too_long_message(message),
            "must recognize this size dialect: {message}"
        );
    }

    for message in [
        // A bad KEY is not a long document: shrinking wastes a cycle and still fails.
        "Incorrect API key provided",
        // Rate limiting is transient, not a document verdict.
        "Rate limit reached for requests",
        // A model the route cannot use must not be shrunk.
        "The model `embed-v5.0-fast` does not exist",
        "",
    ] {
        assert!(
            !is_too_long_message(message),
            "must not classify this as too-long: {message}"
        );
    }
}

/// The estimate is used only for budgeting, so it must at least be
/// monotonically increasing and never zero (a zero would let infinitely many
/// documents into one request).
#[test]
fn test_estimate_tokens_is_never_zero() {
    assert_eq!(estimate_tokens(""), 1);
    assert_eq!(estimate_tokens("ab"), 1);
    assert!(estimate_tokens(&"a".repeat(300)) > estimate_tokens(&"a".repeat(30)));
}

/// Truncation keeps the HEAD and actually lands within the budget.
///
/// Head, not tail: for code the signature and enclosing definition come first,
/// so a tail-truncated symbol would keep closing braces and lose the thing
/// being defined.
#[test]
fn test_truncate_to_token_budget_keeps_the_head() {
    // HEAD is longer than the whole byte budget, so nothing from TAIL may
    // survive. A budget of 100 tokens is 300 bytes; HEAD alone is 400.
    let original = format!("{}{}", "HEAD".repeat(100), "TAIL".repeat(100));
    let truncated = truncate_to_token_budget(std::borrow::Cow::Borrowed(&original), 100);
    assert!(truncated.starts_with("HEAD"), "the head must survive");
    assert!(!truncated.contains("TAIL"), "the tail must be dropped");
    assert_eq!(truncated.len(), 300);
    assert!(estimate_tokens(&truncated) <= 100);
}

/// A text that already fits is returned untouched, borrowing rather than
/// copying — the common path for a short node.
#[test]
fn test_truncate_to_token_budget_is_a_no_op_when_it_fits() {
    let input = std::borrow::Cow::Borrowed("short");
    let out = truncate_to_token_budget(input, 2048);
    assert_eq!(out, "short");
    assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
}

/// Truncation must land on a char boundary: slicing mid-codepoint panics.
#[test]
fn test_truncate_to_token_budget_respects_char_boundaries() {
    // Multibyte everywhere, so a naive byte cut would split a codepoint.
    let original = "é".repeat(400);
    let truncated = truncate_to_token_budget(std::borrow::Cow::Borrowed(&original), 50);
    assert!(truncated.is_char_boundary(truncated.len()));
    assert!(estimate_tokens(&truncated) <= 50);
    // Every surviving character is still a whole 'é'.
    assert!(truncated.chars().all(|c| c == 'é'));
}

/// Truncation is skipped entirely when no limit is configured, so a local
/// endpoint sees the exact document text it always did.
#[test]
fn test_truncate_with_zero_limit_is_a_no_op() {
    let original = "x".repeat(10_000);
    let out = truncate_to_token_budget(std::borrow::Cow::Borrowed(&original), 0);
    assert_eq!(out.len(), 10_000);
}
