use super::*;
use std::sync::Arc;

#[test]
fn parse_cargo_lock() {
    let content = r#"
[[package]]
name = "serde"
version = "1.0.210"

[[package]]
name = "serde_json"
version = "1.0.128"

[[package]]
name = "tokio"
version = "1.40.0"
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_cargo_lock(content);

    assert_eq!(registry.len(), 3);
    let serde = registry.resolve("serde").unwrap();
    assert_eq!(serde.name, "serde");
    assert_eq!(serde.version, "1.0.210");
    assert_eq!(serde.ecosystem, Ecosystem::Cargo);

    let serde_json = registry.resolve("serde_json").unwrap();
    assert_eq!(serde_json.name, "serde_json");
    assert_eq!(serde_json.version, "1.0.128");
}

#[test]
fn parse_package_lock_json_v2() {
    let content = r#"{
  "packages": {
    "": { "name": "my-app" },
    "node_modules/react": { "version": "18.2.0" },
    "node_modules/@types/react": { "version": "18.2.45" },
    "node_modules/lodash": { "version": "4.17.21" }
  }
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_package_lock_json(content);

    assert_eq!(registry.len(), 3);
    let react = registry.resolve("react").unwrap();
    assert_eq!(react.version, "18.2.0");
    assert_eq!(react.ecosystem, Ecosystem::Npm);

    let types_react = registry.resolve("@types/react").unwrap();
    assert_eq!(types_react.version, "18.2.45");
}

#[test]
fn parse_package_json_dependencies() {
    let content = r#"{
  "dependencies": {
    "react": "^18.2.0"
  },
  "devDependencies": {
    "typescript": "^5.4.0"
  }
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_package_json(content);

    assert!(registry.resolve("react").is_some());
    assert!(registry.resolve("typescript").is_some());
}

#[test]
fn parse_yarn_lock() {
    let content = r#"# yarn lockfile v1

"@babel/core@^7.0.0":
  version "7.24.0"
  resolved "https://registry.yarnpkg.com/@babel/core/-/core-7.24.0.tgz"

react@^18.0.0:
  version "18.2.0"
  resolved "https://registry.yarnpkg.com/react/-/react-18.2.0.tgz"
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_yarn_lock(content);

    assert_eq!(registry.len(), 2);
    let react = registry.resolve("react").unwrap();
    assert_eq!(react.version, "18.2.0");
}

#[test]
fn parse_pnpm_lock() {
    let content = r#"
/react@18.2.0:
  resolution: {integrity: sha512-abc}
/@babel/core@7.24.0:
  resolution: {integrity: sha512-def}
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_pnpm_lock(content);

    assert!(registry.resolve("react").is_some());
    assert!(registry.resolve("@babel/core").is_some());
}

#[test]
fn parse_requirements_txt() {
    let content = r#"
numpy==1.26.0
requests>=2.31.0
flask
# comment
-e git+https://example.com

beautifulsoup4[extra]==4.12.0
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_requirements_txt(content);

    assert_eq!(registry.len(), 4);
    let numpy = registry.resolve("numpy").unwrap();
    assert_eq!(numpy.version, "==1.26.0");
    assert_eq!(numpy.ecosystem, Ecosystem::Python);

    let flask = registry.resolve("flask").unwrap();
    assert_eq!(flask.version, "*");

    let bs4 = registry.resolve("beautifulsoup4").unwrap();
    assert_eq!(bs4.version, "==4.12.0");
}

#[test]
fn parse_pipfile_lock() {
    let content = r#"{
  "_meta": {},
  "default": {
    "numpy": { "version": "==1.26.0" },
    "requests": { "version": "==2.31.0" }
  },
  "develop": {
    "pytest": { "version": "==7.4.0" }
  }
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_pipfile_lock(content);

    assert_eq!(registry.len(), 3);
    let numpy = registry.resolve("numpy").unwrap();
    assert_eq!(numpy.version, "==1.26.0");

    let pytest = registry.resolve("pytest").unwrap();
    assert_eq!(pytest.version, "==7.4.0");
}

