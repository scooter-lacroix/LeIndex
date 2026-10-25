//! Endpoint model discovery for the remote embedding route.
//!
//! `leindex setup` needs to offer the models an endpoint *actually* serves
//! rather than a table its author guessed at. Both supported wire families
//! answer a listing request, but with different identifiers, headers and
//! paging — enough surface that it lives here rather than in `remote.rs`,
//! which is at its line budget.

use super::{Client, Deserialize, RemoteEmbeddingConfig, RemoteEmbeddingError, RemoteProvider};
use std::str;

/// One row of a provider's model listing.
///
/// The two wire families name models differently — OpenAI-compatible servers
/// send `{"id": "..."}`, the native Gemini surface sends
/// `{"name": "models/..."}` — so this untagged enum accepts either. It is
/// deliberately an enum rather than a struct with both optional fields: an
/// untagged match on two `Option`s would silently prefer `id` for a body that
/// has neither, and the failure would surface as an empty picker instead of a
/// decode error.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ListableModel {
    /// OpenAI-compatible shape (`{"id": "text-embedding-3-small"}`).
    Named {
        /// Model identifier.
        id: String,
    },
    /// Native Gemini shape (`{"name": "models/gemini-embedding-001"}`).
    Resource {
        /// Model resource path.
        name: String,
    },
}

/// A page of a provider's model listing.
#[derive(Debug, Deserialize)]
struct ModelsPage {
    /// Models on this page.
    data: Vec<ListableModel>,
    /// Cursor for the next page (native Gemini; absent on single-page lists).
    #[serde(default, alias = "nextPageToken")]
    next_page_token: Option<String>,
}

/// Parse one page of a provider model listing into bare model identifiers
/// plus the next-page cursor, if the server supplied one.
///
/// Split out from [`RemoteEmbeddingConfig::list_models`] so the decoding rules
/// — which wire shape yields which identifier, and which rows are dropped — can
/// be tested without a live endpoint.
fn parse_models_page(body: &str) -> Result<(Vec<String>, Option<String>), RemoteEmbeddingError> {
    let page: ModelsPage = serde_json::from_str(body).map_err(|error| {
        RemoteEmbeddingError::InvalidResponse(format!("model listing is not a model list: {error}"))
    })?;
    let models = page
        .data
        .into_iter()
        .map(|model| match model {
            ListableModel::Named { id } => id,
            // Gemini names each model as a resource path
            // (`models/gemini-embedding-001`); the request layer wants the
            // bare identifier, so strip the collection prefix.
            ListableModel::Resource { name } => name
                .strip_prefix("models/")
                .map_or(name.clone(), str::to_string),
        })
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    let next = page
        .next_page_token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    Ok((models, next))
}

impl RemoteEmbeddingConfig {
    /// The `{base}/models` listing URL for this route.
    ///
    /// Both supported wire families expose a model list under a different
    /// path shape, and the spelling is what decides whether the request lands:
    /// OpenAI-compatible servers serve `GET {base_url}/models`, while the
    /// native Gemini surface serves `GET {base_url}/models`, where `base_url`
    /// already carries `/v1beta`. Gemini's listing is paginated, so the page
    /// size is pinned to the documented maximum to keep a large catalogue to
    /// one round trip; the cursor is still followed by [`Self::list_models`] if
    /// a server insists on paging.
    pub fn models_url(&self) -> Option<String> {
        let path = match self.provider {
            // The `Custom` variant's endpoint is the base URL a config
            // resolved to (see [`Self::from_resolved`]); for the historical
            // `OpenAI`/`Cohere` variants the base URL is implied.
            RemoteProvider::Custom { .. } => {
                self.base_url.clone().or_else(|| match &self.provider {
                    RemoteProvider::Custom { endpoint } => Some(endpoint.clone()),
                    _ => None,
                })?
            }
            RemoteProvider::OpenAI { .. } | RemoteProvider::Cohere { .. } => {
                self.provider.default_base_url()?.to_string()
            }
            RemoteProvider::Gemini { .. } => self
                .base_url
                .clone()
                .or_else(|| self.provider.default_base_url().map(str::to_string))?,
        };
        let base = path.trim_end_matches('/');
        // A config may name either the root (`.../v1`) or the full embeddings
        // path (`.../v1/embeddings`); the listing lives one level up from the
        // latter, and asking for `/v1/embeddings/models` returns 404.
        let base = base.strip_suffix("/embeddings").unwrap_or(base);
        Some(match self.provider {
            RemoteProvider::Gemini { .. } => format!("{base}/models?pageSize=1000"),
            _ => format!("{base}/models"),
        })
    }

