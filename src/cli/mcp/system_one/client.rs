//! HTTP `Asker` for Laya (`laya-serve`), TypeSafe Jev and OpenAI Decisions.
//! Single attempt, no retry: the gate is latency-bounded and fails open.

use super::types::{Asker, Provider, Request, Response, SystemOneError};
use super::wire::{decode, encode};
use serde_json::Value;
use std::time::Duration;

/// Default base URL per provider (Laya's `laya-serve` default port is 8000).
pub fn default_base_url(provider: Provider) -> &'static str {
    match provider {
        Provider::Laya => "http://127.0.0.1:8000",
        Provider::Jev => "https://api.typesafe.ai",
        Provider::OpenAi => "https://api.openai.com",
    }
}

/// Default model name per provider.
pub fn default_model(provider: Provider) -> &'static str {
    match provider {
        Provider::Laya => "laya",
        Provider::Jev => "jev-latest",
        Provider::OpenAi => "gpt-6-luna",
    }
}

fn endpoint_path(provider: Provider) -> &'static str {
    match provider {
        Provider::Laya | Provider::Jev => "/v1/systemone",
        Provider::OpenAi => "/v1/decisions",
    }
}

/// `reqwest`-backed provider client.
pub struct HttpAsker {
    client: reqwest::Client,
    provider: Provider,
    url: String,
    model: String,
    api_key: Option<String>,
}

impl HttpAsker {
    /// Build a client. `base_url` may omit the endpoint path.
    pub fn new(
        provider: Provider,
        base_url: &str,
        model: &str,
        api_key: Option<String>,
        timeout: Duration,
    ) -> Result<Self, SystemOneError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| SystemOneError::Transport(e.to_string()))?;
        Ok(Self {
            client,
            provider,
            url: format!(
                "{}{}",
                base_url.trim_end_matches('/'),
                endpoint_path(provider)
            ),
            model: model.to_string(),
            api_key,
        })
    }
}

impl Asker for HttpAsker {
    async fn ask(&self, request: &Request) -> Result<Response, SystemOneError> {
        let mut call =
            self.client
                .post(&self.url)
                .json(&encode(self.provider, &self.model, request));
        if let Some(key) = &self.api_key {
            call = call.bearer_auth(key);
        }
        let response = call
            .send()
            .await
            .map_err(|e| SystemOneError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(SystemOneError::Status(status.as_u16()));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| SystemOneError::Decode(e.to_string()))?;
        decode(self.provider, request, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::system_one::types::Question;
    use serde_json::json;
    use std::io::{Read, Write};

    /// Serve one canned HTTP response; returns the base URL and the captured
    /// request (headers + body) receiver. Pattern follows
    /// `remote_embed_test.rs::spawn_mock_embeddings_server`.
    fn mock_server(status: u16, body: String) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).expect("read");
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= split + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            tx.send(String::from_utf8_lossy(&buf).to_string()).ok();
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).ok();
        });
        (format!("http://{addr}"), rx)
    }

    fn request() -> Request {
        Request {
            state: json!({"q": "x"}),
            questions: vec![(
                "ok".into(),
                Question::YesNo {
                    instructions: "Is it ok?".into(),
                },
            )],
        }
    }

    #[tokio::test]
    async fn posts_jev_shape_with_bearer_and_decodes() {
        let (base, rx) = mock_server(
            200,
            json!({"answers": {"ok": {"type": "noul", "noul": 0.9}},
                   "usage": {"input_tokens": 7, "output_tokens": 1}})
            .to_string(),
        );
        let asker = HttpAsker::new(
            Provider::Jev,
            &base,
            "jev-latest",
            Some("secret".into()),
            Duration::from_secs(2),
        )
        .unwrap();
        let response = asker.ask(&request()).await.unwrap();
        assert_eq!(response.p_yes("ok"), Some(0.9));
        assert_eq!(response.input_tokens, Some(7));
        let raw = rx.recv().unwrap();
        assert!(raw.starts_with("POST /v1/systemone "), "{raw}");
        assert!(
            raw.to_ascii_lowercase()
                .contains("authorization: bearer secret")
        );
        assert!(raw.contains("\"type\":\"noul\""));
    }

    #[tokio::test]
    async fn openai_uses_decisions_path() {
        let (base, rx) = mock_server(
            200,
            json!({"answers": [{"type": "predicate", "name": "ok", "probability": 0.2}]})
                .to_string(),
        );
        let asker = HttpAsker::new(
            Provider::OpenAi,
            &base,
            "gpt-6-luna",
            None,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(asker.ask(&request()).await.unwrap().p_yes("ok"), Some(0.2));
        assert!(rx.recv().unwrap().starts_with("POST /v1/decisions "));
    }

    #[tokio::test]
    async fn non_success_status_is_an_error_not_a_panic() {
        let (base, _rx) = mock_server(429, "{}".into());
        let asker = HttpAsker::new(
            Provider::Jev,
            &base,
            "jev-latest",
            None,
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(matches!(
            asker.ask(&request()).await,
            Err(SystemOneError::Status(429))
        ));
    }

    #[tokio::test]
    async fn unreachable_endpoint_is_a_transport_error() {
        let asker = HttpAsker::new(
            Provider::Laya,
            "http://127.0.0.1:1",
            "laya",
            None,
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(matches!(
            asker.ask(&request()).await,
            Err(SystemOneError::Transport(_))
        ));
    }
}