#[test]
fn parse_pyproject_toml_project_dependencies() {
    let content = r#"
[project]
dependencies = [
  "requests>=2.31.0",
  "beautifulsoup4==4.12.0"
]
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_pyproject_toml(content);

    assert!(registry.resolve("requests").is_some());
    // Alias mapping should resolve bs4 imports.
    assert!(registry.resolve("bs4").is_some());
}

#[test]
fn parse_poetry_lock() {
    let content = r#"
[[package]]
name = "fastapi"
version = "0.110.0"

[[package]]
name = "uvicorn"
version = "0.29.0"
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_poetry_lock(content);

    assert!(registry.resolve("fastapi").is_some());
    assert!(registry.resolve("uvicorn").is_some());
}

#[test]
fn python_aliases_resolve_import_names() {
    let mut registry = ExternalDependencyRegistry::new();
    registry.insert(ExternalDependency {
        name: "python-dateutil".to_string(),
        version: "2.9.0".to_string(),
        ecosystem: Ecosystem::Python,
    });
    registry.insert(ExternalDependency {
        name: "beautifulsoup4".to_string(),
        version: "4.12.0".to_string(),
        ecosystem: Ecosystem::Python,
    });

    assert!(registry.resolve("dateutil").is_some());
    assert!(registry.resolve("bs4").is_some());
}

#[test]
fn parse_go_sum() {
    let content = r#"github.com/gorilla/mux v1.8.1 h1:abc123
github.com/gorilla/mux v1.8.1/go.mod h1:xyz789
github.com/stretchr/testify v1.9.0 h1:def456
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_go_sum(content);

    assert_eq!(registry.len(), 2);
    let mux = registry.resolve("github.com/gorilla/mux").unwrap();
    assert_eq!(mux.version, "v1.8.1");
    assert_eq!(mux.ecosystem, Ecosystem::GoModules);
}

#[test]
fn parse_gemfile_lock() {
    let content = r#"GEM
  remote: https://rubygems.org/
  specs:
    rails (7.1.0)
      activesupport (= 7.1.0)
    activesupport (7.1.0)
    minitest (5.20.0)

PLATFORMS
  ruby
"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_gemfile_lock(content);

    assert!(registry.len() >= 2);
    let rails = registry.resolve("rails").unwrap();
    assert_eq!(rails.version, "7.1.0");
    assert_eq!(rails.ecosystem, Ecosystem::RubyGems);
}

#[test]
fn parse_composer_lock() {
    let content = r#"{
  "packages": [
    { "name": "monolog/monolog", "version": "3.5.0" },
    { "name": "symfony/console", "version": "v6.4.0" }
  ],
  "packages-dev": [
    { "name": "phpunit/phpunit", "version": "10.5.0" }
  ]
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_composer_lock(content);

    assert_eq!(registry.len(), 3);
    let monolog = registry.resolve("monolog/monolog").unwrap();
    assert_eq!(monolog.version, "3.5.0");
    assert_eq!(monolog.ecosystem, Ecosystem::Composer);
}

#[test]
fn resolve_with_prefix_matching() {
    let mut registry = ExternalDependencyRegistry::new();
    registry.insert(ExternalDependency {
        name: "serde".to_string(),
        version: "1.0.210".to_string(),
        ecosystem: Ecosystem::Cargo,
    });

    // Exact match
    assert!(registry.resolve("serde").is_some());

    // Prefix match (import path has sub-module)
    assert!(registry.resolve("serde.de").is_some());
    assert!(registry.resolve("serde.ser.Serializer").is_some());

    // No match
    assert!(registry.resolve("tokio").is_none());
}

