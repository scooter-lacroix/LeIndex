// Tests for the fragment chunker, orphan extraction, and enrichment.
//
// The semantic + naive chunker tests are ported VERBATIM from Warp's
// `full_source_code_embedding/chunker/{semantic_tests,naive_tests}.rs` with two
// mechanical adaptations: byte offsets are `usize` (no `ByteOffset`), and the
// language lookup uses LeIndex's grammar registry (`LanguageId`).

use std::path::Path;

use super::*;
use crate::parse::grammar::LanguageId;

/// Resolve the tree-sitter Language for a file extension via the shared cache.
fn ts_language(ext: &str) -> tree_sitter::Language {
    LanguageId::from_extension(ext)
        .expect("language must exist")
        .from_cache()
        .expect("grammar loads")
}

// ---------------------------------------------------------------------------
// Ported from Warp chunker/semantic_tests.rs (verbatim expectations)
// ---------------------------------------------------------------------------

#[test]
fn test_basic_rust_chunking() {
    let source_code = r#"
#[derive(Debug)]
struct Rectangle {
    width: u32,
    height: u32,
}

impl Rectangle {
    fn area(&self) -> u32 {
        self.width * self.height
    }
}

fn main() {
    let rect1 = Rectangle {
        width: 30,
        height: 50,
    };

    println!(
        "The area of the rectangle is {} square pixels.",
        rect1.area()
    );
}
"#;

    let max_chunk_size = 128;

    let chunks = chunk_semantic(
        source_code,
        Path::new("test.rs"),
        max_chunk_size,
        &ts_language("rs"),
    )
    .unwrap();

    assert_eq!(chunks.len(), 4);

    // None of the chunks should exceed the chunk size.
    for chunk in &chunks {
        assert!(
            chunk.content.len() <= max_chunk_size,
            "Chunk should not exceed max size of {max_chunk_size} but was: {}",
            chunk.content.len()
        );
    }

    assert_eq!(
        chunks[0].content.trim(),
        r#"#[derive(Debug)]
struct Rectangle {
    width: u32,
    height: u32,
}"#
    );
    assert_eq!(
        chunks[1].content.trim(),
        r#"impl Rectangle {
    fn area(&self) -> u32 {
        self.width * self.height
    }
}"#
    );
    assert_eq!(
        chunks[2].content.trim(),
        r#"fn main() {
    let rect1 = Rectangle {
        width: 30,
        height: 50,
    };"#
    );
    assert_eq!(
        chunks[3].content.trim(),
        r#"println!(
        "The area of the rectangle is {} square pixels.",
        rect1.area()
    );
}"#
    );
}

/// Fragments must never cross a node byte range (plan invariant 5): each
/// fragment is a contiguous sub-range of one parsed unit.
#[test]
fn test_no_fragment_crosses_owner_range() {
    let source_code = r#"
fn long_function() {
    let a = 1;
    let b = 2;
    let c = 3;
    let d = 4;
    let e = 5;
    let f = 6;
    let g = 7;
    let h = 8;
    let i = 9;
    let j = 10;
    let k = 11;
    let l = 12;
    let m = 13;
    let n = 14;
    let o = 15;
}
"#;

    let chunks = chunk_semantic(source_code, Path::new("test.rs"), 64, &ts_language("rs"))
        .expect("semantic chunking succeeds for rust");

    // Every fragment is a contiguous slice of the input; ordering is preserved.
    let mut last_end = 0usize;
    for chunk in &chunks {
        assert_eq!(
            chunk.content,
            &source_code[chunk.start_byte_index..chunk.end_byte_index]
        );
        assert!(chunk.start_byte_index >= last_end, "fragments overlap");
        last_end = chunk.end_byte_index;
    }
}

// ---------------------------------------------------------------------------
// Ported from Warp chunker/naive_tests.rs (verbatim expectations)
// ---------------------------------------------------------------------------

