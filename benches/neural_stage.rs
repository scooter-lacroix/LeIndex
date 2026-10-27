//! Opt-in neural-stage measurement, including the installed worker baseline.
//!
//! No model is loaded during ordinary tests. Example (auto-discovered test bench):
//! `LEINDEX_PERF_BACKEND=http cargo +stable test --release --bench neural_stage -- --ignored --nocapture`
//! Worker: set LEINDEX_PERF_BACKEND=worker, LEINDEX_PERF_BINARY=/path/to/leindex,
//! LEINDEX_WORKER_MODEL, LEINDEX_WORKER_EXECUTION_PROVIDER, and
//! LEINDEX_ONNX_INFERENCE_BATCH_SIZE. Worker stderr logs remain under /tmp.
//! BATCHES/WORKERS/ROWS/REPEATS/TEXT_REPEATS use the LEINDEX_PERF_ prefix.
//! Timings include transport, tokenization and output validation, not model load.
//!
//! HTTP target selection reads the REAL config: the endpoint, model, dimension
//! and credential come from `[indexing.embeddings]` — the same route the indexer
//! uses — so measuring needs NO environment at all. Configure once with
//! `leindex setup`, including the API key (the wizard can store it), then run
//! the bench. `LEINDEX_PERF_URL` / `LEINDEX_PERF_MODEL` / `LEINDEX_PERF_API_KEY_ENV`
//! exist only as OVERRIDES for pointing at a different endpoint without editing
//! the config; `LEINDEX_PERF_API_KEY_ENV` names a variable rather than carrying a
//! secret, so no credential is written to this file, a command line, or the CSV.
//!
//! Every HTTP response's `model` field is asserted against the configured model:
//! a proxy that silently routes elsewhere still returns well-shaped vectors of
//! the right width, so the shape checks below cannot distinguish the two. That
//! missing assertion is what made an earlier 128-row measurement unusable.

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use leindex::embed::protocol::{
    BatchId, EmbedRequest, Frame, FrameHeader, MsgType, Request, RerankDocument, RerankRequest,
    Response,
};

fn setting(name: &str, default: usize) -> usize {
    std::env::var(format!("LEINDEX_PERF_{name}"))
        .map(|value| value.parse().expect("positive integer setting"))
        .unwrap_or(default)
}

fn sizes(name: &str, default: &str) -> Vec<usize> {
    std::env::var(format!("LEINDEX_PERF_{name}"))
        .unwrap_or_else(|_| default.into())
        .split(',')
        .map(|v| v.parse().expect("comma-separated positive integers"))
        .inspect(|v| assert!(*v > 0))
        .collect()
}

struct Worker {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    batch_id: u64,
}

