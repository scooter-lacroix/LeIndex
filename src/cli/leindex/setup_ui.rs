//! Interactive `leindex setup` as a single ratatui surface.
//!
//! Replaces the sequence of blocking `dialoguer` prompts with one screen that
//! shows every setup parameter at once, validates as you type, and reports the
//! value each field resolves to.
//!
//! Why one screen: the prompt chain hid the shape of what was being configured.
//! A user answering "embedding route" could not see that the route decides
//! which *later* questions exist, and could not back up to change an answer
//! without abandoning the run. Here the field list is derived from the selected
//! route, so the consequence of a choice is visible immediately.
//!
//! State lives in [`Draft`] as strings rather than typed values: the user is
//! mid-edit on every row, so a half-typed port must not fail to represent. The
//! strings are parsed once, in [`Draft::to_options`], which is the only place a
//! malformed value can be rejected — and [`Draft::validate`] surfaces the same
//! errors ahead of apply so the user sees them on the row, not after.
//!
//! This module is gated on the `tui` feature. `setup` itself only requires
//! `cli`, so a headless build keeps the dialoguer flow; see
//! [`run_interactive_flow`](super::setup::run_interactive_flow).

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::io::IsTerminal;

use super::setup_routes::{SetupOptions, route_label, route_names};
use super::{DetectedGpu, ExecutionProvider, SetupChoices, SetupError, detect_gpu_vendor};
use crate::config::LeIndexConfig;

/// Where the embeddings credential comes from. Mirrors the config's two fields:
/// `remote_api_key_env` (a variable NAME) and `remote_api_key` (the token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyMode {
    None,
    Env,
    Literal,
}

impl KeyMode {
    const ALL: [KeyMode; 3] = [KeyMode::None, KeyMode::Env, KeyMode::Literal];

    fn label(self) -> &'static str {
        match self {
            KeyMode::None => "no authentication",
            KeyMode::Env => "environment variable",
            KeyMode::Literal => "stored in config (plain text)",
        }
    }

    fn next(self) -> Self {
        let position = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[(position + 1) % Self::ALL.len()]
    }
}

/// Every editable row. `visibility` decides which ones exist for the route
/// selected above them, so switching route cannot leave a stale, invisible
/// value behind that later gets written to the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Neural,
    Route,
    Provider,
    LlamaServer,
    DownloadLlama,
    Gguf,
    Host,
    Port,
    GpuLayers,
    AutoStart,
    Endpoint,
    Model,
    Dimension,
    KeyMode,
    KeyEnv,
    KeyLiteral,
    DocTemplate,
    QueryTemplate,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Neural => "Neural embeddings",
            Field::Route => "Embedding route",
            Field::Provider => "Execution provider",
            Field::LlamaServer => "llama-server binary",
            Field::DownloadLlama => "Download llama.cpp runtime",
            Field::Gguf => "EmbeddingGemma2 GGUF",
            Field::Host => "Embedding server host",
            Field::Port => "Embedding server port",
            Field::GpuLayers => "GPU offload layers",
            Field::AutoStart => "Start server on demand",
            Field::Endpoint => "Endpoint URL",
            Field::Model => "Model",
            Field::Dimension => "Vector dimension",
            Field::KeyMode => "API key source",
            Field::KeyEnv => "API key env variable",
            Field::KeyLiteral => "API key (plain text)",
            Field::DocTemplate => "Document template",
            Field::QueryTemplate => "Query template",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Field::Neural => {
                "Off stores a TF-IDF-only config. You can re-run setup at any time to enable it."
            }
            Field::Route => {
                "The route decides which fields below exist. `keep` leaves the current embedding \
                 configuration untouched and ignores everything else on this screen."
            }
            Field::Provider => {
                "Auto resolves CUDA/MIGraphX at runtime and survives hardware changes; pick a \
                 concrete provider only to pin one."
            }
            Field::LlamaServer => {
                "Absolute path to a llama-server binary, or to a runtime directory. \
                 Mutually exclusive with downloading one."
            }
            Field::DownloadLlama => {
                "Fetch a separate, verified official llama.cpp runtime instead of using one that \
                 is already installed."
            }
            Field::Gguf => {
                "Existing EmbeddingGemma2 GGUF file. Blank downloads the pinned preset model."
            }
            Field::Host => {
                "Loopback is the safe default. Binding publicly requires an externally \
                 authenticated server; setup refuses an unauthenticated public bind."
            }
            Field::Port => "Port the managed embedding server listens on, not the MCP port.",
            Field::GpuLayers => {
                "llama.cpp offload layers. 0 selects CPU and is independent of the ONNX \
                 execution provider."
            }
            Field::AutoStart => {
                "Whether LeIndex may start the managed server when a consumer needs it and it \
                 is down."
            }
            Field::Endpoint => {
                "Include /v1 where the provider requires it. Blank uses the preset's documented \
                 endpoint, so a preset update reaches this install."
            }
            Field::Model => "Blank uses the preset's documented model. Set it only to override.",
            Field::Dimension => {
                "Blank preserves an existing route or is discovered. A wrong value fails every \
                 search with a vector-width error."
            }
            Field::KeyMode => {
                "Prefer the environment variable: a literal token is written to the config file in \
                 plain text."
            }
            Field::KeyEnv => "Name of the variable holding the key. Blank clears authentication.",
            Field::KeyLiteral => {
                "Stored as remote_api_key. It outranks the env variable, so setting this clears \
                 any env name."
            }
            Field::DocTemplate => {
                "Prefix applied to indexed documents; {text} is substituted. Asymmetric retrieval \
                 needs both sides to match."
            }
            Field::QueryTemplate => "Prefix applied to search queries; {text} is substituted.",
        }
    }

    /// Rows that exist for this draft, in display order.
    fn visible(draft: &Draft) -> Vec<Field> {
        let mut fields = vec![Field::Neural, Field::Route];
        if !draft.neural {
            return fields;
        }
        if draft.route == "keep" {
            return fields;
        }
        match draft.route.as_str() {
            "qwen3" => fields.push(Field::Provider),
            "embeddinggemma2" => {
                fields.extend([
                    Field::LlamaServer,
                    Field::DownloadLlama,
                    Field::Gguf,
                    Field::Host,
                    Field::Port,
                    Field::GpuLayers,
                    Field::AutoStart,
                ]);
            }
            _ => {
                fields.extend([
                    Field::Endpoint,
                    Field::Model,
                    Field::Dimension,
                    Field::KeyMode,
                ]);
                match draft.key_mode {
                    KeyMode::Env => fields.push(Field::KeyEnv),
                    KeyMode::Literal => fields.push(Field::KeyLiteral),
                    KeyMode::None => {}
                }
                fields.extend([Field::DocTemplate, Field::QueryTemplate]);
            }
        }
        fields
    }
}