#[test]
fn test_chunker() {
    let code = "This is some text content\nthat should be chunked\nusing the naive chunker\nbecause the language isn't recognized.";
    let path = Path::new("test_file.xyz");

    let max_lines = 1;
    let fragments = chunk_naive(code, path, 10000, max_lines);

    assert!(!fragments.is_empty(), "Expected at least one fragment");

    assert_eq!(fragments.len(), code.lines().count());
    for (idx, line) in code.lines().enumerate() {
        assert_eq!(fragments[idx].content, line);
        assert_eq!(fragments[idx].start_line, idx);
        assert_eq!(fragments[idx].end_line, idx);
    }
}

#[test]
fn test_chunker_large_chunk() {
    let code = "This is some text content\nthat should be chunked\nusing the naive chunker\nbecause the language isn't recognized.";
    let path = Path::new("test_file.xyz");

    let fragments = chunk_naive(code, path, 10000, 100);

    // We should have only one fragment
    assert_eq!(fragments.len(), 1);

    assert_eq!(fragments[0].content, code);
    assert_eq!(fragments[0].start_line, 0);
    assert_eq!(fragments[0].end_line, code.lines().count() - 1);
}

#[test]
fn test_chunker_max_bytes() {
    // Create a string with known byte size - each line is exactly 20 bytes including newline
    let code = "line1\nline2\nline3\nline4abcdefghijklmnopqrstuvwxyz";
    let path = Path::new("test_file.xyz");

    // Set max_bytes_per_chunk to 25 bytes to force multiple chunks for the last line (which is 30 bytes).
    let max_bytes_per_chunk = 25;
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1000);

    // Verify we have multiple chunks
    assert!(
        fragments.len() > 1,
        "Expected multiple chunks due to size limit"
    );

    // Verify that no chunk exceeds the max_bytes_per_chunk limit
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            fragment.content.trim().len() <= max_bytes_per_chunk,
            "Fragment {} has size {} bytes, which exceeds limit of {} bytes",
            i,
            fragment.content.len(),
            max_bytes_per_chunk
        );
    }

    // The first fragment contains all of the lines except the last one.
    assert_eq!(fragments[0].content, "line1\nline2\nline3");

    // The last two fragments contains the contents of the line line.
    assert_eq!(fragments[1].content, "line4abcdefghijklmnopqrst");
    assert_eq!(fragments[2].content, "uvwxyz");

    // Verify that the chunks together contain all the original content
    let reassembled_content: String = fragments
        .iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("");

    // Ignore any newlines when doing comparisons--the chunker may drop newlines at fragment boundaries
    // and that's not necessary for testing the correctness of the naive chunker.
    assert_eq!(
        reassembled_content.replace('\n', ""),
        code.replace('\n', ""),
        "Reassembled content does not match original"
    );
}

#[test]
fn test_utf8_emoji_chunking() {
    // Test with emojis (4-byte UTF-8 characters) to ensure byte boundaries are respected
    let code = "Hello 🦀 Rust\nWorld 🌍 Test\n🚀 Rocket 🎯 Target";
    let path = Path::new("test_emoji.txt");

    // Set a small max_bytes_per_chunk to force splitting through emoji characters
    let max_bytes_per_chunk = 15; // This will force splits in the middle of emoji sequences
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1000);

    // Verify we have multiple chunks
    assert!(
        fragments.len() > 1,
        "Expected multiple chunks due to size limit"
    );

    // Verify that no chunk exceeds the max_bytes_per_chunk limit
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            fragment.content.len() <= max_bytes_per_chunk,
            "Fragment {} has size {} bytes, which exceeds limit of {} bytes. Content: '{}'",
            i,
            fragment.content.len(),
            max_bytes_per_chunk,
            fragment.content
        );
    }

    // Verify that all fragments contain valid UTF-8
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            fragment.content.is_ascii() || std::str::from_utf8(fragment.content.as_bytes()).is_ok(),
            "Fragment {} contains invalid UTF-8: {:?}",
            i,
            fragment.content
        );
    }

    // Verify that reassembled content matches original (ignoring newlines)
    let reassembled_content: String = fragments
        .iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        reassembled_content.replace('\n', ""),
        code.replace('\n', ""),
        "Reassembled content does not match original"
    );
}

