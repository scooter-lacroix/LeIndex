//! Documentation parser: markdown / reStructuredText / AsciiDoc / plain text
//! as first-class indexed content.
//!
//! Doc files become heading-section "symbols": each `#`/`##` heading (or RST
//! underline heading) produces one `SignatureInfo` whose byte range spans the
//! section body, so documentation rides the SAME lexical + neural pipeline
//! as code — a query like "architecture decisions" ranks ARCHITECTURE.md
//! sections instead of missing them because they are not code.
//!
//! The section kind is carried through `return_type = "doc_section"` (the
//! existing kind channel), which the graph extractor maps to
//! `NodeType::DocSection`.

use crate::parse::traits::{
    Block, CodeIntelligence, ComplexityMetrics, Edge, Error, Graph, Result, SignatureInfo,
};
use tree_sitter::Parser;

/// Per-file byte cap: a single runaway changelog must not dominate the doc
/// corpus (the corpus-level cap lives in the scan gate).
const MAX_DOC_FILE_BYTES: usize = 1024 * 1024;

/// Documentation parser for markdown-family and plain-text files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFlavor {
    /// Markdown (.md / .markdown) — parsed with pulldown-cmark.
    Markdown,
    /// reStructuredText (.rst) — underline/overline headings.
    Rst,
    /// AsciiDoc (.adoc) — `= Title` style headings.
    Adoc,
    /// Plain text (.txt) — whole file as one section.
    Plain,
}

impl DocFlavor {
    /// Pick the flavor for a file extension.
    pub fn for_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "md" | "markdown" => Some(Self::Markdown),
            "rst" => Some(Self::Rst),
            "adoc" | "asciidoc" => Some(Self::Adoc),
            "txt" => Some(Self::Plain),
            _ => None,
        }
    }
}

/// Documentation parser producing heading-section signatures.
pub struct DocParser {
    flavor: DocFlavor,
}

impl DocParser {
    /// Create a doc parser for the given flavor.
    pub fn new(flavor: DocFlavor) -> Self {
        Self { flavor }
    }
}

impl Default for DocParser {
    fn default() -> Self {
        Self::new(DocFlavor::Markdown)
    }
}

impl CodeIntelligence for DocParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        if source.len() > MAX_DOC_FILE_BYTES {
            // Oversized doc: index just the first chunk's headings rather
            // than dropping the file entirely. The cut retreats to a UTF-8
            // character boundary — slicing mid-codepoint would fail the
            // from_utf8 conversion below and drop the document after all.
            let mut end = source.len().min(MAX_DOC_FILE_BYTES);
            // Retreat past any UTF-8 continuation bytes (0b10xxxxxx) so the
            // slice ends on a character boundary.
            while end > 0 && source[end] & 0xC0 == 0x80 {
                end -= 1;
            }
            let truncated = &source[..end];
            return self.get_signatures(truncated);
        }
        let text = std::str::from_utf8(source)
            .map_err(|e| Error::ParseFailed(format!("doc file is not UTF-8: {e}")))?;
        let sections = match self.flavor {
            DocFlavor::Markdown => markdown_sections(text),
            DocFlavor::Rst => rst_sections(text),
            DocFlavor::Adoc => adoc_sections(text),
            DocFlavor::Plain => plain_sections(text),
        };
        Ok(sections
            .into_iter()
            .map(|(name, byte_range, summary)| SignatureInfo {
                name: sanitize_name(&name, &byte_range),
                qualified_name: name,
                parameters: Vec::new(),
                return_type: Some("doc_section".to_string()),
                visibility: crate::parse::traits::Visibility::Public,
                is_async: false,
                is_method: false,
                docstring: summary,
                calls: Vec::new(),
                imports: Vec::new(),
                byte_range,
                cyclomatic_complexity: 1,
                flow_facts: Vec::new(),
            })
            .collect())
    }

    fn get_signatures_with_parser(
        &self,
        source: &[u8],
        _parser: &mut Parser,
    ) -> Result<Vec<SignatureInfo>> {
        // Doc parsing never uses tree-sitter; the parser param is part of
        // the trait surface only.
        self.get_signatures(source)
    }

    fn compute_cfg(&self, _source: &[u8], _node_id: usize) -> Result<Graph<Block, Edge>> {
        Ok(Graph {
            blocks: vec![],
            edges: vec![],
            entry_block: 0,
            exit_blocks: vec![],
        })
    }

    fn extract_complexity(&self, _node: &tree_sitter::Node<'_>) -> ComplexityMetrics {
        ComplexityMetrics {
            cyclomatic: 1,
            nesting_depth: 0,
            line_count: 0,
            token_count: 0,
        }
    }
}

/// (heading text, section byte range, first-paragraph summary).
type Section = (String, (usize, usize), Option<String>);