/// Mutable setup state. Text fields are `String` because every row is editable
/// at all times; see the module docs.
#[derive(Debug, Clone)]
pub(super) struct Draft {
    neural: bool,
    route: String,
    provider: usize,
    llama_server: String,
    download_llama: String,
    gguf: String,
    host: String,
    port: String,
    gpu_layers: String,
    auto_start: bool,
    endpoint: String,
    model: String,
    dimension: String,
    key_mode: KeyMode,
    key_env: String,
    key_literal: String,
    doc: String,
    query: String,
}

impl Draft {
    /// Seed from the live config so a re-run starts from what is in force
    /// rather than from defaults, which is what made the old prompt chain
    /// silently rewrite settings a user had not been asked about.
    fn from_config(config: &LeIndexConfig) -> Self {
        let embeddings = &config.indexing.embeddings;
        // `embeddinggemma2_preset_active` is the config's own authority for "is
        // this the managed Gemma route". A previous version of this seeding also
        // required `remote_service` to be unset, which is exactly backwards: the
        // managed route is the one that SETS `remote_service = "gemma"`. That
        // made a correctly configured Gemma install render as "Custom
        // OpenAI-compatible", so applying the screen would have rewritten a good
        // config. Ask the predicate, do not re-derive it.
        let route = if embeddings.embeddinggemma2_preset_active() {
            "embeddinggemma2"
        } else if embeddings.resolved_remote_with(&config.services).is_some() {
            // Prefer the named preset when it is a recognized one, so the menu
            // highlights the provider actually in force.
            match crate::config::REMOTE_PRESETS
                .iter()
                .find(|preset| preset.name == embeddings.remote_provider)
            {
                Some(preset) => preset.name,
                None => "remote",
            }
        } else {
            "qwen3"
        };
        let key_mode = if embeddings.remote_api_key.is_some() {
            KeyMode::Literal
        } else if embeddings.remote_api_key_env.is_some() {
            KeyMode::Env
        } else {
            KeyMode::None
        };
        Self {
            neural: embeddings.enable_hybrid,
            route: route.to_string(),
            provider: 0,
            llama_server: String::new(),
            download_llama: String::new(),
            gguf: String::new(),
            host: "127.0.0.1".to_string(),
            port: "8080".to_string(),
            gpu_layers: String::new(),
            auto_start: true,
            endpoint: embeddings.remote_endpoint.clone().unwrap_or_default(),
            model: embeddings.remote_model.clone().unwrap_or_default(),
            dimension: embeddings
                .remote_dimension
                .map(|dimension| dimension.to_string())
                .unwrap_or_default(),
            key_mode,
            key_env: embeddings.remote_api_key_env.clone().unwrap_or_default(),
            // Never seed a stored secret into an editable row: it would be
            // rendered on screen and re-written on apply even if untouched.
            key_literal: String::new(),
            doc: embeddings.remote_doc_template.clone().unwrap_or_default(),
            query: embeddings.remote_query_template.clone().unwrap_or_default(),
        }
    }