impl Worker {
    fn new(label: &str, index: usize) -> Result<Self> {
        let binary = std::env::var("LEINDEX_PERF_BINARY")
            .unwrap_or_else(|_| "/home/scooter/.cargo/bin/leindex".into());
        let log_path = format!(
            "/tmp/leindex-perf-{}-{label}-{index}.log",
            std::process::id()
        );
        let stderr = std::fs::File::create(&log_path)?;
        let mut child = Command::new(binary)
            .arg("--internal-embed-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()?;
        println!("worker_log={log_path} pid={}", child.id());
        let input = child.stdin.take().context("worker stdin")?;
        let output = BufReader::new(child.stdout.take().context("worker stdout")?);
        Ok(Self {
            child,
            input: Some(input),
            output,
            batch_id: 0,
        })
    }

    fn request(&mut self, texts: &[String], dim: usize, rerank: bool) -> Result<Vec<f32>> {
        self.batch_id += 1;
        let (msg_type, request) = if rerank {
            (
                MsgType::RerankRequest,
                Request::Rerank(RerankRequest {
                    query: "bounded neural embedding batch cancellation".into(),
                    documents: texts
                        .iter()
                        .enumerate()
                        .map(|(index, content)| RerankDocument {
                            id: index.to_string(),
                            content: content.clone(),
                            initial_score: 0.5,
                        })
                        .collect(),
                }),
            )
        } else {
            (
                MsgType::EmbedRequest,
                Request::Embed(EmbedRequest {
                    texts: texts.to_vec(),
                    expected_dim: dim,
                    cache_keys: vec![],
                }),
            )
        };
        let frame = Frame::new(
            FrameHeader {
                batch_id: BatchId(self.batch_id),
                msg_type,
            },
            &request,
        )?;
        let input = self.input.as_mut().context("closed worker stdin")?;
        input.write_all(&frame.encode_wire()?)?;
        input.flush()?;
        let mut prefix = [0; 4];
        self.output.read_exact(&mut prefix)?;
        let size = u32::from_le_bytes(prefix) as usize;
        ensure!(
            size <= 32 * 1024 * 1024,
            "oversized worker response: {size}"
        );
        let mut payload = vec![0; size];
        self.output.read_exact(&mut payload)?;
        let response = Frame::from_wire_bytes(&payload)?;
        ensure!(
            response.header.batch_id == frame.header.batch_id,
            "batch ID mismatch"
        );
        match response.decode_payload::<Response>()? {
            Response::Embed(response) if !rerank => {
                ensure!(
                    response.count == texts.len() && response.dimension == dim,
                    "wrong shape"
                );
                ensure!(
                    response.vectors.len() == texts.len() * dim,
                    "short worker output"
                );
                ensure!(
                    response.vectors.iter().all(|v| v.is_finite()),
                    "non-finite output"
                );
                Ok(response.vectors)
            }
            Response::Rerank(mut response) if rerank => {
                ensure!(response.results.len() == texts.len(), "short rerank output");
                response
                    .results
                    .sort_by_key(|r| r.id.parse::<usize>().expect("numeric ID"));
                let mut scores = Vec::with_capacity(texts.len());
                for (index, result) in response.results.into_iter().enumerate() {
                    ensure!(result.id == index.to_string(), "rerank ID mismatch");
                    ensure!(result.rerank_score.is_finite(), "non-finite rerank score");
                    scores.push(result.rerank_score);
                }
                Ok(scores)
            }
            Response::Error(error) => bail!("worker: {error}"),
            other => bail!("unexpected response {other:?}"),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    if let Err(error) = self.child.kill() {
                        eprintln!("worker cleanup kill: {error}");
                    }
                    if let Err(error) = self.child.wait() {
                        eprintln!("worker cleanup wait: {error}");
                    }
                    break;
                }
            }
        }
    }
}

enum Backend {
    Http {
        runtime: tokio::runtime::Runtime,
        client: reqwest::Client,
        url: String,
        model: String,
        api_key: Option<String>,
    },
    Worker(Worker),
}

impl Backend {
    fn new(name: &str, label: &str, index: usize) -> Result<Self> {
        if name == "worker" {
            return Ok(Self::Worker(Worker::new(label, index)?));
        }
        ensure!(name == "http", "backend must be http or worker");
        // The target comes from the REAL config — the same `[indexing.embeddings]`
        // route the indexer uses — so a measurement needs no environment at all.
        // That is the point: the endpoint, model and credential are things the
        // user configures through `leindex setup`, and a benchmark that demanded
        // an env export for each was measuring a configuration no user has.
        //
        // `LEINDEX_PERF_*` stays as an OVERRIDE for targeting a different endpoint
        // without touching the config, and only then is a model expected there.
        let cfg = leindex::config::LeIndexConfig::load().context("load leindex config")?;
        let resolved = cfg
            .indexing
            .embeddings
            .resolved_remote_with(&cfg.services)
            .context(
                "no remote embedding route resolves; configure one with `leindex setup` \
                 (or set LEINDEX_PERF_URL to override)",
            )?;
        let config_key = cfg
            .indexing
            .embeddings
            .resolved_api_key()
            .map_err(anyhow::Error::msg)?;
        // `resolved_api_key` is the config's OWN resolution (env NAME first,
        // literal token second), so the bench and the indexer cannot disagree
        // about which credential is in force.
        let api_key = match std::env::var("LEINDEX_PERF_API_KEY_ENV") {
            Ok(var) => Some(std::env::var(&var).with_context(|| {
                format!("LEINDEX_PERF_API_KEY_ENV names {var}, which is unset")
            })?),
            Err(_) => config_key,
        };
        Ok(Self::Http {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                // Diagnostic switch for stale keep-alives after serial warmups.
                .pool_max_idle_per_host(if setting("NO_POOL", 0) == 1 {
                    0
                } else {
                    usize::MAX
                })
                .build()?,
            url: std::env::var("LEINDEX_PERF_URL").unwrap_or(resolved.endpoint),
            model: std::env::var("LEINDEX_PERF_MODEL")
                .ok()
                .or(resolved.model)
                .context("the configured route names no model; set LEINDEX_PERF_MODEL")?,
            api_key,
        })
    }

