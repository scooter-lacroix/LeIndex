import re

with open('crates/leindex-embed/src/runtime.rs', 'r') as f:
    content = f.read()

# Replace run_onnx_embed_sub_batch padding logic
old_embed_padding = """        // Create input tensors: [batch_size, seq_len]
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
        }"""
new_embed_padding = """        let (input_ids, attention_mask) = Self::build_embed_padded_inputs(encodings, max_len);"""
if old_embed_padding in content:
    content = content.replace(old_embed_padding, new_embed_padding)
else:
    print("Could not find old_embed_padding")

# Replace embed inference logic
old_embed_inference = """        let mut session_guard = session.lock().map_err(|e| WorkerError {
            kind: ErrorKind::OnnxRuntime,
            message: format!("failed to lock ONNX session: {}", e),
        })?;

        let uses_position_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "position_ids");
        let uses_token_type_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
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
        })?;"""
new_embed_inference = """        let outputs = Self::execute_embed_inference(
            session,
            input_ids_tensor,
            attention_mask_tensor,
            position_ids_tensor,
            token_type_ids_tensor,
        )?;"""
if old_embed_inference in content:
    content = content.replace(old_embed_inference, new_embed_inference)
else:
    print("Could not find old_embed_inference")

# Replace run_onnx_rerank_sub_batch padding logic
old_rerank_padding = """        // Build input_ids and attention_mask vectors from encodings
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        // LEFT padding: Qwen3-Reranker is decoder-style \u2014 it must attend up to
        // the final real token (the assistant position). Right padding would
        // place pads after the suffix and break the position the model predicts
        // on. Real tokens go at the END, pads at the start.
        for encoding in encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let n = ids.len().min(max_len);
            for _ in 0..(max_len - n) {
                input_ids.push(0);
                attention_mask.push(0);
            }
            for i in 0..n {
                input_ids.push(ids[i] as i64);
                attention_mask.push(mask[i] as i64);
            }
        }"""
new_rerank_padding = """        let (input_ids, attention_mask) = Self::build_rerank_padded_inputs(encodings, max_len);"""
if old_rerank_padding in content:
    content = content.replace(old_rerank_padding, new_rerank_padding)
else:
    print("Could not find old_rerank_padding")


# Replace rerank inference logic
old_rerank_inference = """        let mut session_guard = session.lock().map_err(|e| WorkerError {
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
        })?;"""
new_rerank_inference = """        let outputs = Self::execute_rerank_inference(
            session,
            input_ids_tensor,
            attention_mask_tensor,
            position_ids_tensor,
        )?;"""
if old_rerank_inference in content:
    content = content.replace(old_rerank_inference, new_rerank_inference)
else:
    print("Could not find old_rerank_inference")


# Replace rerank extract logits logic
old_rerank_logits = """        let raw_logits: Vec<f32> = match shape.as_slice() {
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
        };"""
new_rerank_logits = """        let raw_logits = Self::extract_rerank_logits(shape.as_slice(), output_values, batch_size)?;"""
if old_rerank_logits in content:
    content = content.replace(old_rerank_logits, new_rerank_logits)
else:
    print("Could not find old_rerank_logits")


helpers = """
    #[cfg(feature = "onnx")]
    fn build_embed_padded_inputs(
        encodings: &[tokenizers::Encoding],
        max_len: usize,
    ) -> (Vec<i64>, Vec<i64>) {
        let batch_size = encodings.len();
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
        (input_ids, attention_mask)
    }

    #[cfg(feature = "onnx")]
    fn execute_embed_inference(
        session: &Arc<Mutex<Session>>,
        input_ids_tensor: ort::value::Tensor,
        attention_mask_tensor: ort::value::Tensor,
        position_ids_tensor: ort::value::Tensor,
        token_type_ids_tensor: ort::value::Tensor,
    ) -> Result<Vec<ort::value::DynValue>, WorkerError> {
        let mut session_guard = session.lock().map_err(|e| WorkerError {
            kind: ErrorKind::OnnxRuntime,
            message: format!("failed to lock ONNX session: {}", e),
        })?;

        let uses_position_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "position_ids");
        let uses_token_type_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        // Feed only the inputs the model declares; extras would be rejected.
        // Arms are mutually exclusive, so each tensor moves on exactly one path.
        match (uses_position_ids, uses_token_type_ids) {
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
        })
    }

    #[cfg(feature = "onnx")]
    fn build_rerank_padded_inputs(
        encodings: &[tokenizers::Encoding],
        max_len: usize,
    ) -> (Vec<i64>, Vec<i64>) {
        let batch_size = encodings.len();
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        // LEFT padding: Qwen3-Reranker is decoder-style \u2014 it must attend up to
        // the final real token (the assistant position). Right padding would
        // place pads after the suffix and break the position the model predicts
        // on. Real tokens go at the END, pads at the start.
        for encoding in encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let n = ids.len().min(max_len);
            for _ in 0..(max_len - n) {
                input_ids.push(0);
                attention_mask.push(0);
            }
            for i in 0..n {
                input_ids.push(ids[i] as i64);
                attention_mask.push(mask[i] as i64);
            }
        }
        (input_ids, attention_mask)
    }

    #[cfg(feature = "onnx")]
    fn execute_rerank_inference(
        session: &Arc<Mutex<Session>>,
        input_ids_tensor: ort::value::Tensor,
        attention_mask_tensor: ort::value::Tensor,
        position_ids_tensor: ort::value::Tensor,
    ) -> Result<Vec<ort::value::DynValue>, WorkerError> {
        let mut session_guard = session.lock().map_err(|e| WorkerError {
            kind: ErrorKind::OnnxRuntime,
            message: format!("failed to lock ONNX session for rerank: {}", e),
        })?;

        let uses_position_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "position_ids");
        if uses_position_ids {
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
        })
    }

    #[cfg(feature = "onnx")]
    fn extract_rerank_logits(
        shape: &[usize],
        output_values: Vec<f32>,
        batch_size: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        match shape {
            [n] if *n == batch_size => Ok(output_values),
            [n, 1] if *n == batch_size => Ok(output_values),
            _ => Err(WorkerError {
                kind: ErrorKind::Inference,
                message: format!(
                    "unsupported rerank output shape {:?}; expected [{}] or [{}, 1]",
                    shape, batch_size, batch_size
                ),
            }),
        }
    }
"""

anchor = "    #[cfg(feature = \"onnx\")]\n    fn run_onnx_embed_sub_batch("
content = content.replace(anchor, helpers + "\n" + anchor)

with open('crates/leindex-embed/src/runtime.rs', 'w') as f:
    f.write(content)