#[test]
fn test_utf8_accented_characters() {
    // Test with accented characters (2-byte UTF-8)
    let code = "Café résumé naïve\nÉlève découvrir\nMañana piñata";
    let path = Path::new("test_accents.txt");

    // Set max_bytes_per_chunk to force splitting through accented characters
    let max_bytes_per_chunk = 10;
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1000);

    // Verify we have multiple chunks
    assert!(
        fragments.len() > 1,
        "Expected multiple chunks due to size limit"
    );

    // Verify that all fragments contain valid UTF-8
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            std::str::from_utf8(fragment.content.as_bytes()).is_ok(),
            "Fragment {} contains invalid UTF-8: {:?}",
            i,
            fragment.content.as_bytes()
        );
    }

    // Verify that reassembled content matches original (ignoring newlines)
    let reassembled_content: String = fragments
        .iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        reassembled_content.replace('\n', ""),
        code.replace('\n', ""),
        "Reassembled content does not match original"
    );
}

#[test]
fn test_utf8_mixed_characters() {
    // Test with a mix of ASCII, 2-byte, 3-byte, and 4-byte UTF-8 characters
    let code = "ASCII text 中文 🦀 résumé ℘ math symbols";
    let path = Path::new("test_mixed.txt");

    // Set a small chunk size to force many splits
    let max_bytes_per_chunk = 8;
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1000);

    // Verify we have multiple chunks
    assert!(
        fragments.len() > 1,
        "Expected multiple chunks due to size limit"
    );

    // Verify that all fragments contain valid UTF-8 and don't exceed size limit
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            fragment.content.len() <= max_bytes_per_chunk,
            "Fragment {} has size {} bytes, which exceeds limit of {} bytes",
            i,
            fragment.content.len(),
            max_bytes_per_chunk
        );

        assert!(
            std::str::from_utf8(fragment.content.as_bytes()).is_ok(),
            "Fragment {} contains invalid UTF-8: {:?}",
            i,
            fragment.content.as_bytes()
        );
    }

    // Verify that reassembled content matches original
    let reassembled_content: String = fragments
        .iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        reassembled_content, code,
        "Reassembled content does not match original"
    );
}

#[test]
fn test_utf8_boundary_edge_cases() {
    // Test edge case where chunk boundary falls exactly on a multi-byte character
    let code = "ab🦀cd"; // 'ab' (2 bytes) + '🦀' (4 bytes) + 'cd' (2 bytes) = 8 bytes total
    let path = Path::new("test_edge.txt");

    // Set chunk size to 3 bytes, which would split in the middle of the emoji without our fix
    let max_bytes_per_chunk = 3;
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1000);

    // Should have multiple fragments
    assert!(fragments.len() >= 2, "Expected at least 2 fragments");

    // Verify all fragments are valid UTF-8
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            std::str::from_utf8(fragment.content.as_bytes()).is_ok(),
            "Fragment {} contains invalid UTF-8: {:?}",
            i,
            fragment.content.as_bytes()
        );
    }

    // Verify reassembled content matches original
    let reassembled_content: String = fragments
        .iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        reassembled_content, code,
        "Reassembled content does not match original"
    );
}

#[test]
fn test_utf8_single_multibyte_character() {
    // Test with a single multi-byte character that's larger than chunk size
    let code = "🦀"; // 4-byte emoji
    let path = Path::new("test_single.txt");

    // Set chunk size smaller than the character
    let max_bytes_per_chunk = 2;
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1000);

    // Should have exactly one fragment (can't split a single character)
    assert_eq!(fragments.len(), 1, "Should have exactly one fragment");

    // The fragment should contain the complete character
    assert_eq!(fragments[0].content, code);

    // Verify it's valid UTF-8
    assert!(
        std::str::from_utf8(fragments[0].content.as_bytes()).is_ok(),
        "Fragment contains invalid UTF-8"
    );
}

