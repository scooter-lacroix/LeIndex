//! Equivalence tests: signature-only (lite) extraction must produce the same
//! header fields as the full extraction for every supported language, while
//! leaving the expensive body-derived fields empty.

use crate::parse::bash::BashParser;
use crate::parse::c::CParser;
use crate::parse::cpp::CppParser;
use crate::parse::csharp::CSharpParser;
use crate::parse::dart::DartParser;
use crate::parse::go::GoParser;
use crate::parse::java::JavaParser;
use crate::parse::javascript::{JavaScriptParser, TypeScriptParser};
use crate::parse::kotlin::KotlinParser;
use crate::parse::lua::LuaParser;
use crate::parse::php::PhpParser;
use crate::parse::python::PythonParser;
use crate::parse::ruby::RubyParser;
use crate::parse::rust::RustParser;
use crate::parse::scala::ScalaParser;
use crate::parse::swift::SwiftParser;
use crate::parse::traits::{CodeIntelligence, SignatureInfo, lite};

fn header(sig: &SignatureInfo) -> String {
    format!(
        "{}|{}|{:?}|{:?}|{:?}|async={}|method={}|{:?}",
        sig.name,
        sig.qualified_name,
        sig.parameters,
        sig.return_type,
        sig.visibility,
        sig.is_async,
        sig.is_method,
        sig.byte_range
    )
}

fn assert_lite_matches_full<P: CodeIntelligence>(label: &str, parser: P, source: &str) {
    let mut ts = tree_sitter::Parser::new();
    let full = parser
        .get_signatures_with_parser(source.as_bytes(), &mut ts)
        .unwrap_or_else(|e| panic!("{label}: full extraction failed: {e}"));
    let light = parser
        .get_signatures_lite(source.as_bytes(), &mut ts)
        .unwrap_or_else(|e| panic!("{label}: lite extraction failed: {e}"));

    assert!(!full.is_empty(), "{label}: fixture produced no signatures");
    let full_headers: Vec<String> = full.iter().map(header).collect();
    let lite_headers: Vec<String> = light.iter().map(header).collect();
    assert_eq!(
        full_headers, lite_headers,
        "{label}: lite header fields diverged from full extraction"
    );

    for sig in &light {
        assert!(sig.calls.is_empty(), "{label}: lite kept calls");
        assert!(sig.flow_facts.is_empty(), "{label}: lite kept flow facts");
        assert!(sig.docstring.is_none(), "{label}: lite kept docstring");
        assert!(sig.imports.is_empty(), "{label}: lite kept imports");
        assert_eq!(
            sig.cyclomatic_complexity, 0,
            "{label}: lite kept complexity"
        );
    }
    assert!(
        !lite(),
        "{label}: lite guard leaked out of get_signatures_lite"
    );
}

#[test]
fn test_lite_matches_full_rust() {
    assert_lite_matches_full(
        "rust",
        RustParser::new(),
        "use std::collections::HashMap;\n\
         /// Adds one.\n\
         pub async fn add(x: u32, y: u32) -> u32 {\n    if x > y { helper(x) } else { y.max(1) }\n}\n\
         struct S;\n\
         impl S {\n    /// Method doc\n    pub fn go(&self, a: &str) -> String {\n        a.to_string()\n    }\n    fn assoc() {}\n}\n\
         mod inner { pub(crate) fn nested(z: i64) {} }\n",
    );
}

#[test]
fn test_lite_matches_full_python() {
    assert_lite_matches_full(
        "python",
        PythonParser::new(),
        "import os\n\nasync def fetch(url: str, retries: int = 3) -> bytes:\n    \"\"\"Fetch it.\"\"\"\n    return get(url)\n\nclass C:\n    def m(self, x):\n        if x:\n            return other(x)\n",
    );
}

#[test]
fn test_lite_matches_full_javascript() {
    assert_lite_matches_full(
        "javascript",
        JavaScriptParser::new(),
        "import fs from 'fs';\n/** Doc */\nasync function load(path, opts) {\n  return fs.readFile(path);\n}\nclass K { run(a, b) { return load(a); } }\n",
    );
}

#[test]
fn test_lite_matches_full_typescript() {
    assert_lite_matches_full(
        "typescript",
        TypeScriptParser::new(),
        "import { x } from './x';\n/** Doc */\nexport async function load(path: string, n: number): Promise<string> {\n  return x(path);\n}\nclass K { run(a: string): void { load(a, 1); } }\n",
    );
}

#[test]
fn test_lite_matches_full_go() {
    assert_lite_matches_full(
        "go",
        GoParser::new(),
        "package main\n\nimport \"fmt\"\n\n// Greet says hi.\nfunc Greet(name string, n int) string {\n\tfmt.Println(name)\n\treturn name\n}\n\ntype T struct{}\n\nfunc (t *T) Run(a int) error {\n\treturn nil\n}\n",
    );
}

#[test]
fn test_lite_matches_full_java() {
    assert_lite_matches_full(
        "java",
        JavaParser::new(),
        "import java.util.List;\n/** Doc */\npublic class A {\n    /** Run it. */\n    public int run(int a, String b) {\n        helper(a);\n        return a;\n    }\n    private static void helper(int x) {}\n}\n",
    );
}