    /// List the models this endpoint actually serves.
    ///
    /// This is discovery, not a curated table: the identifiers come from the
    /// provider's own response, so `leindex setup` can offer real choices
    /// instead of IDs an author guessed. Endpoints that expose no listing, or
    /// that refuse unauthenticated listing, are a normal outcome rather than a
    /// failure — callers fall back to free-text entry — so this returns an
    /// `Err` with the reason rather than panic or invent rows.
    ///
    /// Deliberately non-retrying: `leindex setup` must answer promptly, and a
    /// listing that fails once will fail identically on a re-send.
    pub async fn list_models(&self) -> Result<Vec<String>, RemoteEmbeddingError> {
        let Some(url) = self.models_url() else {
            return Err(RemoteEmbeddingError::ApiKeyNotFound(
                "endpoint exposes no model listing".to_string(),
            ));
        };
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs.max(1)))
            .build()
            .map_err(|error| RemoteEmbeddingError::HttpClient(error.to_string()))?;
        let mut models = Vec::new();
        let mut next = Some(url);
        // A cursor loop needs a stop: a server that echoes the same cursor
        // forever would hang setup, and no real catalogue pages 100 times.
        let mut pages_left = 10;
        while let Some(url) = next.take() {
            if pages_left == 0 {
                break;
            }
            pages_left -= 1;
            let mut request = client.get(&url);
            // Each family reads the key from a different header; sending the
            // wrong one is an authentication failure, not a benign miss.
            if let Some(api_key) = &self.api_key {
                request = match self.provider {
                    RemoteProvider::Gemini { .. } => request.header("x-goog-api-key", api_key),
                    _ => request.bearer_auth(api_key),
                };
            }
            let response = request
                .send()
                .await
                .map_err(|error| RemoteEmbeddingError::Transport(error.to_string()))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|error| RemoteEmbeddingError::Transport(error.to_string()))?;
            if !status.is_success() {
                return Err(RemoteEmbeddingError::HttpStatus {
                    status: status.as_u16(),
                    message: body.chars().take(200).collect(),
                });
            }
            let page = parse_models_page(&body)?;
            let before = models.len();
            models.extend(page.0);
            // Paging is only meaningful when a page actually contributed rows:
            // a server that answers a cursor with an empty page is finished,
            // and looping on its cursor would issue the same request forever.
            if models.len() == before {
                break;
            }
            next = page.1.map(|token| {
                // Gemini is the only paging family here; the base URL already
                // carries the pinned page size.
                format!("{url}&pageToken={token}")
            });
        }
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_models_page_accepts_both_wire_shapes() {
        // OpenAI-compatible servers send `id`; the native Gemini surface sends
        // a `models/`-prefixed resource name. Both must reduce to the bare
        // identifier the request layer sends, or the picker would offer a
        // string that 404s.
        let openai = parse_models_page(
            r#"{"data":[
                {"id":"text-embedding-3-small","object":"model"},
                {"id":"text-embedding-3-large","object":"model"}
            ]}"#,
        )
        .expect("an OpenAI-compatible listing must parse");
        assert_eq!(
            openai.0,
            ["text-embedding-3-small", "text-embedding-3-large"]
        );
        assert_eq!(openai.1, None, "a single-page listing has no cursor");

        let gemini = parse_models_page(
            r#"{"data":[{"name":"models/gemini-embedding-001"},
                         {"name":"models/text-embedding-004"}]}"#,
        )
        .expect("a native Gemini listing must parse");
        assert_eq!(gemini.0, ["gemini-embedding-001", "text-embedding-004"]);
    }

    #[test]
    fn test_parse_models_page_surfaces_the_cursor_and_drops_blank_rows() {
        let (models, next) =
            parse_models_page(r#"{"data":[{"id":"a"},{"id":"  "}],"nextPageToken":"tok"}"#)
                .expect("a blank id must not fail the whole page");
        assert_eq!(models, ["a"], "blank identifiers are dropped, not offered");
        assert_eq!(next.as_deref(), Some("tok"));
    }

    #[test]
    fn test_parse_models_page_rejects_a_body_that_is_not_a_listing() {
        // An HTML error page or a bare object must not decode as "no models":
        // that would read to the user as an empty catalogue instead of a
        // contract mismatch.
        let error = parse_models_page(r#"{"error":"unauthorized"}"#)
            .expect_err("a non-listing body must be an error");
        assert!(
            matches!(error, RemoteEmbeddingError::InvalidResponse(_)),
            "a non-listing body must surface as InvalidResponse, got {error:?}"
        );
    }

    #[test]
    fn test_models_url_targets_the_listing_not_the_embeddings_path() {
        // `setup` records the endpoint users think in terms of, which is often
        // the full embeddings URL. Appending `/models` to that yields
        // `/v1/embeddings/models`, and every provider 404s on it.
        let mut config = RemoteEmbeddingConfig {
            base_url: Some("http://127.0.0.1:8080/v1/embeddings".to_string()),
            ..RemoteEmbeddingConfig::default()
        };
        config.provider = RemoteProvider::Custom {
            endpoint: "http://127.0.0.1:8080/v1/embeddings".to_string(),
        };
        assert_eq!(
            config.models_url().as_deref(),
            Some("http://127.0.0.1:8080/v1/models"),
            "the listing must sit one level above the embeddings path"
        );

        // Gemini pins the documented page size; the other families do not page.
        let mut gemini = RemoteEmbeddingConfig {
            base_url: Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            ..RemoteEmbeddingConfig::default()
        };
        gemini.provider = RemoteProvider::Gemini {
            model: "gemini-embedding-001".to_string(),
        };
        assert_eq!(
            gemini.models_url().as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000")
        );
    }

    /// Serve exactly one HTTP response on a loopback socket, returning its port
    /// and a receiver for the request line the client actually sent.
    fn serve_once(response_body: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut request = String::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        request.push_str(&line);
                        if line == "\r\n" {
                            break;
                        }
                    }
                }
            }
            let _ = sender.send(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (port, receiver)
    }

    fn blocking_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime")
    }

    #[test]
    fn test_list_models_requests_the_listing_with_bearer_auth() {
        let (port, request) = serve_once(r#"{"data":[{"id":"served-model"}]}"#);
        let mut config = RemoteEmbeddingConfig {
            base_url: Some(format!("http://127.0.0.1:{port}/v1")),
            api_key: Some("listing-key".to_string()),
            ..RemoteEmbeddingConfig::default()
        };
        config.provider = RemoteProvider::Custom {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
        };

        let models = blocking_runtime()
            .block_on(config.list_models())
            .expect("a served listing must be returned");
        assert_eq!(models, ["served-model"]);

        let request = request.recv().expect("the server must have been called");
        assert!(
            request.starts_with("GET /v1/models "),
            "expected the listing path, got: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer listing-key"),
            "the listing must carry the configured key, got: {request}"
        );
    }

    #[test]
    fn test_list_models_reports_a_refused_listing_instead_of_an_empty_one() {
        // A 404 must not become "no models available": the caller then cannot
        // tell "this endpoint has no catalogue" from "we asked the wrong URL".
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut line = String::new();
            while let Ok(read) = reader.read_line(&mut line) {
                if read == 0 || line == "\r\n" {
                    break;
                }
                line.clear();
            }
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno",
            );
            let _ = stream.flush();
        });

        let mut config = RemoteEmbeddingConfig {
            base_url: Some(format!("http://127.0.0.1:{port}/v1")),
            ..RemoteEmbeddingConfig::default()
        };
        config.provider = RemoteProvider::Custom {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
        };

        let error = blocking_runtime()
            .block_on(config.list_models())
            .expect_err("a refused listing must be an error, not an empty list");
        assert!(
            matches!(error, RemoteEmbeddingError::HttpStatus { status: 404, .. }),
            "a 404 must surface as HttpStatus, got {error:?}"
        );
    }
}
