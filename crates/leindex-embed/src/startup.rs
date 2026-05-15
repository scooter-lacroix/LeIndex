// Startup reporting for the worker process
//
// VAL-CPHASE-009: The worker emits one startup report line containing
// execution provider, fallback reason if any, model name, quantization
// mode, warm-load latency, and chosen model path source.
//
// The startup report is logged as a single structured line at INFO level
// so the main daemon and operators can observe runtime bundle choices.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Startup report emitted by the worker on launch.
#[derive(Debug, Clone)]
pub struct StartupReport {
    /// Execution provider name (e.g., "cpu", "cuda", "rocm").
    pub execution_provider: String,
    /// Whether the requested provider was available.
    pub provider_available: bool,
    /// Reason for fallback if the requested provider was unavailable.
    pub fallback_reason: Option<String>,
    /// Model name (e.g., "qwen3-embed-0.6b").
    pub model_name: String,
    /// Quantization mode (e.g., "none", "int8", "int4").
    pub quantization_mode: String,
    /// Time taken to warm-load the model.
    pub warm_load_latency: Duration,
    /// Resolved model file path.
    pub model_path: Option<PathBuf>,
    /// How the model path was resolved ("env_override", "bundled", "user_cache").
    pub model_path_source: Option<String>,
    /// Error if model resolution failed.
    pub model_error: Option<String>,
}

impl StartupReport {
    /// Format the startup report as a single log line.
    ///
    /// VAL-CPHASE-009: Contains execution provider, fallback reason if any,
    /// model name, quantization mode, warm-load latency, and model path source.
    pub fn to_log_line(&self) -> String {
        let provider_status = if self.provider_available {
            "available".to_string()
        } else {
            format!(
                "unavailable (fallback: {})",
                self.fallback_reason.as_deref().unwrap_or("unknown")
            )
        };

        let model_info = match (&self.model_path, &self.model_path_source) {
            (Some(path), Some(source)) => format!("{} [{}]", path.display(), source),
            (Some(path), None) => path.display().to_string(),
            (None, _) => self
                .model_error
                .clone()
                .unwrap_or_else(|| "not resolved".to_string()),
        };

        format!(
            "startup_report provider={} status={} model={} quant={} warm_load={:?} path={}",
            self.execution_provider,
            provider_status,
            self.model_name,
            self.quantization_mode,
            self.warm_load_latency,
            model_info,
        )
    }

    /// Log the startup report at INFO level.
    pub fn log(&self) {
        tracing::info!("{}", self.to_log_line());
    }
}

/// Builder for constructing startup reports.
pub struct StartupReporter {
    execution_provider: String,
    provider_available: bool,
    fallback_reason: Option<String>,
    model_name: String,
    quantization_mode: String,
    warm_load_latency: Duration,
    model_path: Option<PathBuf>,
    model_path_source: Option<String>,
    model_error: Option<String>,
}

impl StartupReporter {
    /// Create a new reporter with default values.
    pub fn new() -> Self {
        Self {
            execution_provider: "cpu".to_string(),
            provider_available: true,
            fallback_reason: None,
            model_name: String::new(),
            quantization_mode: "none".to_string(),
            warm_load_latency: Duration::ZERO,
            model_path: None,
            model_path_source: None,
            model_error: None,
        }
    }

    /// Set the execution provider and its availability.
    pub fn set_execution_provider(
        &mut self,
        name: &str,
        available: bool,
        fallback_reason: Option<&str>,
    ) {
        self.execution_provider = name.to_string();
        self.provider_available = available;
        self.fallback_reason = fallback_reason.map(|s| s.to_string());
    }

    /// Set the model name.
    pub fn set_model_name(&mut self, name: &str) {
        self.model_name = name.to_string();
    }

    /// Set the quantization mode.
    pub fn set_quantization_mode(&mut self, mode: &str) {
        self.quantization_mode = mode.to_string();
    }

    /// Set the warm-load latency.
    pub fn set_warm_load_latency(&mut self, latency: Duration) {
        self.warm_load_latency = latency;
    }

    /// Set the resolved model path and its source.
    pub fn set_model_path(&mut self, path: &Path, source: &str) {
        self.model_path = Some(path.to_path_buf());
        self.model_path_source = Some(source.to_string());
        self.model_error = None;
    }

    /// Set a model resolution error.
    pub fn set_model_error(&mut self, error: &str) {
        self.model_error = Some(error.to_string());
        self.model_path = None;
        self.model_path_source = None;
    }