#[test]
fn test_lite_matches_full_c() {
    assert_lite_matches_full(
        "c",
        CParser::new(),
        "#include <stdio.h>\n/* Doc */\nint add(int a, int b) {\n    printf(\"x\");\n    return a + b;\n}\nstatic void noop(void) {}\n",
    );
}

#[test]
fn test_lite_matches_full_cpp() {
    assert_lite_matches_full(
        "cpp",
        CppParser::new(),
        "#include <vector>\n/** Doc */\nint add(int a, int b) {\n    helper(a);\n    return a + b;\n}\nclass K {\npublic:\n    void run(int x) { add(x, 1); }\n};\n",
    );
}

#[test]
fn test_lite_matches_full_csharp() {
    assert_lite_matches_full(
        "csharp",
        CSharpParser::new(),
        "using System;\nnamespace N {\n  public class A {\n    public async Task<int> Run(int a, string b) {\n      Helper(a);\n      return a;\n    }\n    private void Helper(int x) {}\n  }\n}\n",
    );
}

#[test]
fn test_lite_matches_full_php() {
    assert_lite_matches_full(
        "php",
        PhpParser::new(),
        "<?php\nuse Foo\\Bar;\n/** Doc */\nfunction add($a, $b) {\n    return helper($a);\n}\nclass K {\n    public function run($x) { return add($x, 1); }\n}\n",
    );
}

#[test]
fn test_lite_matches_full_ruby() {
    assert_lite_matches_full(
        "ruby",
        RubyParser::new(),
        "require 'json'\n# Doc\ndef add(a, b)\n  helper(a)\nend\nclass K\n  def run(x)\n    add(x, 1)\n  end\nend\n",
    );
}

#[test]
fn test_lite_matches_full_scala() {
    assert_lite_matches_full(
        "scala",
        ScalaParser::new(),
        "import scala.collection.mutable\n/** Doc */\nobject A {\n  def add(a: Int, b: Int): Int = helper(a)\n  def helper(x: Int): Int = x\n}\n",
    );
}

#[test]
fn test_lite_matches_full_lua() {
    assert_lite_matches_full(
        "lua",
        LuaParser::new(),
        "local json = require('json')\n-- Doc\nfunction add(a, b)\n  return helper(a)\nend\nlocal function helper(x) return x end\n",
    );
}

#[test]
fn test_lite_matches_full_bash() {
    assert_lite_matches_full(
        "bash",
        BashParser::new(),
        "#!/bin/bash\nsource ./lib.sh\n# Doc\nadd() {\n  echo \"$1\"\n  helper\n}\nhelper() { :; }\n",
    );
}

#[test]
fn test_lite_matches_full_kotlin() {
    assert_lite_matches_full(
        "kotlin",
        KotlinParser::new(),
        "import kotlin.math.max\n/** Doc */\nfun add(a: Int, b: Int): Int {\n    return max(a, b)\n}\nclass K { fun run(x: Int) { add(x, 1) } }\n",
    );
}

#[test]
fn test_lite_matches_full_swift() {
    assert_lite_matches_full(
        "swift",
        SwiftParser::new(),
        "import Foundation\n/// Doc\nfunc add(a: Int, b: Int) -> Int {\n    return max(a, b)\n}\nclass K { func run(x: Int) { _ = add(a: x, b: 1) } }\n",
    );
}

#[test]
fn test_lite_matches_full_dart() {
    assert_lite_matches_full(
        "dart",
        DartParser::new(),
        "import 'dart:math';\n/// Doc\nint add(int a, int b) {\n  return max(a, b);\n}\nclass K { void run(int x) { add(x, 1); } }\n",
    );
}

#[test]
fn test_lite_guard_is_thread_local_and_nests() {
    use crate::parse::traits::LiteGuard;
    assert!(!lite());
    let outer = LiteGuard::enter();
    assert!(lite());
    {
        let _inner = LiteGuard::enter();
        assert!(lite());
    }
    assert!(lite(), "inner guard drop must not clear the outer scope");
    std::thread::spawn(|| assert!(!lite(), "flag must not leak to other threads"))
        .join()
        .unwrap();
    drop(outer);
    assert!(!lite());
}

#[test]
fn test_lite_actually_drops_body_derived_fields_that_full_keeps() {
    // Guards against the equivalence tests passing vacuously: full extraction
    // must produce calls/docstring/imports/complexity for these fixtures.
    let mut ts = tree_sitter::Parser::new();
    let src =
        "use std::fmt;\n/// Doc\npub fn f(x: u32) -> u32 {\n    if x > 1 { g(x) } else { 0 }\n}\n";
    let full = RustParser::new()
        .get_signatures_with_parser(src.as_bytes(), &mut ts)
        .unwrap();
    let f = full.iter().find(|s| s.name == "f").unwrap();
    assert!(!f.calls.is_empty());
    assert!(f.docstring.is_some());
    assert!(!f.imports.is_empty());
    assert!(f.cyclomatic_complexity >= 1);
    assert!(!f.flow_facts.is_empty());

    let src = "import os\n\ndef f(x):\n    \"\"\"Doc.\"\"\"\n    return g(x)\n";
    let full = PythonParser::new()
        .get_signatures_with_parser(src.as_bytes(), &mut ts)
        .unwrap();
    let f = full.iter().find(|s| s.name == "f").unwrap();
    assert!(!f.calls.is_empty());
    assert!(f.docstring.is_some());
    assert!(!f.imports.is_empty());
}