/// Markdown headings via pulldown-cmark's offset iterator — exact byte
/// ranges, no regex drift.
fn markdown_sections(text: &str) -> Vec<Section> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(text, options);

    let mut sections: Vec<Section> = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut title = String::new();
    let mut in_heading = false;

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                if let Some(start) = current_start.take() {
                    sections.push((std::mem::take(&mut title), (start, range.start), None));
                }
                title.clear();
                in_heading = true;
                current_start = Some(range.start);
            }
            Event::Text(chunk) => {
                if in_heading && title.len() < 200 {
                    title.push_str(&chunk);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                in_heading = false;
            }
            _ => {}
        }
    }
    if let Some(start) = current_start {
        sections.push((title, (start, text.len()), None));
    }

    // Fill first-paragraph summaries from the section bodies.
    for (title, (start, end), summary_slot) in &mut sections {
        let _ = title;
        let body = &text.get(*start..*end).unwrap_or("");
        *summary_slot = first_paragraph(body, title.len());
    }
    sections
}

/// Byte offset of the line following `start`: the line's length plus its
/// real terminator. `str::lines` strips `\r\n` but reports only the line
/// body, so advancing by `line.len() + 1` drifts one byte per preceding
/// CRLF and shifts every later section range into the previous line.
fn next_line_start(text: &str, start: usize) -> usize {
    match text[start..].find('\n') {
        Some(nl) => start + nl + 1,
        None => text.len(),
    }
}

/// RST underline headings: `Title\n=====` (and overline forms).
fn rst_sections(text: &str) -> Vec<Section> {
    let lines: Vec<(usize, &str)> = text
        .lines()
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset = next_line_start(text, start);
            Some((start, line))
        })
        .collect();
    let mut sections: Vec<Section> = Vec::new();
    let mut i = 1;
    while i < lines.len() {
        let (pos, line) = lines[i];
        let trimmed = line.trim_end();
        let is_underline = !trimmed.is_empty()
            && trimmed.len() >= 3
            && trimmed
                .chars()
                .all(|c| matches!(c, '=' | '-' | '~' | '^' | '"' | '\''));
        if is_underline {
            let (prev_pos, prev_line) = lines[i - 1];
            let title = prev_line.trim();
            if !title.is_empty()
                && !title
                    .chars()
                    .all(|c| matches!(c, '=' | '-' | '~' | '^' | '"' | '\''))
            {
                sections.push((title.to_string(), (prev_pos, text.len()), None));
            }
        }
        let _ = pos;
        i += 1;
    }
    // Each section ends where the next section's title begins.
    let starts: Vec<usize> = sections.iter().map(|(_, range, _)| range.0).collect();
    for (section, next_start) in sections.iter_mut().zip(starts.iter().skip(1)) {
        section.1.1 = *next_start;
    }
    if let Some(last) = sections.last_mut() {
        last.1.1 = text.len();
        let body = &text.get(last.1.0..).unwrap_or("");
        last.2 = first_paragraph(body, 0);
    }
    sections
}

/// AsciiDoc `= Title` / `== Section` headings.
fn adoc_sections(text: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut offset = 0usize;
    for line in text.lines() {
        let start = offset;
        offset = next_line_start(text, offset);
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|&c| c == '=').count();
        if level >= 1 && trimmed.len() > level && trimmed.as_bytes()[level] == b' ' {
            let title = trimmed[level + 1..].trim();
            if !title.is_empty() {
                sections.push((title.to_string(), (start, text.len()), None));
            }
        }
    }
    let starts: Vec<usize> = sections.iter().map(|(_, range, _)| range.0).collect();
    for (section, next_start) in sections.iter_mut().zip(starts.iter().skip(1)) {
        section.1.1 = *next_start;
    }
    if let Some(last) = sections.last_mut() {
        last.1.1 = text.len();
        let body = &text.get(last.1.0..).unwrap_or("");
        last.2 = first_paragraph(body, 0);
    }
    sections
}

/// Plain text: the whole file as one section titled by the first
/// non-empty line (or `document`).
fn plain_sections(text: &str) -> Vec<Section> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let title = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("document")
        .chars()
        .take(120)
        .collect::<String>();
    vec![(title, (0, text.len()), first_paragraph(text, 0))]
}