    fn provider_options() -> Vec<(String, ExecutionProvider)> {
        let mut entries = vec![
            (
                "Auto (detects CUDA/MIGraphX at runtime)".to_string(),
                ExecutionProvider::Auto,
            ),
            ("CPU (works everywhere)".to_string(), ExecutionProvider::Cpu),
            ("NVIDIA CUDA".to_string(), ExecutionProvider::Cuda),
        ];
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            entries.push((
                "AMD MIGraphX (ROCm)".to_string(),
                ExecutionProvider::Migraphx,
            ));
        }
        entries
    }

    fn routes() -> Vec<String> {
        route_names()
            .iter()
            .map(|route| (*route).to_string())
            .collect()
    }

    /// The rendered value of one row.
    fn value(&self, field: Field) -> String {
        match field {
            Field::Neural => yes_no(self.neural),
            Field::Route => route_label(&self.route),
            Field::Provider => Self::provider_options()[self.provider].0.clone(),
            Field::LlamaServer => or_dash(&self.llama_server),
            Field::DownloadLlama => {
                if self.download_llama.is_empty() {
                    "none".to_string()
                } else {
                    self.download_llama.clone()
                }
            }
            Field::Gguf => or_default(&self.gguf, "download the pinned preset"),
            Field::Host => self.host.clone(),
            Field::Port => self.port.clone(),
            Field::GpuLayers => or_default(&self.gpu_layers, "server default"),
            Field::AutoStart => yes_no(self.auto_start),
            Field::Endpoint => or_default(&self.endpoint, "preset endpoint"),
            Field::Model => or_default(&self.model, "preset model"),
            Field::Dimension => or_default(&self.dimension, "preserve or discover"),
            Field::KeyMode => self.key_mode.label().to_string(),
            Field::KeyEnv => or_default(&self.key_env, "(unset)"),
            Field::KeyLiteral => {
                if self.key_literal.is_empty() {
                    "(not set)".to_string()
                } else {
                    // Never echo a secret back in full.
                    let visible: String = self.key_literal.chars().take(4).collect();
                    format!("{visible}… ({} chars)", self.key_literal.chars().count())
                }
            }
            Field::DocTemplate => or_default(&self.doc, "raw text"),
            Field::QueryTemplate => or_default(&self.query, "raw text"),
        }
    }

    /// Advance a choice/toggle row, or return false when the row is text.
    fn cycle(&mut self, field: Field, forward: bool) -> bool {
        match field {
            Field::Neural => {
                self.neural = !self.neural;
                true
            }
            Field::AutoStart => {
                self.auto_start = !self.auto_start;
                true
            }
            Field::Route => {
                let routes = Self::routes();
                let current = routes.iter().position(|r| *r == self.route).unwrap_or(0);
                let next = step(current, routes.len(), forward);
                self.route = routes[next].clone();
                true
            }
            Field::Provider => {
                let count = Self::provider_options().len();
                self.provider = step(self.provider, count, forward);
                true
            }
            Field::KeyMode => {
                self.key_mode = if forward {
                    self.key_mode.next()
                } else {
                    let position = KeyMode::ALL
                        .iter()
                        .position(|mode| *mode == self.key_mode)
                        .unwrap_or(0);
                    KeyMode::ALL[(position + KeyMode::ALL.len() - 1) % KeyMode::ALL.len()]
                };
                true
            }
            Field::DownloadLlama => {
                const OPTIONS: [&str; 4] = ["", "cpu", "vulkan", "cuda"];
                let current = OPTIONS
                    .iter()
                    .position(|option| *option == self.download_llama)
                    .unwrap_or(0);
                self.download_llama = OPTIONS[step(current, OPTIONS.len(), forward)].to_string();
                true
            }
            _ => false,
        }
    }

    /// The text buffer a text row edits.
    fn text_mut(&mut self, field: Field) -> Option<&mut String> {
        match field {
            Field::LlamaServer => Some(&mut self.llama_server),
            Field::Gguf => Some(&mut self.gguf),
            Field::Host => Some(&mut self.host),
            Field::Port => Some(&mut self.port),
            Field::GpuLayers => Some(&mut self.gpu_layers),
            Field::Endpoint => Some(&mut self.endpoint),
            Field::Model => Some(&mut self.model),
            Field::Dimension => Some(&mut self.dimension),
            Field::KeyEnv => Some(&mut self.key_env),
            Field::KeyLiteral => Some(&mut self.key_literal),
            Field::DocTemplate => Some(&mut self.doc),
            Field::QueryTemplate => Some(&mut self.query),
            _ => None,
        }
    }

    /// Reject values that would produce an unusable config, named per row so the
    /// error appears next to the field the user must fix.
    fn validate(&self, field: Field) -> Option<String> {
        match field {
            Field::Port => match self.port.trim().parse::<u16>() {
                Ok(0) => Some("must be between 1 and 65535".to_string()),
                Ok(_) => None,
                Err(_) => Some("not a port number".to_string()),
            },
            Field::Dimension => {
                let value = self.dimension.trim();
                if value.is_empty() {
                    return None;
                }
                match value.parse::<usize>() {
                    Ok(0) => Some("dimension must be nonzero".to_string()),
                    Ok(_) => None,
                    Err(_) => Some("not a number".to_string()),
                }
            }
            Field::GpuLayers => {
                let value = self.gpu_layers.trim();
                if value.is_empty() || value.eq_ignore_ascii_case("auto") || value == "all" {
                    return None;
                }
                match value.parse::<u32>() {
                    Ok(_) => None,
                    Err(_) => Some("expected an integer, 'auto', or 'all'".to_string()),
                }
            }
            Field::Endpoint => {
                let value = self.endpoint.trim();
                if value.is_empty() {
                    return None;
                }
                // Setup refuses query strings and fragments: they leak into
                // logs and indicate a copy-pasted URL carrying a credential.
                if value.contains('?') {
                    return Some("drop the query string".to_string());
                }
                if value.contains('#') {
                    return Some("drop the fragment".to_string());
                }
                if !value.starts_with("http://") && !value.starts_with("https://") {
                    return Some("must start with http:// or https://".to_string());
                }
                None
            }
            Field::KeyEnv => {
                let value = self.key_env.trim();
                if value.is_empty() {
                    return None;
                }
                if value.contains(|c: char| c.is_whitespace() || c == '=') {
                    return Some("a variable NAME cannot contain spaces or '='".to_string());
                }
                None
            }
            _ => None,
        }
    }

    /// True when both managed-server sources are set, which setup rejects as
    /// ambiguous rather than silently preferring one.
    fn conflict(&self) -> Option<String> {
        if self.route == "embeddinggemma2"
            && !self.llama_server.trim().is_empty()
            && !self.download_llama.trim().is_empty()
        {
            return Some(
                "llama-server path and download are mutually exclusive; clear one".to_string(),
            );
        }
        None
    }

    /// Whether the route needs a credential at all, for the apply-time warning.
    fn key_missing(&self) -> bool {
        let remote = self.route == "remote"
            || crate::config::REMOTE_PRESETS
                .iter()
                .any(|preset| preset.name == self.route);
        if !remote {
            return false;
        }
        match self.key_mode {
            KeyMode::None => false,
            KeyMode::Env => self.key_env.trim().is_empty(),
            KeyMode::Literal => self.key_literal.trim().is_empty(),
        }
    }

    /// Build the options the existing (tested) setup path already understands.
    /// Nothing here bypasses `configure_external`: this only fills the same
    /// `SetupOptions` the flags would.
    fn to_options(&self) -> Result<SetupOptions, SetupError> {
        let parse_port = |value: &str| -> Result<Option<u16>, SetupError> {
            match value.trim() {
                "" => Ok(None),
                raw => raw
                    .parse::<u16>()
                    .map(Some)
                    .map_err(|_| SetupError::Conflict {
                        message: format!("invalid port {raw:?}"),
                    }),
            }
        };
        let parse_dimension = |value: &str| -> Result<Option<usize>, SetupError> {
            match value.trim() {
                "" => Ok(None),
                raw => raw
                    .parse::<usize>()
                    .map(Some)
                    .map_err(|_| SetupError::Conflict {
                        message: format!("invalid dimension {raw:?}"),
                    }),
            }
        };
        let blank_to_none = |value: &str| -> Option<String> {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        Ok(SetupOptions {
            embedding: Some(self.route.clone()).filter(|route| route != "keep"),
            llama_server: blank_to_none(&self.llama_server).map(std::path::PathBuf::from),
            download_llama: blank_to_none(&self.download_llama),
            gguf: blank_to_none(&self.gguf).map(std::path::PathBuf::from),
            embedding_host: blank_to_none(&self.host),
            embedding_port: parse_port(&self.port)?,
            gpu_layers: match self.gpu_layers.trim() {
                "" => None,
                raw => Some(raw.parse::<u32>().map_err(|_| SetupError::Conflict {
                    message: format!("invalid GPU layer count {raw:?}"),
                })?),
            },
            auto_start: Some(self.auto_start),
            embedding_endpoint: blank_to_none(&self.endpoint),
            embedding_model: blank_to_none(&self.model),
            embedding_dimension: parse_dimension(&self.dimension)?,
            // One credential field is written per apply, never both: a literal
            // token outranks the env name, so setting both would make the
            // config disagree with itself.
            embedding_api_key_env: match self.key_mode {
                KeyMode::Env => Some(self.key_env.trim().to_string()),
                KeyMode::Literal => Some(String::new()),
                KeyMode::None => Some(String::new()),
            },
            embedding_api_key: match self.key_mode {
                KeyMode::Literal => blank_to_none(&self.key_literal),
                _ => None,
            },
            embedding_doc_template: (self.route != "embeddinggemma2")
                .then(|| self.doc.trim().to_string()),
            embedding_query_template: (self.route != "embeddinggemma2")
                .then(|| self.query.trim().to_string()),
            system_one: false,
            interactive: true,
            ..Default::default()
        })
    }

    fn to_choices(&self) -> Result<SetupChoices, SetupError> {
        Ok(SetupChoices {
            neural_enabled: self.neural,
            provider: (self.route == "qwen3").then(|| Self::provider_options()[self.provider].1),
            remote_gemma: self.route == "embeddinggemma2",
            options: self.to_options()?,
        })
    }
}