    fn embed(&mut self, texts: &[String], dim: usize, rerank: bool) -> Result<Vec<f32>> {
        match self {
            Self::Worker(worker) => worker.request(texts, dim, rerank),
            Self::Http {
                runtime,
                client,
                url,
                model,
                api_key,
            } => {
                ensure!(!rerank, "HTTP harness is embedding-only");
                runtime.block_on(async {
                    let mut request = client
                        .post(url.as_str())
                        .json(&serde_json::json!({"model": model, "input": texts}));
                    if let Some(key) = api_key {
                        request = request.bearer_auth(key);
                    }
                    let response: serde_json::Value =
                        request.send().await?.error_for_status()?.json().await?;
                    // A proxy that routed elsewhere still returns well-shaped
                    // vectors, so shape checks alone cannot tell which model
                    // answered. Assert it per response.
                    ensure!(
                        response["model"].as_str() == Some(model.as_str()),
                        "endpoint echoed model {:?}, expected {:?}",
                        response["model"].as_str(),
                        model
                    );
                    let data = response["data"].as_array().context("missing data array")?;
                    ensure!(data.len() == texts.len(), "short HTTP output");
                    let mut seen = vec![false; texts.len()];
                    let mut vectors = vec![0.0; texts.len() * dim];
                    for entry in data {
                        let index = entry["index"].as_u64().context("missing index")? as usize;
                        ensure!(
                            index < texts.len() && !seen[index],
                            "invalid/duplicate index"
                        );
                        seen[index] = true;
                        let row = entry["embedding"].as_array().context("missing vector")?;
                        ensure!(row.len() == dim, "wrong HTTP dimension: {}", row.len());
                        for (offset, value) in row.iter().enumerate() {
                            let value = value.as_f64().context("non-numeric embedding")? as f32;
                            ensure!(value.is_finite(), "non-finite embedding");
                            vectors[index * dim + offset] = value;
                        }
                    }
                    Ok(vectors)
                })
            }
        }
    }
}

/// Configured vector width for the HTTP route.
///
/// The shape check below asserts that every returned vector has this width, so a
/// hardcoded guess would fail the run on any route configured for a different
/// dimension — the `gemini` preset declares 3072, a local llama-server 768.
/// Ask the config instead of guessing, and let `LEINDEX_PERF_DIM` override.
fn config_dim() -> Result<usize> {
    let cfg = leindex::config::LeIndexConfig::load().context("load leindex config")?;
    cfg.indexing
        .embeddings
        .resolved_remote_with(&cfg.services)
        .and_then(|resolved| resolved.remote_dimension)
        .context("the configured route names no dimension; set LEINDEX_PERF_DIM")
}