#[test]
fn test_utf8_line_endings_with_multibyte() {
    // Test multi-byte characters at line boundaries
    let code = "Hello🌍\nWorld🦀\nTest🎯";
    let path = Path::new("test_lines.txt");

    let max_bytes_per_chunk = 10;
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1); // 1 line per chunk

    // Should have 3 fragments (one per line)
    assert_eq!(fragments.len(), 3, "Should have 3 fragments for 3 lines");

    // Verify all fragments are valid UTF-8
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            std::str::from_utf8(fragment.content.as_bytes()).is_ok(),
            "Fragment {} contains invalid UTF-8: {:?}",
            i,
            fragment.content.as_bytes()
        );
    }

    // Verify line numbers are correct
    assert_eq!(fragments[0].start_line, 0);
    assert_eq!(fragments[0].end_line, 0);
    assert_eq!(fragments[1].start_line, 1);
    assert_eq!(fragments[1].end_line, 1);
    assert_eq!(fragments[2].start_line, 2);
    assert_eq!(fragments[2].end_line, 2);
}

#[test]
fn test_panic_regression_byte_boundary() {
    // This is a regression test for the "byte index is not a char boundary" panic.
    // Before the fix, this would panic when trying to slice at byte index 3,
    // which is in the middle of the 4-byte emoji '🦀'.
    let code = "Hi🦀Test";
    let path = Path::new("test_panic.txt");

    // This chunk size would cause the original code to panic
    let max_bytes_per_chunk = 3;

    // This should not panic
    let fragments = chunk_naive(code, path, max_bytes_per_chunk, 1000);

    // Verify we get valid fragments
    assert!(!fragments.is_empty(), "Should have at least one fragment");

    // Verify all fragments are valid UTF-8
    for (i, fragment) in fragments.iter().enumerate() {
        assert!(
            std::str::from_utf8(fragment.content.as_bytes()).is_ok(),
            "Fragment {} contains invalid UTF-8: {:?}",
            i,
            fragment.content.as_bytes()
        );
    }

    // Verify reassembled content matches original
    let reassembled_content: String = fragments
        .iter()
        .map(|f| f.content)
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        reassembled_content, code,
        "Reassembled content does not match original"
    );
}

// ---------------------------------------------------------------------------
// Orphan (Tier-3) tests
// ---------------------------------------------------------------------------

fn orphan_input<'a>(
    code: &'a str,
    node_ranges: &'a [(usize, usize)],
    file_doc_end: usize,
    max_bytes: usize,
) -> OrphanInput<'a> {
    OrphanInput {
        file_bytes: code.as_bytes(),
        path: Path::new("orphan_test.rs"),
        node_ranges,
        file_doc_end,
        max_bytes,
    }
}

/// Module-level statements not covered by any node range become orphans and
/// are retrievable as fragments.
#[test]
fn test_orphan_module_level_statements_retrievable() {
    let code = "// file doc line\n\nconst MAX: u32 = 10;\n\nfn used() {}\n\n// standalone helper constant\nconst MIN: u32 = 1;\n";
    // `fn used` is covered by a Tier-1 node; the two consts are orphans.
    let fn_start = code.find("fn used").unwrap();
    let fn_end = fn_start + "fn used() {}".len();
    let node_ranges = [(fn_start, fn_end)];

    let orphans = orphan_fragments(orphan_input(code, &node_ranges, 18, 4096));

    // The leading file doc is excluded; the consts remain.
    assert!(!orphans.is_empty(), "expected orphan fragments");
    let combined: String = orphans.iter().map(|f| f.content).collect();
    assert!(
        combined.contains("const MAX"),
        "module const must be retrievable"
    );
    assert!(
        combined.contains("const MIN"),
        "module const must be retrievable"
    );
    assert!(
        combined.contains("standalone helper"),
        "orphan comment must be retrievable"
    );
    // The file doc region is excluded.
    assert!(
        !combined.contains("file doc line"),
        "file-doc region must be excluded from orphans"
    );
    // Byte ranges are re-based to file coordinates and contiguous per region.
    for f in &orphans {
        assert_eq!(f.content, &code[f.start_byte_index..f.end_byte_index]);
    }
}

