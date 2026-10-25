//! Unit tests for `remote`, moved out of the parent file by pure code motion
//! (Large File Detection gate). Declared as `mod tests` there, so `super::*`
//! still reaches every private item.

use super::*;

#[test]
fn test_remote_provider_default_dimension() {
    let provider = RemoteProvider::OpenAI {
        model: "text-embedding-3-small".to_string(),
    };
    assert_eq!(provider.default_dimension(), 1536);
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
