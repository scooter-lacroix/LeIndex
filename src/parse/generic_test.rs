//! Tier-0 breadth tests: every registered language must load its grammar
//! and parse a real fixture. This is the guardrail for the 100+ languages
//! goal — breadth can only grow, never silently regress.

#[cfg(test)]
fn parse_case(language: &str, source: &str) {
    let parser = crate::parse::languages::parser_for_language(language)
        .unwrap_or_else(|| panic!("no parser registered for {language}"));
    // Signatures may legitimately be empty (parse-only languages); the
    // contract under test is that the grammar LOADS and the file PARSES
    // without error.
    parser
        .get_signatures(source.as_bytes())
        .unwrap_or_else(|e| panic!("parse failed for {language}: {e}"));
}

#[cfg(test)]
#[test]
fn test_tier0_bespoke_languages_parse() {
    parse_case(
        "swift",
        "struct User { let name: String }\nfunc greet(_ user: User) -> String { return \"hi\" }\n",
    );
    parse_case(
        "kotlin",
        "class Repo { fun find(id: Int): String { return \"x\" } }\n",
    );
    parse_case(
        "dart",
        "class Repo { String find(int id) { return 'x'; } }\n",
    );
}

#[cfg(test)]
#[test]
fn test_tier0_generic_languages_parse() {
    parse_case(
        "html",
        "<html><body><script>function f() {}</script></body></html>",
    );
    parse_case("css", ".a { color: red; }");
    parse_case("scss", ".a { .b { color: red; } }");
    parse_case("yaml", "name: leindex\nversion: 2\n");
    parse_case("cmake", "add_executable(leindex main.c)\n");
    parse_case("elixir", "defmodule Repo do\n  def find(id), do: id\nend\n");
    parse_case("erlang", "find(_Id) -> ok.\n");
    parse_case("haskell", "find :: Int -> String\nfind _ = \"x\"\n");
    parse_case("perl", "sub find { return 1; }\n");
    parse_case("r", "find <- function(id) { id }\n");
    parse_case("zig", "pub fn find(id: u32) u32 { return id; }\n");
    parse_case("graphql", "type Query { find(id: ID!): String }\n");
    parse_case("hcl", "resource \"aws_instance\" \"web\" { ami = \"x\" }\n");
    parse_case("make", "all:\n\techo hi\n");
    parse_case("elisp", "(defun find (id) id)\n");
    parse_case("julia", "function find(id) return id end\n");
    parse_case("d", "int find(int id) { return id; }\n");
    parse_case("glsl", "vec3 find(float x) { return vec3(x); }\n");
    parse_case("embedded-template", "<p><%= user.name %></p>");
}

#[cfg(test)]
#[test]
fn test_generic_parser_extracts_signatures_where_rules_exist() {
    let parser = crate::parse::languages::parser_for_language("zig").unwrap();
    let signatures = parser
        .get_signatures(b"pub fn alpha() void {}\npub fn beta(x: u32) u32 { return x; }\n")
        .expect("zig parse");
    assert!(
        signatures.iter().any(|s| s.name.contains("alpha")),
        "zig FnDef rule should surface 'alpha': {:?}",
        signatures
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>()
    );

    let parser = crate::parse::languages::parser_for_language("graphql").unwrap();
    let signatures = parser
        .get_signatures(b"type Query { find(id: ID!): String }\n")
        .expect("graphql parse");
    assert!(
        signatures.iter().any(|s| s.name.contains("Query")),
        "graphql object_type_definition should surface 'Query': {:?}",
        signatures
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>()
    );
}

/// The breadth floor: Tier-0 active-language count. When this fails after
/// ADDING languages, bump the floor — it exists to catch silent REMOVALS.
#[cfg(test)]
#[test]
fn test_tier0_active_language_floor() {
    let count = crate::parse::languages::active_language_count();
    assert!(
        count >= 37,
        "Tier-0 regression: only {count} active languages (floor 37)"
    );
}
