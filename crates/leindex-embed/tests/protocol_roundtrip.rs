// Protocol round-trip integration test
//
// VAL-CPHASE-003: Worker request/response frames serialize and deserialize
// without losing batch identity, payload ordering, dimensions, or error identity.

use leindex_embed::protocol::{
    embed_request_frame, embed_response_frame, error_frame, rerank_request_frame,
    rerank_response_frame, BatchId, EmbedRequest, EmbedResponse, ErrorKind, Frame, MsgType,
    Request, RerankDocument, RerankRequest, RerankResponse, Response, WorkerError,
};

/// Helper: encode a frame to wire bytes and decode it back.
fn roundtrip_frame(frame: &Frame) -> Frame {
    let wire = frame.encode_wire().expect("encode should succeed");
    // Skip the 4-byte length prefix for Frame::from_wire_bytes
    Frame::from_wire_bytes(&wire[4..]).expect("decode should succeed")
}

// ── Embed request round-trip ────────────────────────────────────────────

#[test]
fn test_embed_request_roundtrip_single_text() {
    let batch_id = BatchId::new(1);
    let request = EmbedRequest {
        texts: vec!["fn main() {}".to_string()],
        expected_dim: 1024,
    };

    let frame = embed_request_frame(batch_id, request).unwrap();
    let decoded = roundtrip_frame(&frame);

    assert_eq!(decoded.header.batch_id, batch_id);
    assert_eq!(decoded.header.msg_type, MsgType::EmbedRequest);

    let req: Request = decoded.decode_payload().unwrap();
    match req {
        Request::Embed(e) => {
            assert_eq!(e.texts.len(), 1);
            assert_eq!(e.texts[0], "fn main() {}");
            assert_eq!(e.expected_dim, 1024);
        }
        _ => panic!("expected Embed request"),
    }
}

#[test]
fn test_embed_request_roundtrip_batch() {
    let batch_id = BatchId::new(42);
    let texts: Vec<String> = (0..50).map(|i| format!("text number {}", i)).collect();
    let request = EmbedRequest {
        texts: texts.clone(),
        expected_dim: 768,
    };

    let frame = embed_request_frame(batch_id, request).unwrap();
    let decoded = roundtrip_frame(&frame);

    assert_eq!(decoded.header.batch_id, batch_id);

    let req: Request = decoded.decode_payload().unwrap();
    match req {
        Request::Embed(e) => {
            // Payload ordering is preserved
            assert_eq!(e.texts, texts);
            assert_eq!(e.expected_dim, 768);
        }
        _ => panic!("expected Embed request"),
    }
}

#[test]
fn test_embed_request_roundtrip_unicode() {
    let batch_id = BatchId::new(100);
    let request = EmbedRequest {
        texts: vec![
            "日本語テスト".to_string(),
            "emoji 🎉🚀".to_string(),
            "Ünïcödé".to_string(),
        ],
        expected_dim: 256,
    };

    let frame = embed_request_frame(batch_id, request).unwrap();
    let decoded = roundtrip_frame(&frame);

    let req: Request = decoded.decode_payload().unwrap();
    match req {
        Request::Embed(e) => {
            assert_eq!(e.texts[0], "日本語テスト");
            assert_eq!(e.texts[1], "emoji 🎉🚀");
            assert_eq!(e.texts[2], "Ünïcödé");
        }
        _ => panic!("expected Embed request"),
    }
}

// ── Embed response round-trip ───────────────────────────────────────────