/// A file whose leading region is entirely the file doc yields no orphan there.
#[test]
fn test_orphan_file_doc_region_excluded() {
    let code = "//! Module docs\n//! more docs\n\nfn f() {}\n";
    let fn_start = code.find("fn f").unwrap();
    let fn_end = fn_start + "fn f() {}".len();
    let doc_end = code.find("\n\nfn f").unwrap() + 1;
    let node_ranges = [(fn_start, fn_end)];

    let orphans = orphan_fragments(orphan_input(code, &node_ranges, doc_end, 4096));
    let combined: String = orphans.iter().map(|f| f.content).collect();
    assert!(
        !combined.contains("Module docs"),
        "file-doc region must not be double-indexed"
    );
    // fn is a node range so it is excluded; the file is otherwise fully covered
    // by the doc + node, leaving no orphan region behind.
    assert!(!combined.contains("fn f"));
}

/// When node ranges cover every byte (plus the doc region), no orphans remain.
#[test]
fn test_orphan_empty_complement_yields_zero_rows() {
    let code = "// doc\nfn f() {}\n";
    let doc_end = 6;
    let whole = (0usize, code.len());
    let node_ranges = [whole];
    let orphans = orphan_fragments(orphan_input(code, &node_ranges, doc_end, 4096));
    assert!(
        orphans.is_empty(),
        "fully covered file must yield zero orphans"
    );
}

/// Non-UTF-8 input produces zero orphan fragments instead of panicking.
#[test]
fn test_orphan_invalid_utf8_yields_zero_rows() {
    let input = OrphanInput {
        file_bytes: &[0xff, 0xfe, 0x00, 0x01],
        path: Path::new("binary.bin"),
        node_ranges: &[],
        file_doc_end: 0,
        max_bytes: 4096,
    };
    assert!(orphan_fragments(input).is_empty());
}

// ---------------------------------------------------------------------------
// Enrichment tests
// ---------------------------------------------------------------------------

#[test]
fn test_owner_header_format() {
    let header = owner_header("function", "rust", 3, 2, 7);
    assert_eq!(
        header,
        "// type:function lang:rust callers:3 callees:2 complexity:7"
    );
    // Connectivity caps match `enriched_node_content`.
    let capped = owner_header("function", "rust", 500, 500, 7);
    assert!(
        capped.contains("callers:50 callees:50"),
        "callers/callees capped at 50"
    );
}

#[test]
fn test_enrich_fragment_prepends_header_doc_and_symbol() {
    let code = "// Computes area.\nfn area() {\n    1\n}\n";
    let frag_start = code.find("fn area").unwrap();
    let fragment = Fragment {
        content: &code[frag_start..],
        start_line: 0,
        end_line: 2,
        start_byte_index: frag_start,
        end_byte_index: code.len(),
        file_path: Path::new("rect.rs"),
    };
    let header = owner_header("function", "rust", 0, 0, 1);
    let enriched = enrich_fragment(&fragment, code.as_bytes(), &header, "area");

    assert!(enriched.starts_with(&header), "header first");
    assert!(enriched.contains("// area in rect.rs"), "symbol line");
    assert!(enriched.contains("Computes area."), "stripped doc context");
    assert!(
        !enriched.contains("// Computes area."),
        "doc markers stripped"
    );
    assert!(
        enriched.ends_with(fragment.content),
        "fragment content last"
    );
}

#[test]
fn test_enrich_orphan_module_header() {
    let fragment = Fragment {
        content: "const MIN: u32 = 1;",
        start_line: 3,
        end_line: 3,
        start_byte_index: 0,
        end_byte_index: 18,
        file_path: Path::new("orphan_test.rs"),
    };
    let header = orphan_header("rust", Path::new("orphan_test.rs"));
    let enriched = enrich_orphan(&fragment, &header);
    assert_eq!(
        enriched,
        "// type:module lang:rust file:orphan_test.rs\nconst MIN: u32 = 1;"
    );
}