fn step(current: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    if forward {
        (current + 1) % count
    } else {
        (current + count - 1) % count
    }
}

fn yes_no(value: bool) -> String {
    if value { "yes".into() } else { "no".into() }
}

fn or_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

/// `blank` shown when empty, so a row never renders as nothing and read as a
/// rendering bug.
fn or_default(value: &str, blank: &str) -> String {
    if value.trim().is_empty() {
        format!("({blank})")
    } else {
        value.to_string()
    }
}

/// Application state for the screen.
struct App {
    draft: Draft,
    fields: Vec<Field>,
    selected: usize,
    editing: Option<String>,
    message: Option<String>,
    should_quit: bool,
    accepted: bool,
    detected: DetectedGpu,
    expanded: bool,
}

impl App {
    fn new(config: &LeIndexConfig) -> Self {
        let draft = Draft::from_config(config);
        let fields = Field::visible(&draft);
        Self {
            draft,
            fields,
            selected: 0,
            editing: None,
            message: None,
            should_quit: false,
            accepted: false,
            detected: detect_gpu_vendor(),
            expanded: false,
        }
    }

    /// Recompute the visible rows after a change that alters them (route,
    /// neural, key mode) and keep the cursor on the row the user was on.
    fn refresh_fields(&mut self) {
        let current = self.fields.get(self.selected).copied();
        self.fields = Field::visible(&self.draft);
        self.selected = current
            .and_then(|field| self.fields.iter().position(|candidate| *candidate == field))
            .unwrap_or_else(|| self.selected.min(self.fields.len().saturating_sub(1)));
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.fields.is_empty() {
            return;
        }
        let count = self.fields.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(count);
        self.selected = next as usize;
    }