#[test]
fn test_embed_response_roundtrip() {
    let batch_id = BatchId::new(200);
    let response = EmbedResponse::new(
        vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
        2, // count
        4, // dimension
    );

    let frame = embed_response_frame(batch_id, response).unwrap();
    let decoded = roundtrip_frame(&frame);

    assert_eq!(decoded.header.batch_id, batch_id);
    assert_eq!(decoded.header.msg_type, MsgType::EmbedResponse);

    let resp: Response = decoded.decode_payload().unwrap();
    match resp {
        Response::Embed(e) => {
            assert_eq!(e.count, 2);
            assert_eq!(e.dimension, 4);
            assert_eq!(e.vectors, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
            // Verify flat row-major access
            assert_eq!(e.get_embedding(0), Some(&[1.0, 2.0, 3.0, 4.0][..]));
            assert_eq!(e.get_embedding(1), Some(&[5.0, 6.0, 7.0, 8.0][..]));
        }
        _ => panic!("expected Embed response"),
    }
}

#[test]
fn test_embed_response_roundtrip_large_batch() {
    let batch_id = BatchId::new(300);
    let count = 100;
    let dim = 1024;
    let vectors: Vec<f32> = (0..(count * dim)).map(|i| i as f32 * 0.001).collect();

    let response = EmbedResponse::new(vectors.clone(), count, dim);

    let frame = embed_response_frame(batch_id, response).unwrap();
    let decoded = roundtrip_frame(&frame);

    let resp: Response = decoded.decode_payload().unwrap();
    match resp {
        Response::Embed(e) => {
            assert_eq!(e.count, count);
            assert_eq!(e.dimension, dim);
            assert_eq!(e.vectors, vectors);
        }
        _ => panic!("expected Embed response"),
    }
}

// ── Rerank request/response round-trip ──────────────────────────────────

#[test]
fn test_rerank_request_roundtrip() {
    let batch_id = BatchId::new(400);
    let request = RerankRequest {
        query: "find authentication code".to_string(),
        documents: vec![
            RerankDocument {
                id: "node1".to_string(),
                content: "fn authenticate(user: &str)".to_string(),
                initial_score: 0.95,
            },
            RerankDocument {
                id: "node2".to_string(),
                content: "struct AuthConfig".to_string(),
                initial_score: 0.80,
            },
        ],
    };

    let frame = rerank_request_frame(batch_id, request).unwrap();
    let decoded = roundtrip_frame(&frame);

    assert_eq!(decoded.header.batch_id, batch_id);
    assert_eq!(decoded.header.msg_type, MsgType::RerankRequest);

    let req: Request = decoded.decode_payload().unwrap();
    match req {
        Request::Rerank(r) => {
            assert_eq!(r.query, "find authentication code");
            assert_eq!(r.documents.len(), 2);
            assert_eq!(r.documents[0].id, "node1");
            assert_eq!(r.documents[0].initial_score, 0.95);
            assert_eq!(r.documents[1].id, "node2");
        }
        _ => panic!("expected Rerank request"),
    }
}

#[test]
fn test_rerank_response_roundtrip() {
    let batch_id = BatchId::new(500);
    let response = RerankResponse {
        results: vec![
            leindex_embed::protocol::RerankResult {
                id: "node1".to_string(),
                original_score: 0.95,
                rerank_score: 0.98,
                combined_score: 0.97,
            },
            leindex_embed::protocol::RerankResult {
                id: "node2".to_string(),
                original_score: 0.80,
                rerank_score: 0.75,
                combined_score: 0.77,
            },
        ],
    };

    let frame = rerank_response_frame(batch_id, response).unwrap();
    let decoded = roundtrip_frame(&frame);

    assert_eq!(decoded.header.batch_id, batch_id);
    assert_eq!(decoded.header.msg_type, MsgType::RerankResponse);

    let resp: Response = decoded.decode_payload().unwrap();
    match resp {
        Response::Rerank(r) => {
            assert_eq!(r.results.len(), 2);
            assert_eq!(r.results[0].id, "node1");
            assert_eq!(r.results[0].combined_score, 0.97);
        }
        _ => panic!("expected Rerank response"),
    }
}

// ── Error round-trip ────────────────────────────────────────────────────

#[test]
fn test_error_roundtrip() {
    let batch_id = BatchId::new(600);
    let error = WorkerError {
        kind: ErrorKind::OnnxRuntime,
        message: "CUDA execution provider not available".to_string(),
    };

    let frame = error_frame(batch_id, error).unwrap();
    let decoded = roundtrip_frame(&frame);

    assert_eq!(decoded.header.batch_id, batch_id);
    assert_eq!(decoded.header.msg_type, MsgType::Error);

    let resp: Response = decoded.decode_payload().unwrap();
    match resp {
        Response::Error(e) => {
            assert_eq!(e.kind, ErrorKind::OnnxRuntime);
            assert_eq!(e.message, "CUDA execution provider not available");
        }
        _ => panic!("expected Error response"),
    }
}

#[test]
fn test_all_error_kinds_roundtrip() {
    let kinds = [
        ErrorKind::OnnxRuntime,
        ErrorKind::ModelNotFound,
        ErrorKind::Tokenizer,
        ErrorKind::Inference,
        ErrorKind::InvalidRequest,
        ErrorKind::Internal,
    ];

    for (i, kind) in kinds.iter().enumerate() {
        let batch_id = BatchId::new(i as u64);
        let error = WorkerError {
            kind: *kind,
            message: format!("test error for {:?}", kind),
        };

        let frame = error_frame(batch_id, error).unwrap();
        let decoded = roundtrip_frame(&frame);

        let resp: Response = decoded.decode_payload().unwrap();
        match resp {
            Response::Error(e) => {
                assert_eq!(e.kind, *kind);
            }
            _ => panic!("expected Error response for kind {:?}", kind),
        }
    }
}

// ── Batch identity preservation ─────────────────────────────────────────

#[test]
fn test_batch_id_preserved_across_all_message_types() {
    let batch_id = BatchId::new(0xCAFEBABE);

    // Embed request
    let frame = embed_request_frame(
        batch_id,
        EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 4,
        },
    )
    .unwrap();
    let decoded = roundtrip_frame(&frame);
    assert_eq!(decoded.header.batch_id, batch_id);

    // Embed response
    let frame =
        embed_response_frame(batch_id, EmbedResponse::new(vec![1.0, 2.0, 3.0, 4.0], 1, 4)).unwrap();
    let decoded = roundtrip_frame(&frame);
    assert_eq!(decoded.header.batch_id, batch_id);

    // Rerank request
    let frame = rerank_request_frame(
        batch_id,
        RerankRequest {
            query: "q".to_string(),
            documents: vec![],
        },
    )
    .unwrap();
    let decoded = roundtrip_frame(&frame);
    assert_eq!(decoded.header.batch_id, batch_id);

    // Error
    let frame = error_frame(
        batch_id,
        WorkerError {
            kind: ErrorKind::Internal,
            message: "test".to_string(),
        },
    )
    .unwrap();
    let decoded = roundtrip_frame(&frame);
    assert_eq!(decoded.header.batch_id, batch_id);
}

