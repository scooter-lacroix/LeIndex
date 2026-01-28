// Integration and unit tests for leparse

use crate::traits::LanguageConfig;

#[cfg(test)]
mod language_detection_tests {
    use super::*;

    #[test]
    fn test_python_extension_detection() {
        let config = LanguageConfig::from_extension("py");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "Python");
    }

    #[test]
    fn test_javascript_extension_detection() {
        let config = LanguageConfig::from_extension("js");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "JavaScript");

        let config = LanguageConfig::from_extension("jsx");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "JavaScript");
    }

    #[test]
    fn test_typescript_extension_detection() {
        let config = LanguageConfig::from_extension("ts");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "TypeScript");

        let config = LanguageConfig::from_extension("tsx");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "TypeScript");
    }

    #[test]
    fn test_c_extension_detection() {
        let config = LanguageConfig::from_extension("c");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "C");

        let config = LanguageConfig::from_extension("h");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "C");
    }

    #[test]
    fn test_bash_extension_detection() {
        let config = LanguageConfig::from_extension("sh");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "Bash");

        let config = LanguageConfig::from_extension("bash");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "Bash");
    }

    #[test]
    fn test_json_extension_detection() {
        let config = LanguageConfig::from_extension("json");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "JSON");
    }

    #[test]
    fn test_go_extension_detection() {
        let config = LanguageConfig::from_extension("go");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "Go");
    }

    #[test]
    fn test_rust_extension_detection() {
        let config = LanguageConfig::from_extension("rs");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "Rust");
    }

    #[test]
    fn test_case_insensitive_extension_detection() {
        let config = LanguageConfig::from_extension("PY");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "Python");

        let config = LanguageConfig::from_extension("Js");
        assert!(config.is_some());
        assert_eq!(config.unwrap().name, "JavaScript");
    }

    #[test]
    fn test_unknown_extension_returns_none() {
        let config = LanguageConfig::from_extension("unknown");
        assert!(config.is_none());

        let config = LanguageConfig::from_extension("");
        assert!(config.is_none());
    }

    #[test]
    fn test_language_config_extensions() {
        let py_config = LanguageConfig::from_extension("py").unwrap();
        assert!(py_config.extensions.contains(&"py".to_string()));
        assert_eq!(py_config.extensions.len(), 1);

        let js_config = LanguageConfig::from_extension("js").unwrap();
        assert!(js_config.extensions.contains(&"js".to_string()));
        assert!(js_config.extensions.contains(&"jsx".to_string()));
    }
}