    fn selected_field(&self) -> Option<Field> {
        self.fields.get(self.selected).copied()
    }

    fn handle(&mut self, key: KeyEvent) {
        // While editing, keys belong to the buffer.
        if let Some(buffer) = self.editing.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let text = buffer.clone();
                    if let Some(field) = self.selected_field() {
                        if let Some(slot) = self.draft.text_mut(field) {
                            *slot = text;
                        }
                    }
                    self.editing = None;
                    self.refresh_fields();
                }
                KeyCode::Esc => self.editing = None,
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Char(character) => buffer.push(character),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('e') => self.expanded = !self.expanded,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.accept();
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Tab => self.move_cursor(1),
            KeyCode::Left => self.cycle(false),
            KeyCode::Right => self.cycle(true),
            KeyCode::Enter => self.begin_edit(),
            KeyCode::Esc => self.should_quit = true,
            _ => {}
        }
    }

    fn cycle(&mut self, forward: bool) {
        let Some(field) = self.selected_field() else {
            return;
        };
        if self.draft.cycle(field, forward) {
            self.message = None;
            self.refresh_fields();
        }
    }

    fn begin_edit(&mut self) {
        let Some(field) = self.selected_field() else {
            return;
        };
        if let Some(slot) = self.draft.text_mut(field) {
            self.editing = Some(slot.clone());
        } else {
            self.cycle(true);
        }
    }

    fn accept(&mut self) {
        if let Some(conflict) = self.draft.conflict() {
            self.message = Some(conflict);
            return;
        }
        let invalid = self
            .fields
            .iter()
            .find_map(|field| self.draft.validate(*field).map(|error| (*field, error)));
        if let Some((field, error)) = invalid {
            self.selected = self
                .fields
                .iter()
                .position(|candidate| *candidate == field)
                .unwrap_or(0);
            self.message = Some(format!("{}: {error}", field.label()));
            return;
        }
        self.accepted = true;
        self.should_quit = true;
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    if app.expanded {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(4),
                Constraint::Length(3),
            ])
            .split(frame.area());
        draw_header(frame, app, areas[0]);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(areas[1]);
        draw_fields(frame, app, columns[0]);
        draw_status(frame, app, columns[1]);
        draw_help(frame, app, areas[2]);
        draw_footer(frame, app, areas[3]);
        return;
    }

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(4),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, app, areas[0]);
    draw_fields(frame, app, areas[1]);
    draw_help(frame, app, areas[2]);
    draw_footer(frame, app, areas[3]);
}