#[test]
fn resolve_with_hyphen_underscore_normalisation() {
    let mut registry = ExternalDependencyRegistry::new();
    registry.insert(ExternalDependency {
        name: "serde-json".to_string(),
        version: "1.0.0".to_string(),
        ecosystem: Ecosystem::Cargo,
    });

    // Underscore variant should match
    assert!(registry.resolve("serde_json").is_some());
    // Original hyphen should also match
    assert!(registry.resolve("serde-json").is_some());
}

#[test]
fn resolve_normalises_import_path() {
    let mut registry = ExternalDependencyRegistry::new();
    registry.insert(ExternalDependency {
        name: "github.com/gorilla/mux".to_string(),
        version: "v1.8.1".to_string(),
        ecosystem: Ecosystem::GoModules,
    });

    // Path separator variants
    assert!(registry.resolve("github.com/gorilla/mux").is_some());
    assert!(registry.resolve("github.com.gorilla.mux").is_some());
}

#[test]
fn annotate_external_nodes_works() {
    use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};

    let mut pdg = ProgramDependenceGraph::new();
    let ext_id = pdg.add_node(Node {
        id: "serde".to_string(),
        name: "serde".to_string(),
        node_type: NodeType::External,
        file_path: Arc::from(""),
        byte_range: (0, 0),
        language: "external".to_string(),
        complexity: 0,
    });

    let internal_id = pdg.add_node(Node {
        id: "my_func".to_string(),
        name: "my_func".to_string(),
        node_type: NodeType::Function,
        file_path: Arc::from("src/lib.rs"),
        byte_range: (0, 100),
        language: "rust".to_string(),
        complexity: 5,
    });

    let mut registry = ExternalDependencyRegistry::new();
    registry.insert(ExternalDependency {
        name: "serde".to_string(),
        version: "1.0.210".to_string(),
        ecosystem: Ecosystem::Cargo,
    });

    let stats = annotate_external_nodes(&mut pdg, &registry);

    assert_eq!(stats.total_external, 1);
    assert_eq!(stats.resolved, 1);
    assert_eq!(stats.unresolved, 0);

    // Check the node was updated
    let node = pdg.get_node(ext_id).unwrap();
    assert_eq!(node.language, "external:cargo");
    assert!(node.id.contains("1.0.210"));

    // Internal node should be unchanged
    let internal = pdg.get_node(internal_id).unwrap();
    assert_eq!(internal.language, "rust");
}

#[test]
fn from_project_empty_dir_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ExternalDependencyRegistry::from_project(dir.path());
    assert_eq!(registry.len(), 0);
}

#[test]
fn from_project_with_cargo_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("Cargo.lock"),
        "[[package]]\nname = \"anyhow\"\nversion = \"1.0.86\"\n",
    )
    .expect("write");

    let registry = ExternalDependencyRegistry::from_project(dir.path());
    assert_eq!(registry.len(), 1);
    assert!(registry.resolve("anyhow").is_some());
}

#[test]
fn from_project_with_cargo_toml_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n",
    )
    .expect("write");

    let registry = ExternalDependencyRegistry::from_project(dir.path());
    assert!(registry.resolve("serde").is_some());
}

#[test]
fn from_project_with_requirements_txt() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("requirements.txt"),
        "numpy==1.26.0\nrequests>=2.31.0\n",
    )
    .expect("write");

    let registry = ExternalDependencyRegistry::from_project(dir.path());
    assert_eq!(registry.len(), 2);
    assert!(registry.resolve("numpy").is_some());
    assert!(registry.resolve("requests").is_some());
}

#[test]
fn from_project_with_package_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies":{"react":"^18.2.0"}}"#,
    )
    .expect("write");

    let registry = ExternalDependencyRegistry::from_project(dir.path());
    assert!(registry.resolve("react").is_some());
}

#[test]
fn from_project_discovers_nested_workspace_manifests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nested = dir.path().join("packages").join("web");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(
        nested.join("package.json"),
        r#"{"dependencies":{"react":"^18.2.0","zod":"^3.23.8"}}"#,
    )
    .expect("write");

    let registry = ExternalDependencyRegistry::from_project(dir.path());
    assert!(registry.resolve("react").is_some());
    assert!(registry.resolve("zod").is_some());
}

