use super::*;

impl WorkerRuntime {
    #[cfg(feature = "onnx")]
    pub(super) fn run_onnx_embed<S: AsRef<str>>(
        &self,
        session: &Arc<Mutex<Session>>,
        tokenizer: &Arc<tokenizers::Tokenizer>,
        texts: &[S],
        expected_dim: usize,
        cancel_token: &Arc<AtomicBool>,
    ) -> Result<EmbedResponse, WorkerError> {
        if expected_dim == 0 {
            return Err(WorkerError {
                kind: ErrorKind::InvalidRequest,
                message: "expected_dim must be non-zero".to_string(),
            });
        }

        if texts.is_empty() {
            return Ok(EmbedResponse::new(vec![], 0, expected_dim));
        }

        let active_provider = &self.provider_runtime_status.execution_provider;
        let inference_batch_size =
            configured_onnx_inference_batch_size(&self.config.model_name, active_provider);
        let fixed_batch = active_provider.eq_ignore_ascii_case("migraphx")
            || active_provider.eq_ignore_ascii_case("rocm");

        let all_pooled = self.run_onnx_embed_text_batch_loop(
            texts,
            inference_batch_size,
            fixed_batch,
            expected_dim,
            cancel_token,
            |sub_texts| {
                tokenizer
                    .encode_batch(sub_texts.iter().map(|text| text.as_ref()).collect(), true)
                    .map_err(|e| WorkerError {
                        kind: ErrorKind::Tokenizer,
                        message: format!("tokenization failed: {}", e),
                    })
            },
            |encodings, dim| self.run_onnx_embed_sub_batch(session, encodings, dim),
        )?;

        EmbedResponse::try_new(all_pooled, texts.len(), expected_dim).map_err(|message| {
            WorkerError {
                kind: ErrorKind::Inference,
                message,
            }
        })
    }