/// First paragraph of a section body as the docstring (doc-comment
/// convention), skipping the heading itself.
fn first_paragraph(body: &str, _heading_len: usize) -> Option<String> {
    // The section body starts after the heading's terminating blank line.
    let after = match body.find("\n\n") {
        Some(blank) => &body[blank + 2..],
        None => body
            .strip_prefix('#')
            .map(|rest| rest.trim_start())
            .unwrap_or(body),
    };
    let paragraph: String = after
        .trim_start()
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed: String = paragraph.trim().chars().take(500).collect();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Heading text can contain arbitrary markdown; node names must stay
/// file-system-and-index friendly.
fn sanitize_name(raw: &str, byte_range: &(usize, usize)) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '-' | ' ' | '.' | '/') {
                c
            } else {
                ' '
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join("_");
    if collapsed.is_empty() {
        format!("section_{}", byte_range.0)
    } else {
        collapsed.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markdown_sections_have_exact_byte_ranges() {
        let text = "# Title\n\nintro paragraph.\n\n## Section A\n\ncontent a\n\n## Section B\n\ncontent b\n";
        let sections = markdown_sections(text);
        assert_eq!(sections.len(), 3, "{sections:?}");
        assert_eq!(sections[0].0, "Title");
        assert_eq!(sections[1].0, "Section A");
        assert_eq!(sections[2].0, "Section B");
        // Section A spans from its heading to Section B's heading.
        assert_eq!(
            text[sections[1].1.0..sections[1].1.1].trim(),
            "## Section A\n\ncontent a"
        );
        assert_eq!(sections[1].2.as_deref(), Some("content a"));
    }

    #[test]
    fn test_plain_text_whole_file_section() {
        let sections = plain_sections("My Notes\n\nsome body\n");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "My Notes");
        assert_eq!(sections[0].1, (0, 20));
    }

    #[test]
    fn test_doc_parser_signatures_carry_kind() {
        let parser = DocParser::new(DocFlavor::Markdown);
        let signatures = parser
            .get_signatures(b"# Architecture\n\nLeIndex has layers.\n")
            .unwrap();
        assert_eq!(signatures.len(), 1);
        assert_eq!(signatures[0].name, "Architecture");
        assert_eq!(signatures[0].return_type.as_deref(), Some("doc_section"));
        assert_eq!(
            signatures[0].docstring.as_deref(),
            Some("LeIndex has layers.")
        );
    }

    #[test]
    fn test_flavor_for_extension() {
        assert_eq!(DocFlavor::for_extension("md"), Some(DocFlavor::Markdown));
        assert_eq!(DocFlavor::for_extension("RST"), Some(DocFlavor::Rst));
        assert_eq!(DocFlavor::for_extension("txt"), Some(DocFlavor::Plain));
        assert_eq!(DocFlavor::for_extension("rs"), None);
    }

    #[test]
    fn test_oversized_doc_truncates_not_drops() {
        let mut big = vec![b'#'; 64];
        big.resize(64 + 1_100_000, b'\n');
        let parser = DocParser::new(DocFlavor::Plain);
        // Plain flavor returns one section regardless; the guarantee under
        // test is that oversized input does not error.
        assert!(parser.get_signatures(&big).is_ok());
    }

    /// Oversized docs are truncated at a UTF-8 character boundary (round-9
    /// Codex P2): a 1 MiB cut landing mid-codepoint used to fail the UTF-8
    /// conversion and drop the document the truncation existed to save.
    #[test]
    fn test_oversized_doc_truncates_at_a_character_boundary() {
        let parser = DocParser::new(DocFlavor::Markdown);
        // 'é' is 2 bytes; place one straddling the 1 MiB cut point.
        let mut body = vec![b'#'; MAX_DOC_FILE_BYTES - 1];
        body.extend_from_slice("é\n# Tail heading\n".as_bytes());
        assert!(body.len() > MAX_DOC_FILE_BYTES);
        assert_eq!(
            body[MAX_DOC_FILE_BYTES - 1],
            0xC3,
            "cut lands mid-codepoint"
        );
        let signatures = parser
            .get_signatures(&body)
            .expect("truncation must not fail UTF-8 validation");
        let _ = signatures;
    }

    /// Section ranges stay byte-exact under CRLF line endings (round-9
    /// Codex P2): `str::lines` strips \r\n while the offset walk advanced by
    /// only one byte, shifting every later heading into the previous line.
    #[test]
    fn test_rst_and_adoc_section_ranges_survive_crlf() {
        let rst =
            "Title One\r\n=====\r\n\r\nintro text\r\n\r\nTitle Two\r\n=====\r\n\r\nbody two\r\n";
        let sections = rst_sections(rst);
        assert!(
            sections.len() >= 2,
            "both headings detected under CRLF, got {sections:?}"
        );
        assert_eq!(sections[0].0, "Title One");
        assert_eq!(sections[1].0, "Title Two");
        for (name, (start, end), _) in &sections {
            assert!(
                end > start && *end <= rst.len(),
                "section {name} range ({start},{end}) out of bounds/empty"
            );
            let slice = &rst[*start..*end];
            assert!(
                slice.contains(name),
                "section {name} must start at its own title, got: {slice:?}"
            );
        }

        let adoc = "= Title One\r\n\r\nintro\r\n\r\n== Title Two\r\n\r\nbody two\r\n";
        let sections = adoc_sections(adoc);
        assert!(
            sections.len() >= 2,
            "both adoc headings under CRLF: {sections:?}"
        );
        for (name, (start, end), _) in &sections {
            assert!(end > start && *end <= adoc.len());
            let slice = &adoc[*start..*end];
            assert!(
                slice.contains(name),
                "adoc section {name} must start at its own title, got: {slice:?}"
            );
        }
    }
}