fn draw_header(frame: &mut ratatui::Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let detection = match app.detected {
        DetectedGpu::Amd => "AMD GPU / ROCm visible",
        DetectedGpu::Nvidia => "NVIDIA GPU / CUDA visible",
        DetectedGpu::Unknown => "no AMD/NVIDIA tooling detected",
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "LeIndex setup",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("row {}/{} ", app.selected + 1, app.fields.len().max(1)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                if app.expanded { "expanded" } else { "compact" },
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![Span::styled(
            format!("host: {detection}"),
            Style::default().fg(Color::DarkGray),
        )]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_fields(frame: &mut ratatui::Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    // One row per field, two rows of chrome; keep the selection in view.
    let visible_rows = area.height.saturating_sub(2) as usize;
    let offset = if app.selected >= visible_rows {
        app.selected + 1 - visible_rows
    } else {
        0
    };
    let mut lines: Vec<Line<'_>> = Vec::new();
    for (index, field) in app
        .fields
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_rows.max(1))
    {
        let selected = index == app.selected;
        let editing = selected && app.editing.is_some();
        let value = match (editing, &app.editing) {
            (true, Some(buffer)) => format!("{buffer}▏"),
            _ => app.draft.value(*field),
        };
        let error = app.draft.validate(*field);
        let marker = if selected { "▸ " } else { "  " };
        let label_style = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let value_style = match &error {
            Some(_) => Style::default().fg(Color::Red),
            None if editing => Style::default().fg(Color::Yellow),
            None => Style::default().fg(Color::White),
        };
        lines.push(Line::from(vec![
            Span::raw(marker),
            Span::styled(format!("{:<28}", field.label()), label_style),
            Span::styled(value, value_style),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(" settings ")),
        ),
        area,
    );
}

fn draw_status(frame: &mut ratatui::Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let route = route_label(&app.draft.route);
    let neural = yes_no(app.draft.neural);
    let auth = match app.draft.key_mode {
        KeyMode::None => "none",
        KeyMode::Env => {
            if app.draft.key_env.trim().is_empty() {
                "env (unset)"
            } else {
                "env"
            }
        }
        KeyMode::Literal => {
            if app.draft.key_literal.trim().is_empty() {
                "literal (unset)"
            } else {
                "literal (redacted)"
            }
        }
    };
    let managed = app.draft.route == "embeddinggemma2";
    let lines = vec![
        Line::from(Span::styled(
            "resolved status",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("neural: {neural}")),
        Line::from(format!("route: {route}")),
        Line::from(format!("credential: {auth}")),
        Line::from(format!(
            "managed service: {}",
            if managed { "llama.cpp" } else { "not selected" }
        )),
        Line::from("capabilities"),
        Line::from("  editable here: embedding route + runtime"),
        Line::from("  CLI/config-only: search, fragments, MCP, System One"),
        Line::from("  diagnostics: use `leindex setup --check`"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" status ")),
        area,
    );
}

fn draw_help(frame: &mut ratatui::Frame<'_>, app: &App, area: ratatui::layout::Rect) {
    let help = match app.selected_field() {
        Some(field) => format!("{}: {}", field.label(), field.help()),
        None => "No editable parameters for this route.".to_string(),
    };
    // The message replaces help: an error the user must act on outranks
    // explanation of a row they are no longer looking at.
    let (text, style) = match &app.message {
        Some(message) => (message.clone(), Style::default().fg(Color::Red)),
        None => (help, Style::default().fg(Color::Gray)),
    };
    frame.render_widget(
        Paragraph::new(text)
            .style(style)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" help ")),
        area,
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, _app: &App, area: ratatui::layout::Rect) {
    let keys =
        "↑↓ move   ←→ change   enter edit/commit   e expand   esc cancel   ctrl+s apply   q quit";
    frame.render_widget(
        Paragraph::new(keys)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// Terminal modes as a drop guard, so an error on the way out cannot strand the
/// terminal in raw mode. Same contract as the `leindex tui` guard.
struct Guard {
    fullscreen: bool,
}

impl Guard {
    fn enter(fullscreen: bool) -> Result<Self, SetupError> {
        enable_raw_mode().map_err(|error| SetupError::Interactive(error.to_string()))?;
        if fullscreen {
            if let Err(error) = execute!(std::io::stdout(), EnterAlternateScreen) {
                let _ = disable_raw_mode();
                return Err(SetupError::Interactive(error.to_string()));
            }
        }
        Ok(Self { fullscreen })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.fullscreen {
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
        } else {
            let _ = execute!(std::io::stdout(), cursor::Show);
        }
        let _ = disable_raw_mode();
    }
}

/// Whether this build and this terminal can host the screen.
///
/// Both halves matter: a piped stdin has no keystrokes to read, and starting raw
/// mode on it fails much later with an error that names nothing useful.
pub(super) fn available() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Run the setup screen, returning the choices the existing apply path consumes.
pub(super) fn run(config: &LeIndexConfig, fullscreen: bool) -> Result<SetupChoices, SetupError> {
    let _guard = Guard::enter(fullscreen)?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)
        .map_err(|error| SetupError::Interactive(format!("terminal init failed: {error}")))?;

    let mut app = App::new(config);
    loop {
        terminal
            .draw(|frame| draw(frame, &app))
            .map_err(|error| SetupError::Interactive(format!("draw failed: {error}")))?;
        if app.should_quit {
            break;
        }
        match crossterm::event::read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => app.handle(key),
            Ok(_) => {}
            Err(error) => {
                return Err(SetupError::Interactive(format!("input failed: {error}")));
            }
        }
    }

    if !app.accepted {
        return Err(SetupError::Interactive("setup cancelled".to_string()));
    }
    if app.draft.key_missing() {
        // Not fatal: an unauthenticated endpoint is legitimate. Say so once, to
        // stderr after the alternate screen is gone, so it is not lost.
        eprintln!(
            "note: the selected route has no API key; searches will fail until one is configured"
        );
    }
    app.draft.to_choices()
}

#[cfg(test)]
mod test {
    use super::*;

    /// A draft seeded from the defaults, then pointed at a given route.
    fn draft_for(route: &str) -> Draft {
        let mut draft = Draft::from_config(&LeIndexConfig::default());
        draft.route = route.to_string();
        draft.neural = true;
        draft
    }

    /// Expanding is a view change only: it must not rebuild the draft, move the
    /// cursor, or discard an in-progress text edit.
    #[test]
    fn test_expanded_view_preserves_current_edits() {
        let mut app = App::new(&LeIndexConfig::default());
        app.draft.route = "embeddinggemma2".to_string();
        app.refresh_fields();
        app.selected = app
            .fields
            .iter()
            .position(|field| *field == Field::Port)
            .expect("managed route has a port row");
        app.draft.port = "43123".to_string();
        let before = app.draft.clone();

        app.handle(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        assert!(app.expanded);
        assert_eq!(app.draft.route, before.route);
        assert_eq!(app.draft.port, before.port);
        assert_eq!(app.selected_field(), Some(Field::Port));
        assert!(app.editing.is_none());
    }

    /// The route decides which rows exist. If this drifts, the screen offers a
    /// field the apply path ignores, or withholds one the user must set.
    #[test]
    fn test_visible_fields_follow_the_selected_route() {
        let remote = Field::visible(&draft_for("gemini"));
        assert!(remote.contains(&Field::Endpoint));
        assert!(remote.contains(&Field::Dimension));
        // A managed-server field must not appear on a remote route: it belongs
        // to a different route and would be silently ignored.
        assert!(!remote.contains(&Field::Port));

        let managed = Field::visible(&draft_for("embeddinggemma2"));
        assert!(managed.contains(&Field::Port));
        assert!(managed.contains(&Field::LlamaServer));
        assert!(!managed.contains(&Field::Endpoint));

        let local = Field::visible(&draft_for("qwen3"));
        assert_eq!(local, vec![Field::Neural, Field::Route, Field::Provider]);

        // `keep` writes nothing, so nothing may be offered.
        let keep = Field::visible(&draft_for("keep"));
        assert_eq!(keep, vec![Field::Neural, Field::Route]);
    }

    /// Switching neural off collapses the screen to the one row that matters:
    /// everything below it becomes unreachable rather than merely ignored.
    #[test]
    fn test_disabling_neural_hides_every_embedding_field() {
        let mut draft = draft_for("gemini");
        draft.neural = false;
        assert_eq!(
            Field::visible(&draft),
            vec![Field::Neural, Field::Route],
            "neural off must not leave embedding rows on screen"
        );
    }

    /// `remote_api_key` outranks `remote_api_key_env` at request time, so
    /// writing both would make the config disagree with itself. Exactly one
    /// credential field may be populated per apply.
    #[test]
    fn test_only_one_credential_source_is_written() {
        let mut draft = draft_for("gemini");

        draft.key_mode = KeyMode::Literal;
        draft.key_literal = "pasted-token".to_string();
        let options = draft.to_options().expect("literal key options");
        assert_eq!(options.embedding_api_key.as_deref(), Some("pasted-token"));
        assert_eq!(
            options.embedding_api_key_env.as_deref(),
            Some(""),
            "a literal token must clear the env name so one source remains"
        );

        draft.key_mode = KeyMode::Env;
        draft.key_env = "GEMINI_API_KEY".to_string();
        let options = draft.to_options().expect("env key options");
        assert_eq!(options.embedding_api_key, None);
        assert_eq!(
            options.embedding_api_key_env.as_deref(),
            Some("GEMINI_API_KEY")
        );
    }

    /// An unparseable port or dimension must be rejected with the row named,
    /// not applied as a default the user never chose.
    #[test]
    fn test_invalid_values_are_rejected_per_row() {
        let mut draft = draft_for("embeddinggemma2");
        draft.port = "not-a-port".to_string();
        assert!(draft.validate(Field::Port).is_some());

        let mut draft = draft_for("gemini");
        draft.dimension = "0".to_string();
        assert!(draft.validate(Field::Dimension).is_some());
        draft.dimension = "1536".to_string();
        assert!(draft.validate(Field::Dimension).is_none());

        // A query string is how a copy-pasted URL leaks a credential into a
        // config file and every log line that prints the endpoint.
        draft.endpoint = "https://example.test/v1?key=secret".to_string();
        assert!(draft.validate(Field::Endpoint).is_some());
        draft.endpoint = "https://example.test/v1".to_string();
        assert!(draft.validate(Field::Endpoint).is_none());
    }

    /// Both managed-server sources at once is ambiguous; the screen refuses
    /// instead of silently preferring one.
    #[test]
    fn test_managed_server_sources_conflict() {
        let mut draft = draft_for("embeddinggemma2");
        draft.llama_server = "/opt/llama-server".to_string();
        draft.download_llama = "cuda".to_string();
        assert!(draft.conflict().is_some());
        draft.download_llama.clear();
        assert!(draft.conflict().is_none());
    }

    /// A route that needs a credential and has none is a warning, not a
    /// refusal: an unauthenticated endpoint is legitimate.
    #[test]
    fn test_missing_key_warns_only_for_authenticated_routes() {
        let mut draft = draft_for("gemini");
        draft.key_mode = KeyMode::None;
        assert!(!draft.key_missing(), "no auth requested is not a gap");

        draft.key_mode = KeyMode::Env;
        draft.key_env.clear();
        assert!(draft.key_missing(), "env mode with no name is a gap");

        draft.key_mode = KeyMode::Literal;
        draft.key_literal.clear();
        assert!(draft.key_missing(), "literal mode with no token is a gap");

        // A local route never needs a credential at all.
        assert!(!draft_for("qwen3").key_missing());
    }

    /// A managed Gemma install must seed as the Gemma route, not as a generic
    /// remote endpoint.
    ///
    /// This is a regression guard: an earlier version required `remote_service`
    /// to be UNSET to select the Gemma route, but the managed route is precisely
    /// the one that sets it. A correctly configured install therefore rendered
    /// as "Custom OpenAI-compatible" and applying the screen would have
    /// rewritten a good config.
    #[test]
    fn test_managed_gemma_config_seeds_the_gemma_route() {
        let mut config = LeIndexConfig::default();
        config.indexing.embeddings.remote_provider = "embeddinggemma-2".to_string();
        config.indexing.embeddings.remote_service = Some("gemma".to_string());
        config.indexing.embeddings.enable_hybrid = true;
        let draft = Draft::from_config(&config);
        assert_eq!(
            draft.route, "embeddinggemma2",
            "a managed Gemma install must not seed as a custom remote route"
        );
        // The managed rows, not the remote rows, must be the ones on screen.
        let fields = Field::visible(&draft);
        assert!(fields.contains(&Field::Port));
        let options = draft.to_options().expect("managed gemma options");
        assert_eq!(options.embedding.as_deref(), Some("embeddinggemma2"));
        assert!(options.embedding_doc_template.is_none());
        assert!(options.embedding_query_template.is_none());
    }

    /// A remote route keeps its template controls because they are meaningful
    /// external-provider overrides.
    #[test]
    fn test_remote_route_emits_template_options() {
        let draft = draft_for("gemini");
        let options = draft.to_options().expect("remote options");
        assert_eq!(options.embedding_doc_template.as_deref(), Some(""));
        assert_eq!(options.embedding_query_template.as_deref(), Some(""));
    }

    /// Nothing may preseed the secret buffer: rendering a stored token and
    /// re-writing it untouched is how a secret ends up displayed and duplicated.
    #[test]
    fn test_a_stored_token_is_never_seeded_into_the_editable_row() {
        let mut config = LeIndexConfig::default();
        config.indexing.embeddings.remote_api_key = Some("stored-secret".to_string());
        let draft = Draft::from_config(&config);
        assert!(
            draft.key_literal.is_empty(),
            "the token must not be pre-filled"
        );
        assert_eq!(
            draft.key_mode,
            KeyMode::Literal,
            "but the mode must reflect it"
        );
        // The rendered row shows a redacted hint, never the value.
        assert!(!draft.value(Field::KeyLiteral).contains("stored-secret"));
    }
}