    /// Build the final startup report.
    pub fn build(self) -> StartupReport {
        StartupReport {
            execution_provider: self.execution_provider,
            provider_available: self.provider_available,
            fallback_reason: self.fallback_reason,
            model_name: self.model_name,
            quantization_mode: self.quantization_mode,
            warm_load_latency: self.warm_load_latency,
            model_path: self.model_path,
            model_path_source: self.model_path_source,
            model_error: self.model_error,
        }
    }
}

impl Default for StartupReporter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_startup_report_format_with_path() {
        let report = StartupReport {
            execution_provider: "cpu".to_string(),
            provider_available: true,
            fallback_reason: None,
            model_name: "qwen3-embed-0.6b".to_string(),
            quantization_mode: "none".to_string(),
            warm_load_latency: Duration::from_millis(150),
            model_path: Some(PathBuf::from("/opt/leindex/models/qwen3-embed-0.6b.onnx")),
            model_path_source: Some("bundled".to_string()),
            model_error: None,
        };

        let line = report.to_log_line();
        assert!(line.contains("provider=cpu"));
        assert!(line.contains("status=available"));
        assert!(line.contains("model=qwen3-embed-0.6b"));
        assert!(line.contains("quant=none"));
        assert!(line.contains("warm_load="));
        assert!(line.contains("bundled"));
    }

    #[test]
    fn test_startup_report_format_with_fallback() {
        let report = StartupReport {
            execution_provider: "cuda".to_string(),
            provider_available: false,
            fallback_reason: Some("CUDA runtime not found".to_string()),
            model_name: "qwen3-embed-0.6b".to_string(),
            quantization_mode: "int8".to_string(),
            warm_load_latency: Duration::from_millis(200),
            model_path: Some(PathBuf::from(
                "/home/user/.leindex/models/qwen3-embed-0.6b.onnx",
            )),
            model_path_source: Some("user_cache".to_string()),
            model_error: None,
        };

        let line = report.to_log_line();
        assert!(line.contains("provider=cuda"));
        assert!(line.contains("unavailable"));
        assert!(line.contains("CUDA runtime not found"));
        assert!(line.contains("user_cache"));
    }

    #[test]
    fn test_startup_report_format_with_model_error() {
        let report = StartupReport {
            execution_provider: "cpu".to_string(),
            provider_available: true,
            fallback_reason: None,
            model_name: "qwen3-embed-0.6b".to_string(),
            quantization_mode: "none".to_string(),
            warm_load_latency: Duration::ZERO,
            model_path: None,
            model_path_source: None,
            model_error: Some("model not found".to_string()),
        };

        let line = report.to_log_line();
        assert!(line.contains("model not found"));
    }

    #[test]
    fn test_reporter_builder() {
        let mut reporter = StartupReporter::new();
        reporter.set_execution_provider("cpu", true, None);
        reporter.set_model_name("qwen3-embed-0.6b");
        reporter.set_quantization_mode("none");
        reporter.set_warm_load_latency(Duration::from_millis(100));
        reporter.set_model_path(&PathBuf::from("/opt/models/model.onnx"), "bundled");

        let report = reporter.build();
        assert_eq!(report.execution_provider, "cpu");
        assert!(report.provider_available);
        assert!(report.fallback_reason.is_none());
        assert_eq!(report.model_name, "qwen3-embed-0.6b");
        assert_eq!(report.quantization_mode, "none");
        assert_eq!(report.warm_load_latency, Duration::from_millis(100));
        assert_eq!(
            report.model_path,
            Some(PathBuf::from("/opt/models/model.onnx"))
        );
        assert_eq!(report.model_path_source, Some("bundled".to_string()));
    }

    #[test]
    fn test_reporter_builder_with_fallback() {
        let mut reporter = StartupReporter::new();
        reporter.set_execution_provider("cuda", false, Some("no CUDA driver"));
        let report = reporter.build();
        assert!(!report.provider_available);
        assert_eq!(report.fallback_reason, Some("no CUDA driver".to_string()));
    }

    #[test]
    fn test_reporter_builder_with_model_error() {
        let mut reporter = StartupReporter::new();
        reporter.set_model_error("file not found");
        let report = reporter.build();
        assert_eq!(report.model_error, Some("file not found".to_string()));
        assert!(report.model_path.is_none());
    }

    #[test]
    fn test_startup_report_log_does_not_panic() {
        let report = StartupReport {
            execution_provider: "cpu".to_string(),
            provider_available: true,
            fallback_reason: None,
            model_name: "test".to_string(),
            quantization_mode: "none".to_string(),
            warm_load_latency: Duration::ZERO,
            model_path: None,
            model_path_source: None,
            model_error: None,
        };
        // Should not panic
        report.log();
    }
}