#[test]
fn from_project_with_go_mod() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("go.mod"),
        "module demo\n\nrequire github.com/gorilla/mux v1.8.1\n",
    )
    .expect("write");

    let registry = ExternalDependencyRegistry::from_project(dir.path());
    assert!(registry.resolve("github.com/gorilla/mux").is_some());
}

#[test]
fn annotate_external_nodes_marks_builtin_modules() {
    use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};

    let mut pdg = ProgramDependenceGraph::new();
    let ext_id = pdg.add_node(Node {
        id: "std".to_string(),
        name: "std".to_string(),
        node_type: NodeType::External,
        file_path: Arc::from(""),
        byte_range: (0, 0),
        language: "external".to_string(),
        complexity: 0,
    });

    let registry = ExternalDependencyRegistry::new();
    let stats = annotate_external_nodes(&mut pdg, &registry);
    assert_eq!(stats.total_external, 1);
    assert_eq!(stats.resolved, 0);
    assert_eq!(stats.builtin, 1);
    assert_eq!(stats.unresolved, 0);
    assert_eq!(pdg.get_node(ext_id).unwrap().language, "external:system");
}

#[test]
fn annotate_external_nodes_deduplicates_unresolved_import_names() {
    use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};

    let mut pdg = ProgramDependenceGraph::new();
    pdg.add_node(Node {
        id: "src/main.rs:__external__:react".to_string(),
        name: "react".to_string(),
        node_type: NodeType::External,
        file_path: Arc::from("src/main.rs"),
        byte_range: (0, 0),
        language: "external".to_string(),
        complexity: 0,
    });
    pdg.add_node(Node {
        id: "src/app.ts:__external__:react".to_string(),
        name: "react".to_string(),
        node_type: NodeType::External,
        file_path: Arc::from("src/app.ts"),
        byte_range: (0, 0),
        language: "external".to_string(),
        complexity: 0,
    });

    let registry = ExternalDependencyRegistry::new();
    let stats = annotate_external_nodes(&mut pdg, &registry);
    assert_eq!(stats.total_external, 2);
    assert_eq!(stats.unresolved, 1);
}

#[test]
fn parse_bun_lock_basic() {
    let content = r#"{
  "lockfileVersion": 1,
  "packages": {
    "react": ["react@18.2.0", "", {}, "sha512-abc="],
    "@types/react": ["@types/react@18.2.45", "", {}, "sha512-def="],
    "lodash": ["lodash@4.17.21", "", {}, "sha512-ghi="]
  }
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_bun_lock(content);

    assert_eq!(registry.len(), 3);
    let react = registry.resolve("react").unwrap();
    assert_eq!(react.version, "18.2.0");
    assert_eq!(react.ecosystem, Ecosystem::Npm);

    let types_react = registry.resolve("@types/react").unwrap();
    assert_eq!(types_react.version, "18.2.45");

    let lodash = registry.resolve("lodash").unwrap();
    assert_eq!(lodash.version, "4.17.21");
}

#[test]
fn parse_bun_lock_with_trailing_commas() {
    // bun.lock uses JSONC format with trailing commas
    let content = r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": {
      "name": "my-app",
      "dependencies": {
        "react": "^18.2.0",
        "zod": "^3.23.8",
      },
      "devDependencies": {
        "typescript": "^5.4.0",
      },
    },
  },
  "packages": {
    "react": ["react@18.2.0", "", {}, "sha512-abc="],
    "zod": ["zod@3.23.8", "", {}, "sha512-def="],
  },
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_bun_lock(content);

    // Packages from lockfile should be resolved with exact versions
    let react = registry.resolve("react").unwrap();
    assert_eq!(react.version, "18.2.0");

    let zod = registry.resolve("zod").unwrap();
    assert_eq!(zod.version, "3.23.8");

    // Workspace deps that aren't in packages should be included as fallback
    let ts = registry.resolve("typescript").unwrap();
    assert_eq!(ts.version, "^5.4.0");
}

