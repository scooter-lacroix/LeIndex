use super::*;

impl WorkerRuntime {
    /// Handle a rerank request.
    /// Lazily load + return the reranker cross-encoder session + tokenizer (on
    /// demand). The reranker is loaded only on the first rerank request and
    /// evicted after `RERANK_IDLE_EVICTION_SECS` of idleness
    /// (`maybe_evict_rerank`). Uses the CPU execution provider so it does not
    /// contend with the embedder's GPU session and stays cheap (a cross-encoder
    /// over a small top-N). Double-checked under `rerank_init_lock` so concurrent
    /// socket requests don't double-load.
    #[cfg(feature = "onnx")]
    pub(super) fn ensure_rerank_session(
        &self,
    ) -> Result<(Arc<Mutex<Session>>, Arc<tokenizers::Tokenizer>), WorkerError> {
        // Fast path: already resident.
        if let (Some(s), Some(t)) = (
            self.rerank_session.lock().ok().and_then(|g| g.clone()),
            self.rerank_tokenizer.lock().ok().and_then(|g| g.clone()),
        ) {
            *self.last_rerank_activity.lock().unwrap() = Instant::now();
            return Ok((s, t));
        }
        let _init = self.rerank_init_lock.lock().map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("rerank init lock poisoned: {}", e),
        })?;
        // Double-check after acquiring the lock.
        if let (Some(s), Some(t)) = (
            self.rerank_session.lock().ok().and_then(|g| g.clone()),
            self.rerank_tokenizer.lock().ok().and_then(|g| g.clone()),
        ) {
            *self.last_rerank_activity.lock().unwrap() = Instant::now();
            return Ok((s, t));
        }
        let model_name = self.config.rerank_model_name.clone();
        if model_name.trim().is_empty() {
            return Err(WorkerError {
                kind: ErrorKind::ModelNotFound,
                message: "no rerank model configured".to_string(),
            });
        }
        let model_path =
            crate::embed::model_path::ModelResolver::resolve(&model_name).map_err(|e| {
                WorkerError {
                    kind: ErrorKind::ModelNotFound,
                    message: format!("rerank model '{}' not found: {}", model_name, e),
                }
            })?;
        // Rerank tokenizer: convention `{model_stem}-tokenizer.json` beside the
        // model. ModelResolver::resolve_tokenizer ignores model_name and would
        // return the EMBED tokenizer (wrong vocab), so derive the path
        // explicitly: bge-reranker-base.onnx -> bge-reranker-base-tokenizer.json.
        let tokenizer_path = format!("{}-tokenizer.json", model_path.with_extension("").display());
        let tokenizer = Arc::new(tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(
            |e| WorkerError {
                kind: ErrorKind::Tokenizer,
                message: format!("rerank tokenizer load failed ({}): {}", tokenizer_path, e),
            },
        )?);
        // Use the same concrete provider the embedder session resolved to
        // (self.provider_runtime_status.execution_provider), so the
        // cross-encoder is fast (~1-3s after a one-time compile cached as a
        // .mxr) and the reranker never receives the unresolved "auto" token.
        // The native ORT_MIGraphX_MODEL_CACHE_PATH cache persists across idle
        // evictions, so on-demand reloads stay warm. CPU is ~70s/query for
        // top-20 × 512 — unusable interactively. If the provider is unavailable
        // for this model, build_session falls back to CPU automatically.
        let provider = self.provider_runtime_status.execution_provider.as_str();
        let outcome =
            Self::build_session(&model_path, provider, self.config.ort_threads).map_err(|e| {
                WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("rerank session build failed: {}", e),
                }
            })?;
        let session = Arc::new(Mutex::new(outcome.session));
        tracing::info!(model = %model_name, provider, "reranker loaded on demand");
        *self.rerank_session.lock().unwrap() = Some(session.clone());
        *self.rerank_tokenizer.lock().unwrap() = Some(tokenizer.clone());
        *self.last_rerank_activity.lock().unwrap() = Instant::now();
        Ok((session, tokenizer))
    }

    /// Drop the reranker session + tokenizer if it has been idle longer than
    /// `RERANK_IDLE_EVICTION_SECS`. Called from the worker idle loop so the
    /// reranker's memory is reclaimed between rerank bursts. No-op if the
    /// reranker isn't loaded.
    #[cfg(feature = "onnx")]
    pub fn maybe_evict_rerank(&self) {
        let evict = {
            let last = match self.last_rerank_activity.lock() {
                Ok(g) => *g,
                Err(_) => return,
            };
            self.rerank_session
                .lock()
                .map(|g| g.is_some())
                .unwrap_or(false)
                && last.elapsed().as_secs() > RERANK_IDLE_EVICTION_SECS
        };
        if evict {
            let had = self
                .rerank_session
                .lock()
                .map(|mut g| g.take().is_some())
                .unwrap_or(false);
            if had {
                let _ = self.rerank_tokenizer.lock().map(|mut g| g.take());
                tracing::info!(secs = RERANK_IDLE_EVICTION_SECS, "reranker idle-evicted");
            }
        }
    }

    pub(super) fn handle_rerank(&self, frame: &Frame) -> Result<RerankResponse, WorkerError> {
        let request: Request = frame.decode_payload().map_err(|e| WorkerError {
            kind: ErrorKind::InvalidRequest,
            message: format!("failed to decode rerank request: {}", e),
        })?;

        let rerank_req = match request {
            Request::Rerank(req) => req,
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::InvalidRequest,
                    message: "expected Rerank request".to_string(),
                });
            }
        };

        #[cfg(feature = "onnx")]
        {
            // Reranker is loaded ON DEMAND (separate from the embed session).
            let (session, tokenizer) = self.ensure_rerank_session()?;
            self.run_onnx_rerank(&session, &tokenizer, &rerank_req)
        }

        #[cfg(not(feature = "onnx"))]
        {
            // No ONNX feature: return passthrough scores
            tracing::warn!("ONNX feature not enabled for rerank, using passthrough scores");
            let results: Vec<_> = rerank_req
                .documents
                .into_iter()
                .map(|doc| protocol::RerankResult {
                    id: doc.id,
                    original_score: doc.initial_score,
                    rerank_score: doc.initial_score,
                    combined_score: doc.initial_score,
                })
                .collect();
            Ok(RerankResponse { results })
        }
    }

    #[cfg(feature = "onnx")]
    pub(super) fn run_onnx_rerank(
        &self,
        session: &Arc<Mutex<Session>>,
        tokenizer: &Arc<tokenizers::Tokenizer>,
        rerank_req: &protocol::RerankRequest,
    ) -> Result<RerankResponse, WorkerError> {
        // Qwen3-Reranker (including the seq-cls ONNX port) REQUIRES its chat
        // template — the model was trained on the "Judge whether the Document
        // meets the requirements... answer yes or no" prompt. Raw (query, doc)
        // pairs are out-of-distribution and produce near-random logits (this was
        // the regression: the reranker surfaced tests/garbage). Build the full
        // templated string per document. Format verified against the seq-cls
        // model card. Instruction is code-tuned (Qwen3-Reranker is
        // instruction-sensitive; the web-search default is ~1-5% weaker on code).
        const RERANK_PREFIX: &str = "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n";
        const RERANK_SUFFIX: &str = "<|im_end|>\n<|im_start|>assistant\nThinking\n\nAnswer\n\n";
        const RERANK_INSTRUCT: &str =
            "Given a code search query, retrieve the most relevant source code";
        // Token length of the fixed assistant suffix, so rerank truncation can
        // preserve it: the Qwen3-Reranker predicts at the suffix position, so
        // dropping it (as a naive first-N truncation does) scores long
        // documents from an out-of-distribution prompt. Computed once per call;
        // add_special = false because the suffix appears mid-template (BOS is
        // only added at the template start).
        let rerank_suffix_len: usize = tokenizer
            .encode(RERANK_SUFFIX, false)
            .map(|enc| enc.get_ids().len())
            .unwrap_or(0);
        let pair_texts: Vec<String> = rerank_req
            .documents
            .iter()
            .map(|doc| {
                format!(
                    "{RERANK_PREFIX}<Instruct>: {RERANK_INSTRUCT}\n<Query>: {}\n<Document>: {}{RERANK_SUFFIX}",
                    rerank_req.query, doc.content
                )
            })
            .collect();

        // Batch tokenize all templated inputs.
        let encodings = tokenizer
            .encode_batch(pair_texts, true)
            .map_err(|e| WorkerError {
                kind: ErrorKind::Tokenizer,
                message: format!("rerank tokenization failed: {}", e),
            })?;

        if encodings.is_empty() {
            return Ok(RerankResponse { results: vec![] });
        }

        // Process encodings in sub-batches to bound peak memory.
        let mut all_rerank_scores: Vec<f32> = Vec::with_capacity(rerank_req.documents.len());

        let active_provider = &self.provider_runtime_status.execution_provider;
        let inference_batch_size =
            configured_onnx_inference_batch_size(&self.config.model_name, active_provider);
        let fixed_batch = active_provider.eq_ignore_ascii_case("migraphx")
            || active_provider.eq_ignore_ascii_case("rocm");
        for sub_batch in encodings.chunks(inference_batch_size) {
            self.touch();
            if fixed_batch && sub_batch.len() < inference_batch_size {
                let mut padded = sub_batch.to_vec();
                if let Some(template) = sub_batch.first() {
                    padded.resize(inference_batch_size, template.clone());
                }
                let sub_scores =
                    self.run_onnx_rerank_sub_batch(session, &padded, rerank_suffix_len)?;
                all_rerank_scores.extend_from_slice(&sub_scores[..sub_batch.len()]);
            } else {
                let sub_scores =
                    self.run_onnx_rerank_sub_batch(session, sub_batch, rerank_suffix_len)?;
                all_rerank_scores.extend_from_slice(&sub_scores);
            }
        }

        // Build results with combined scores: 70% rerank + 30% initial
        let mut results: Vec<_> = rerank_req
            .documents
            .iter()
            .zip(all_rerank_scores)
            .map(|(doc, rerank_score)| {
                let combined_score = 0.7 * rerank_score + 0.3 * doc.initial_score;
                protocol::RerankResult {
                    id: doc.id.clone(),
                    original_score: doc.initial_score,
                    rerank_score,
                    combined_score,
                }
            })
            .collect();

        // Sort by combined score descending
        results.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(RerankResponse { results })
    }

    /// Run ONNX rerank inference on a single sub-batch of encodings
    /// and return the scalar rerank scores.
    #[cfg(feature = "onnx")]
    pub(super) fn run_onnx_rerank_sub_batch(
        &self,
        session: &Arc<Mutex<Session>>,
        encodings: &[tokenizers::Encoding],
        suffix_token_len: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        let batch_size = encodings.len();
        if batch_size == 0 {
            return Ok(vec![]);
        }

        // Rerank uses a larger context than the embed model: the Qwen3-Reranker
        // chat template (prefix + instruct + query + document + suffix) is ~60
        // tokens before the document, so the embed model's 128 would truncate
        // the document + the required assistant suffix.
        let max_len = RERANK_MAX_SEQ_LEN;

        if max_len == 0 {
            // Return zero scores if tokenization produced nothing
            return Ok(vec![0.0f32; batch_size]);
        }

        // Build LEFT-padded input_ids / attention_mask (decoder-style padding
        // with overflow-safe suffix preservation).
        let (input_ids, attention_mask) =
            Self::build_rerank_input(encodings, max_len, suffix_token_len);

        // Create the [batch_size, seq_len] input tensors. Identical array+tensor
        // creation across all three, so a local macro gives each one error path.
        macro_rules! make_rerank_tensor {
            ($data:expr, $label:literal) => {
                ort::value::Tensor::from_array(
                    ndarray::Array2::from_shape_vec((batch_size, max_len), $data).map_err(|e| {
                        WorkerError {
                            kind: ErrorKind::Inference,
                            message: format!("failed to create rerank {} array: {}", $label, e),
                        }
                    })?,
                )
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create rerank {} tensor: {}", $label, e),
                })?
            };
        }
        let input_ids_tensor = make_rerank_tensor!(input_ids.clone(), "input_ids");
        let attention_mask_tensor = make_rerank_tensor!(attention_mask.clone(), "attention_mask");
        let position_ids_tensor =
            make_rerank_tensor!(build_position_ids(batch_size, max_len), "position_ids");

        let mut session_guard = session.lock().map_err(|e| WorkerError {
            kind: ErrorKind::OnnxRuntime,
            message: format!("failed to lock ONNX session for rerank: {}", e),
        })?;

        let uses_position_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "position_ids");
        let outputs = if uses_position_ids {
            session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "position_ids" => position_ids_tensor,
            })
        } else {
            session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
            })
        }
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("ONNX rerank inference failed: {}", e),
        })?;

        Self::finalize_rerank_output(&outputs, batch_size)
    }

    /// Build LEFT-padded `input_ids` / `attention_mask` for a rerank batch.
    /// Qwen3-Reranker is decoder-style: real tokens go at the END (pads at the
    /// start) so it attends up to the final assistant-suffix position. When an
    /// input overflows the window, the first (max_len - suffix) tokens AND the
    /// final `suffix_token_len` tokens are kept (document middle trimmed) so the
    /// assistant suffix the model predicts on is preserved.
    #[cfg(feature = "onnx")]
    pub(super) fn build_rerank_input(
        encodings: &[tokenizers::Encoding],
        max_len: usize,
        suffix_token_len: usize,
    ) -> (Vec<i64>, Vec<i64>) {
        let batch_size = encodings.len();
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        for encoding in encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let total = ids.len();
            let n = total.min(max_len);
            for _ in 0..(max_len - n) {
                input_ids.push(0);
                attention_mask.push(0);
            }
            if total > max_len && suffix_token_len > 0 && suffix_token_len < max_len {
                let front = max_len - suffix_token_len;
                for i in 0..front {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                }
                for i in (total - suffix_token_len)..total {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                }
            } else {
                for i in 0..n {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                }
            }
        }
        (input_ids, attention_mask)
    }

    /// Validate the rerank output shape and sigmoid-map the raw yes/no logits into
    /// [0,1] relevance scores. Extracted from `run_onnx_rerank_sub_batch`.
    #[cfg(feature = "onnx")]
    pub(super) fn finalize_rerank_output(
        outputs: &ort::session::SessionOutputs<'_>,
        batch_size: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        if outputs.len() == 0 {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: "ONNX rerank model returned no outputs".to_string(),
            });
        }
        let output = &outputs[0];
        let shape: Vec<usize> = output.shape().iter().map(|&d| d as usize).collect();
        let output_values = extract_output_tensor_f32(output).map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to extract rerank output tensor: {}", e),
        })?;
        let raw_logits: Vec<f32> = match shape.as_slice() {
            [n] if *n == batch_size => output_values,
            [n, 1] if *n == batch_size => output_values,
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!(
                        "unsupported rerank output shape {:?}; expected [{}] or [{}, 1]",
                        shape, batch_size, batch_size
                    ),
                });
            }
        };
        // Qwen3-Reranker seq-cls emits a raw yes/no logit; sigmoid maps it to
        // [0,1] relevance so the 0.7*rerank + 0.3*initial combine is on the same
        // scale as the initial search score.
        Ok(raw_logits
            .into_iter()
            .map(|l| 1.0 / (1.0 + (-l).exp()))
            .collect())
    }
}