#[test]
#[ignore = "real model/endpoint performance run; explicitly opt in during a quiet window"]
fn test_neural_stage() -> Result<()> {
    let backend = std::env::var("LEINDEX_PERF_BACKEND").context("set LEINDEX_PERF_BACKEND")?;
    let rows = setting("ROWS", 512);
    let repeats = setting("REPEATS", 3);
    let default_dim = if backend == "http" {
        config_dim()?
    } else {
        1024
    };
    let dim = setting("DIM", default_dim);
    let rerank = std::env::var("LEINDEX_PERF_RERANK").is_ok_and(|v| v == "1");
    ensure!(
        rows > 0 && repeats > 0 && dim > 0,
        "positive settings required"
    );
    let text_repeats = setting("TEXT_REPEATS", 4);
    let corpus: Vec<String> = (0..rows)
        .map(|i| format!(
            "title: src/embed/worker_{i}.rs | process_batch\ntext: pub fn process_batch_{i}(items: &[String]) -> Result<Vec<f32>, Error> {{ {} }}",
            "let result = embed(items)?; check_cancelled()?; store_rows(&result)?; ".repeat(text_repeats)
        ))
        .collect();
    let bytes: usize = corpus.iter().map(String::len).sum();
    let digest = blake3::hash(corpus.join("\n").as_bytes());
    println!("corpus rows={rows} utf8_bytes={bytes} blake3={digest} synthetic=true");
    println!(
        "backend,batch,workers,repeat,rows,seconds,rows_per_sec,p50_request_ms,p95_request_ms"
    );
    for batch in sizes("BATCHES", "1,8,32,128,512") {
        for concurrency in sizes("WORKERS", "1,2,4") {
            let label = format!("b{batch}-w{concurrency}");
            let mut clients = Vec::new();
            for index in 0..concurrency {
                let start = Instant::now();
                let mut client = Backend::new(&backend, &label, index)?;
                let output = client.embed(&corpus[..batch.min(rows)], dim, rerank)?;
                println!(
                    "cold_to_first_response label={label} worker={index} seconds={:.6} values={}",
                    start.elapsed().as_secs_f64(),
                    output.len()
                );
                clients.push(client);
            }
            for repeat in 0..repeats {
                let barrier = Arc::new(Barrier::new(concurrency));
                let results = std::thread::scope(|scope| {
                    let handles: Vec<_> = clients
                        .iter_mut()
                        .enumerate()
                        .map(|(worker, client)| {
                            let barrier = barrier.clone();
                            let corpus = &corpus;
                            scope.spawn(move || -> Result<(f64, Vec<f64>, usize)> {
                                barrier.wait();
                                let start = Instant::now();
                                let mut latencies = Vec::new();
                                let mut count = 0;
                                for texts in corpus.chunks(batch).skip(worker).step_by(concurrency)
                                {
                                    let request_start = Instant::now();
                                    let output = client.embed(texts, dim, rerank)?;
                                    std::hint::black_box(output);
                                    latencies.push(request_start.elapsed().as_secs_f64() * 1000.0);
                                    count += texts.len();
                                }
                                Ok((start.elapsed().as_secs_f64(), latencies, count))
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| h.join().expect("benchmark thread panicked"))
                        .collect::<Result<Vec<_>>>()
                })?;
                let seconds = results.iter().map(|r| r.0).fold(0.0, f64::max);
                let count: usize = results.iter().map(|r| r.2).sum();
                ensure!(count == rows, "unprocessed rows");
                let mut latencies: Vec<_> = results.into_iter().flat_map(|r| r.1).collect();
                latencies.sort_by(f64::total_cmp);
                let percentile = |p: usize| {
                    latencies[((latencies.len() * p).div_ceil(100) - 1).min(latencies.len() - 1)]
                };
                println!(
                    "{backend},{batch},{concurrency},{repeat},{count},{seconds:.6},{:.3},{:.3},{:.3}",
                    count as f64 / seconds,
                    percentile(50),
                    percentile(95)
                );
            }
        }
    }
    Ok(())
}

#[test]
fn test_perf_sizes_are_nonzero() {
    // Keep percentile arithmetic and chunk stepping valid without loading models.
    assert!(sizes("UNSET_TEST_SIZES", "1,8,32").iter().all(|v| *v > 0));
}