#[test]
fn parse_bun_lock_with_jsonc_comments() {
    let content = r#"{
  // top-level comment
  "packages": {
    "react": ["react@18.2.0", "", {}, "sha512-abc="], /* inline block comment */
    "urlish": ["urlish@1.0.0", "", {"homepage": "https://example.com/a//b"}, "sha512-def="],
  },
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_bun_lock(content);

    assert_eq!(registry.resolve("react").unwrap().version, "18.2.0");
    assert_eq!(registry.resolve("urlish").unwrap().version, "1.0.0");
}

#[test]
fn parse_bun_lock_scoped_packages() {
    let content = r#"{
  "packages": {
    "@babel/core": ["@babel/core@7.29.0", "", {}, "sha512-abc="],
    "@eslint/js": ["@eslint/js@10.0.1", "", {}, "sha512-def="]
  }
}"#;
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_bun_lock(content);

    assert_eq!(registry.len(), 2);
    let babel = registry.resolve("@babel/core").unwrap();
    assert_eq!(babel.version, "7.29.0");

    let eslint = registry.resolve("@eslint/js").unwrap();
    assert_eq!(eslint.version, "10.0.1");
}

#[test]
fn parse_bun_lock_empty_and_invalid() {
    // Empty JSON
    let mut registry = ExternalDependencyRegistry::new();
    registry.parse_bun_lock("{}");
    assert_eq!(registry.len(), 0);

    // Invalid JSON
    registry.parse_bun_lock("not valid json at all");
    assert_eq!(registry.len(), 0);

    // No packages or workspaces
    registry.parse_bun_lock(r#"{"lockfileVersion": 1}"#);
    assert_eq!(registry.len(), 0);
}

#[test]
fn strip_trailing_commas_handles_escaped_backslashes() {
    // A string containing an escaped backslash before a quote:
    // "foo\\" — the \\ is an escaped backslash, so the " after it
    // terminates the string. The old code (checking only the single
    // preceding char) would incorrectly think the quote was escaped.
    let input = r#"{"key": "value\\", "num": 42,}"#;
    let result = strip_trailing_commas(input);
    // The trailing comma after 42 should be removed
    assert!(
        !result.contains("42,}"),
        "trailing comma should be stripped, got: {}",
        result
    );
    assert!(result.contains("42"));
    // The backslash pair should be preserved
    assert!(
        result.contains("value\\\\"),
        "escaped backslashes should be preserved, got: {}",
        result
    );

    // Verify the result is valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&result).expect("result should be valid JSON");
    assert_eq!(parsed["key"], r"value\");
    assert_eq!(parsed["num"], 42);
}

#[test]
fn strip_trailing_commas_handles_escaped_quote() {
    // A string with an escaped quote: "foo\"" — the \" is an escaped
    // quote, so the string does NOT terminate there. The final "
    // terminates the string. The trailing comma after should be stripped.
    let input = r#"{"key": "foo\"bar", "num": 42,}"#;
    let result = strip_trailing_commas(input);
    let parsed: serde_json::Value =
        serde_json::from_str(&result).expect("result should be valid JSON");
    assert_eq!(parsed["key"], r#"foo"bar"#);
    assert_eq!(parsed["num"], 42);
}

#[test]
fn strip_trailing_commas_preserves_commas_inside_strings() {
    // Commas inside strings should NOT be stripped even if followed by }
    let input = r#"{"key": "a,b}", "num": 42}"#;
    let result = strip_trailing_commas(input);
    let parsed: serde_json::Value =
        serde_json::from_str(&result).expect("result should be valid JSON");
    assert_eq!(parsed["key"], "a,b}");
}