    /// Tokenize and infer one bounded text sub-batch at a time.
    ///
    /// This is deliberately sequential: it bounds tokenizer memory and lets
    /// inference begin as soon as the first sub-batch is ready without sharing
    /// the tokenizer or ORT session across threads. A future pipelined design
    /// must preserve the same cancellation, padding, ordering, and error
    /// semantics before it can replace this helper.
    #[cfg(feature = "onnx")]
    pub fn run_onnx_embed_text_batch_loop<S, E, T, R>(
        &self,
        texts: &[S],
        inference_batch_size: usize,
        fixed_batch: bool,
        expected_dim: usize,
        cancel_token: &Arc<AtomicBool>,
        mut tokenize: T,
        mut run_sub_batch: R,
    ) -> Result<Vec<f32>, WorkerError>
    where
        S: AsRef<str>,
        E: Clone,
        T: FnMut(&[S]) -> Result<Vec<E>, WorkerError>,
        R: FnMut(&[E], usize) -> Result<Vec<f32>, WorkerError>,
    {
        if inference_batch_size == 0 {
            return Err(WorkerError {
                kind: ErrorKind::InvalidRequest,
                message: "inference batch size must be non-zero".to_string(),
            });
        }

        let mut all_pooled = Vec::with_capacity(texts.len() * expected_dim);
        for sub_texts in texts.chunks(inference_batch_size) {
            if cancel_token.load(Ordering::Acquire) {
                tracing::info!(
                    "cancel flag detected between tokenized sub-batches; aborting embed after {} of {} texts",
                    all_pooled.len() / expected_dim.max(1),
                    texts.len()
                );
                return Err(WorkerError {
                    kind: ErrorKind::Inference,
                    message: "batch cancelled between sub-batches".to_string(),
                });
            }
            self.touch();

            let encodings = tokenize(sub_texts)?;
            if encodings.len() != sub_texts.len() {
                return Err(WorkerError {
                    kind: ErrorKind::Tokenizer,
                    message: format!(
                        "tokenizer returned {} encodings for {} texts",
                        encodings.len(),
                        sub_texts.len()
                    ),
                });
            }

            if fixed_batch && encodings.len() < inference_batch_size {
                let real_count = encodings.len();
                let mut padded = encodings;
                if let Some(template) = padded.first().cloned() {
                    padded.resize(inference_batch_size, template);
                }
                let pooled = run_sub_batch(&padded, expected_dim)?;
                let expected_values = inference_batch_size * expected_dim;
                if pooled.len() != expected_values {
                    return Err(WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "fixed-batch inference returned {} values, expected {} ({} rows x {} dim)",
                            pooled.len(),
                            expected_values,
                            inference_batch_size,
                            expected_dim
                        ),
                    });
                }
                let real_values = real_count * expected_dim;
                all_pooled.extend_from_slice(&pooled[..real_values]);
            } else {
                let pooled = run_sub_batch(&encodings, expected_dim)?;
                let expected_values = encodings.len() * expected_dim;
                if pooled.len() != expected_values {
                    return Err(WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "inference returned {} values, expected {} ({} rows x {} dim)",
                            pooled.len(),
                            expected_values,
                            encodings.len(),
                            expected_dim
                        ),
                    });
                }
                all_pooled.extend_from_slice(&pooled);
            }
        }

        let expected_total = texts.len() * expected_dim;
        if all_pooled.len() != expected_total {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: format!(
                    "aggregate inference output length mismatch: got {}, expected {}",
                    all_pooled.len(),
                    expected_total
                ),
            });
        }
        Ok(all_pooled)
    }

    /// For fixed-batch providers (MIGraphX/ROCm), a final partial sub-batch is
    /// padded up to `inference_batch_size` (using the first encoding as a
    /// template) before inference and then trimmed back to the real sub-batch
    /// count — mirroring the rerank path. For dynamic-batch providers
    /// (CPU/CUDA), every sub-batch is forwarded unchanged and appended.
    ///
    /// The `run_sub_batch` closure performs inference for one sub-batch and
    /// returns the flattened row-major pooled vectors; it receives the final
    /// `expected_dim` so padding-aware callers can size their output.
    #[cfg(all(feature = "onnx", test))]
    pub(super) fn run_onnx_embed_batch_loop<F>(
        &self,
        encodings: &[tokenizers::Encoding],
        inference_batch_size: usize,
        fixed_batch: bool,
        expected_dim: usize,
        mut run_sub_batch: F,
    ) -> Result<Vec<f32>, WorkerError>
    where
        F: FnMut(&[tokenizers::Encoding], usize) -> Result<Vec<f32>, WorkerError>,
    {
        let mut all_pooled: Vec<f32> = Vec::with_capacity(encodings.len() * expected_dim);

        for sub_batch in encodings.chunks(inference_batch_size) {
            // Keep the worker alive across a large multi-batch test drive.
            self.touch();

            if fixed_batch && sub_batch.len() < inference_batch_size {
                // Fixed-batch providers (MIGraphX/ROCm) require a fixed input
                // batch shape, so pad this final partial sub-batch up to
                // inference_batch_size (using the first encoding as a template)
                // before running inference, then trim the results back to the
                // real sub-batch count. Mirrors the rerank path.
                let mut padded = sub_batch.to_vec();
                if let Some(template) = sub_batch.first() {
                    padded.resize(inference_batch_size, template.clone());
                }
                let sub_pooled = run_sub_batch(&padded, expected_dim)?;
                all_pooled.extend_from_slice(&sub_pooled[..sub_batch.len() * expected_dim]);
            } else {
                let sub_pooled = run_sub_batch(sub_batch, expected_dim)?;
                all_pooled.extend_from_slice(&sub_pooled);
            }
        }

        Ok(all_pooled)
    }

    /// Run ONNX inference on a single sub-batch, including collapsed-batch
    /// recovery. Fixed-batch padding is performed by the text/encoding batch
    /// loop so collapsed single-row retries cannot be padded recursively.
    #[cfg(feature = "onnx")]
    pub(super) fn run_onnx_embed_sub_batch(
        &self,
        session: &Arc<Mutex<Session>>,
        encodings: &[tokenizers::Encoding],
        expected_dim: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        match self.run_onnx_embed_sub_batch_inner(session, encodings, expected_dim) {
            Ok(vectors) => Ok(vectors),
            Err(ref err) if err.message.starts_with(COLLAPSED_BATCH_SENTINEL) => {
                tracing::warn!(
                    "ONNX model collapsed batch dimension (sent {}); retrying each sequence \
                     individually with batch_size=1",
                    encodings.len()
                );
                let mut all_vectors = Vec::with_capacity(encodings.len() * expected_dim);
                for encoding in encodings {
                    let single = std::slice::from_ref(encoding);
                    let vectors =
                        self.run_onnx_embed_sub_batch_inner(session, single, expected_dim)?;
                    all_vectors.extend_from_slice(&vectors);
                }
                Ok(all_vectors)
            }
            Err(err) => Err(err),
        }
    }

    /// Run one already-shaped ONNX sub-batch: build tensors, invoke ORT once,
    /// and validate/pool/normalize the outputs. The caller owns padding and
    /// collapsed-batch recovery policy.
    #[cfg(feature = "onnx")]
    pub(super) fn run_onnx_embed_sub_batch_inner(
        &self,
        session: &Arc<Mutex<Session>>,
        encodings: &[tokenizers::Encoding],
        expected_dim: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        let batch_size = encodings.len();
        if batch_size == 0 {
            return Ok(vec![]);
        }

        let max_len = configured_onnx_sequence_len();
        if env_flag(ONNX_LOG_SHAPES_ENV) {
            let max_encoding_len = encodings.iter().map(|e| e.len()).max().unwrap_or(0);
            tracing::info!(
                batch_size,
                max_len,
                max_encoding_len,
                "ONNX embedding input shape"
            );
        }

        if max_len == 0 {
            return Ok(vec![0.0f32; batch_size * expected_dim]);
        }

        // Create input tensors: [batch_size, seq_len]
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        for encoding in encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();

            // Pad to max_len
            for i in 0..max_len {
                if i < ids.len() {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                } else {
                    input_ids.push(0i64);
                    attention_mask.push(0i64);
                }
            }
        }

        // Build the [batch_size, seq_len] input tensors. The array+tensor
        // creation is identical across all four inputs, so a local macro keeps
        // each as a single (labelled) expression with one error path.
        macro_rules! make_i64_tensor {
            ($data:expr, $label:literal) => {
                ort::value::Tensor::from_array(
                    ndarray::Array2::from_shape_vec((batch_size, max_len), $data).map_err(|e| {
                        WorkerError {
                            kind: ErrorKind::Inference,
                            message: format!("failed to create {} array: {}", $label, e),
                        }
                    })?,
                )
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create {} tensor: {}", $label, e),
                })?
            };
        }

        let input_ids_tensor = make_i64_tensor!(input_ids, "input_ids");
        let attention_mask_tensor = make_i64_tensor!(attention_mask.clone(), "attention_mask");
        let position_ids_tensor =
            make_i64_tensor!(build_position_ids(batch_size, max_len), "position_ids");
        // token_type_ids: BERT/GTE-style models have a `token_type_embeddings`
        // layer that requires this input — without it inference fails at
        // `/embeddings/token_type_embeddings/Gather` with "Missing Input:
        // token_type_ids". For single-text retrieval every token is segment 0,
        // so feed all-zeros. Models that lack this input never read it.
        let token_type_ids_tensor =
            make_i64_tensor!(vec![0i64; batch_size * max_len], "token_type_ids");

        let mut session_guard = session.lock().map_err(|e| WorkerError {
            kind: ErrorKind::OnnxRuntime,
            message: format!("failed to lock ONNX session: {}", e),
        })?;

        let (uses_position_ids, uses_token_type_ids) = *self.input_names.get_or_init(|| {
            (
                session_guard
                    .inputs()
                    .iter()
                    .any(|input| input.name() == "position_ids"),
                session_guard
                    .inputs()
                    .iter()
                    .any(|input| input.name() == "token_type_ids"),
            )
        });
        // Feed only the inputs the model declares; extras would be rejected.
        // Arms are mutually exclusive, so each tensor moves on exactly one path.
        let outputs = match (uses_position_ids, uses_token_type_ids) {
            (true, true) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "position_ids" => position_ids_tensor,
                "token_type_ids" => token_type_ids_tensor,
            }),
            (true, false) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "position_ids" => position_ids_tensor,
            }),
            (false, true) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            }),
            (false, false) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
            }),
        }
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("ONNX inference failed: {}", e),
        })?;

        match self.finalize_embed_output(&outputs, batch_size, expected_dim, &attention_mask) {
            Ok(vectors) => Ok(vectors),
            Err(err) => Err(err),
        }
    }

    /// Validate the embed output shape, normalize a pre-pooled `[b, hidden]`
    /// tensor in place, or pool+normalize a `[b, seq, hidden]` tensor. Extracted
    /// from `run_onnx_embed_sub_batch` to keep that function's branch count bounded.
    #[cfg(feature = "onnx")]
    pub(super) fn finalize_embed_output(
        &self,
        outputs: &ort::session::SessionOutputs<'_>,
        batch_size: usize,
        expected_dim: usize,
        attention_mask: &[i64],
    ) -> Result<Vec<f32>, WorkerError> {
        if outputs.len() == 0 {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: "ONNX model returned no outputs".to_string(),
            });
        }
        // Expected: [batch_size, seq_len, hidden_dim] or [batch_size, hidden_dim].
        let output_shape: Vec<usize> = outputs[0].shape().iter().map(|&d| d as usize).collect();
        // MIGraphX may return float16 even when the source graph is float32;
        // normalize both provider output types to the f32 storage contract.
        let embeddings_f32 = extract_output_tensor_f32(&outputs[0]).map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to extract output tensor: {}", e),
        })?;
        let (actual_seq_len, hidden_dim) = match output_shape.as_slice() {
            [bs, sl, hd] if *bs == batch_size => {
                if *hd != expected_dim {
                    return Err(WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "output dimension mismatch: model produced {}, expected {}",
                            hd, expected_dim
                        ),
                    });
                }
                (*sl, *hd)
            }
            // Collapsed batch: model returned [1, seq_len, hidden_dim] when
            // batch_size > 1 was sent. Some ONNX exports (non-dynamic variants)
            // silently collapse the batch dimension. Signal the caller to retry
            // each sequence individually with batch_size=1.
            [1, sl, hd] if batch_size > 1 && *hd == expected_dim => {
                return Err(WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!(
                        "{}: model collapsed batch dimension (sent {}, got [1, {}, {}])",
                        COLLAPSED_BATCH_SENTINEL, batch_size, sl, hd
                    ),
                });
            }
            [bs, hd] if *bs == batch_size => {
                if *hd != expected_dim {
                    return Err(WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "output dimension mismatch: model produced {}, expected {}",
                            hd, expected_dim
                        ),
                    });
                }
                // Already pooled: L2-normalize per row.
                let dim = *hd;
                let mut embeddings_f32 = embeddings_f32;
                for b in 0..batch_size {
                    let start = b * dim;
                    let end = start + dim;
                    let row = &mut embeddings_f32[start..end];
                    let norm: f32 = row.iter().map(|v| v * v).sum::<f32>().sqrt();
                    if norm > 1e-10f32 {
                        for v in row.iter_mut() {
                            *v /= norm;
                        }
                    }
                }
                return Ok(embeddings_f32);
            }
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!(
                        "unexpected output shape {:?}; expected [{}, seq_len, hidden_dim] or [{}, hidden_dim]",
                        output_shape, batch_size, batch_size
                    ),
                });
            }
        };
        if embeddings_f32.len() != batch_size * actual_seq_len * hidden_dim {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: format!(
                    "output size mismatch: shape {:?} implies {} elements, got {}",
                    output_shape,
                    batch_size * actual_seq_len * hidden_dim,
                    embeddings_f32.len()
                ),
            });
        }
        // Select the final unpadded token required by Qwen3, then L2 normalize.
        let pooled = self.pool_and_normalize(
            &embeddings_f32,
            batch_size,
            actual_seq_len,
            attention_mask,
            hidden_dim,
        )?;
        Ok(pooled.vectors)
    }

    #[cfg(feature = "onnx")]
    pub(super) fn pool_and_normalize(
        &self,
        embeddings: &[f32],
        batch_size: usize,
        seq_len: usize,
        attention_mask: &[i64],
        expected_dim: usize,
    ) -> Result<EmbedResponse, WorkerError> {
        let hidden_dim = expected_dim;
        let mut pooled: Vec<f32> = Vec::with_capacity(batch_size * hidden_dim);
        let mut row: Vec<f32> = vec![0.0f32; hidden_dim];

        for b in 0..batch_size {
            row.fill(0.0);
            let mask_start = b * seq_len;
            let last_token = (0..seq_len)
                .rev()
                .find(|&s| attention_mask.get(mask_start + s).copied().unwrap_or(0) > 0);
            if let Some(token_index) = last_token {
                let embedding_start = (b * seq_len + token_index) * hidden_dim;
                let embedding = embeddings
                    .get(embedding_start..embedding_start + hidden_dim)
                    .ok_or_else(|| WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "embedding output is too short: need elements {}..{}, got {}",
                            embedding_start,
                            embedding_start + hidden_dim,
                            embeddings.len()
                        ),
                    })?;
                row.copy_from_slice(embedding);
            }

            let norm = row.iter().map(|value| value * value).sum::<f32>().sqrt();

            if norm > 1e-10f32 {
                for value in &mut row {
                    *value /= norm;
                }
            }

            pooled.extend_from_slice(&row);
        }

        Ok(EmbedResponse::new(pooled, batch_size, expected_dim))
    }
}