// ── Wire format consistency ─────────────────────────────────────────────

#[test]
fn test_wire_format_has_length_prefix() {
    let frame = embed_request_frame(
        BatchId::new(1),
        EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 4,
        },
    )
    .unwrap();

    let wire = frame.encode_wire().unwrap();
    assert!(
        wire.len() > 4,
        "wire output should have 4-byte length prefix"
    );

    let len = u32::from_le_bytes([wire[0], wire[1], wire[2], wire[3]]) as usize;
    assert_eq!(
        len,
        wire.len() - 4,
        "length prefix should match remaining bytes"
    );
}

#[test]
fn test_empty_embed_request_roundtrip() {
    let batch_id = BatchId::new(0);
    let request = EmbedRequest {
        texts: vec![],
        expected_dim: 1024,
    };

    let frame = embed_request_frame(batch_id, request).unwrap();
    let decoded = roundtrip_frame(&frame);

    let req: Request = decoded.decode_payload().unwrap();
    match req {
        Request::Embed(e) => {
            assert!(e.texts.is_empty());
        }
        _ => panic!("expected Embed request"),
    }
}

#[test]
fn test_empty_embed_response_roundtrip() {
    let batch_id = BatchId::new(0);
    let response = EmbedResponse::new(vec![], 0, 1024);

    let frame = embed_response_frame(batch_id, response).unwrap();
    let decoded = roundtrip_frame(&frame);

    let resp: Response = decoded.decode_payload().unwrap();
    match resp {
        Response::Embed(e) => {
            assert_eq!(e.count, 0);
            assert!(e.vectors.is_empty());
        }
        _ => panic!("expected Embed response"),
    }
}
