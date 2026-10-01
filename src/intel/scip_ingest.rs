//! Compact, one-pass SCIP protobuf ingestion.

use flate2::read::GzDecoder;
use protobuf::Message;
use scip::types::{self, PositionEncoding, SymbolRole, occurrence};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Maximum accepted raw SCIP payload and decompressed gzip payload.
pub const MAX_SCIP_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Maximum accepted gzip-compressed SCIP payload.
pub const MAX_SCIP_COMPRESSED_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// The payload representation that exceeded an ingest limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScipPayloadKind {
    /// An uncompressed SCIP payload.
    Raw,
    /// A gzip-compressed SCIP payload before decompression.
    Compressed,
    /// The output produced while decompressing a gzip payload.
    Decompressed,
}

/// A definition projected out of SCIP's protobuf graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionFact {
    /// SCIP symbol identifier.
    pub symbol: String,
    /// Relative source path from the SCIP document.
    pub file_path: String,
    /// Optional half-open absolute UTF-8 byte range projected from the source.
    pub byte_range: Option<(usize, usize)>,
    /// Display or qualified name supplied by SCIP.
    pub qualified_name: String,
    /// Human-readable signature documentation.
    pub signature: Option<String>,
}

/// A relationship projected out of an occurrence or symbol relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipFact {
    /// Source SCIP symbol.
    pub source: String,
    /// Target SCIP symbol.
    pub target: String,
    /// `call`, `inheritance`, or `type_of`.
    pub kind: String,
}

/// Compact SCIP facts retained after protobuf structures are dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactScipFacts {
    /// Definition facts.
    pub definitions: Vec<DefinitionFact>,
    /// Relationship facts.
    pub relationships: Vec<RelationshipFact>,
    /// Number of definitions that could not be associated with a source range.
    pub definitions_without_range: usize,
    /// Number of non-definition occurrences whose symbol is unresolved in the
    /// index-wide symbol set. This is not a count of local-symbol references.
    pub unmatched_references: usize,
    /// Relative paths represented by the index.
    pub files: Vec<String>,
}

/// Errors produced while decoding a SCIP payload.
#[derive(Debug, thiserror::Error)]
pub enum ScipIngestError {
    /// The payload was not valid SCIP protobuf.
    #[error("invalid SCIP payload: {0}")]
    Decode(#[from] protobuf::Error),
    /// The compressed payload could not be decompressed or the input file could not be read.
    #[error("invalid gzip SCIP payload: {0}")]
    Gzip(#[from] io::Error),
    /// The input exceeded one of the ingest byte limits.
    #[error("{kind:?} SCIP payload is {size} bytes, over the {limit}-byte limit")]
    Oversized {
        /// Which representation exceeded its limit.
        kind: ScipPayloadKind,
        /// Observed payload size, possibly one byte over the limit.
        size: usize,
        /// Maximum accepted size.
        limit: usize,
    },
}

/// A reader that exposes at most `limit` bytes from an underlying stream.
///
/// The one-byte probe after the limit is important: it allows an input whose
/// size is exactly the limit while still detecting one additional byte without
/// retaining unbounded data.
struct LimitedReader<R> {
    inner: R,
    limit: usize,
    consumed: usize,
    exceeded: bool,
    io_error: Option<io::Error>,
}

impl<R> LimitedReader<R> {
    fn new(inner: R, limit: usize) -> Self {
        Self {
            inner,
            limit,
            consumed: 0,
            exceeded: false,
            io_error: None,
        }
    }

    fn take_io_error(&mut self) -> Option<io::Error> {
        self.io_error.take()
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }

        if self.consumed < self.limit {
            let remaining = self.limit - self.consumed;
            let read_len = buffer.len().min(remaining);
            let read_result = self.inner.read(&mut buffer[..read_len]);
            return match read_result {
                Ok(read) => {
                    self.consumed += read;
                    Ok(read)
                }
                Err(error) => {
                    self.io_error = Some(io::Error::new(error.kind(), error.to_string()));
                    Err(error)
                }
            };
        }

        // Probe the underlying stream at the boundary. Do not retain the
        // extra byte: the caller only needs to know that the limit was passed.
        let mut probe = [0_u8; 1];
        match self.inner.read(&mut probe) {
            Ok(0) => Ok(0),
            Ok(_) => {
                self.exceeded = true;
                Err(io::Error::other(
                    "SCIP payload exceeds configured byte limit",
                ))
            }
            Err(error) => {
                self.io_error = Some(io::Error::new(error.kind(), error.to_string()));
                Err(error)
            }
        }
    }
}

fn oversized(kind: ScipPayloadKind, size: usize, limit: usize) -> ScipIngestError {
    ScipIngestError::Oversized { kind, size, limit }
}

fn parse_raw_reader<R: Read>(
    reader: R,
    source_root: Option<&Path>,
) -> Result<CompactScipFacts, ScipIngestError> {
    let mut bounded = LimitedReader::new(reader, MAX_SCIP_PAYLOAD_BYTES);
    let parsed = types::Index::parse_from_reader(&mut bounded);
    if bounded.exceeded {
        return Err(oversized(
            ScipPayloadKind::Raw,
            MAX_SCIP_PAYLOAD_BYTES.saturating_add(1),
            MAX_SCIP_PAYLOAD_BYTES,
        ));
    }
    Ok(project_index(&parsed?, source_root))
}

fn parse_gzip_reader<R: Read>(
    reader: R,
    source_root: Option<&Path>,
) -> Result<CompactScipFacts, ScipIngestError> {
    let mut compressed = LimitedReader::new(reader, MAX_SCIP_COMPRESSED_PAYLOAD_BYTES);
    let (parsed, decompressed_exceeded, decompression_error) = {
        let mut decoder = GzDecoder::new(&mut compressed);
        let mut decompressed = LimitedReader::new(&mut decoder, MAX_SCIP_PAYLOAD_BYTES);
        let parsed = types::Index::parse_from_reader(&mut decompressed);
        (parsed, decompressed.exceeded, decompressed.take_io_error())
    };

    if decompressed_exceeded {
        return Err(oversized(
            ScipPayloadKind::Decompressed,
            MAX_SCIP_PAYLOAD_BYTES.saturating_add(1),
            MAX_SCIP_PAYLOAD_BYTES,
        ));
    }
    if compressed.exceeded {
        return Err(oversized(
            ScipPayloadKind::Compressed,
            MAX_SCIP_COMPRESSED_PAYLOAD_BYTES.saturating_add(1),
            MAX_SCIP_COMPRESSED_PAYLOAD_BYTES,
        ));
    }
    if let Some(error) = decompression_error {
        return Err(ScipIngestError::Gzip(error));
    }
    Ok(project_index(&parsed?, source_root))
}

/// Parse raw or gzip-compressed SCIP bytes and project them into compact facts.
pub fn ingest_bytes(bytes: &[u8], gzip: bool) -> Result<CompactScipFacts, ScipIngestError> {
    if gzip {
        if bytes.len() > MAX_SCIP_COMPRESSED_PAYLOAD_BYTES {
            return Err(oversized(
                ScipPayloadKind::Compressed,
                bytes.len(),
                MAX_SCIP_COMPRESSED_PAYLOAD_BYTES,
            ));
        }
        parse_gzip_reader(Cursor::new(bytes), None)
    } else {
        if bytes.len() > MAX_SCIP_PAYLOAD_BYTES {
            return Err(oversized(
                ScipPayloadKind::Raw,
                bytes.len(),
                MAX_SCIP_PAYLOAD_BYTES,
            ));
        }
        parse_raw_reader(Cursor::new(bytes), None)
    }
}

/// Parse a `.scip` file, accepting gzip based on its magic bytes or extension.
pub fn ingest_file(path: &Path) -> Result<CompactScipFacts, ScipIngestError> {
    ingest_file_from_root(path, None)
}

/// Parse a `.scip` file and resolve source-less SCIP ranges from `project_root`.
///
/// SCIP indexers commonly omit `Document.text`; in that case the project source
/// is loaded on demand using the document's relative path. The loader is bounded
/// by the same payload limit used for SCIP itself.
pub fn ingest_file_from_root(
    path: &Path,
    project_root: Option<&Path>,
) -> Result<CompactScipFacts, ScipIngestError> {
    let mut file = File::open(path).map_err(ScipIngestError::Gzip)?;
    let size = file.metadata().map_err(ScipIngestError::Gzip)?.len();
    let mut prefix = [0_u8; 2];
    let prefix_len = file.read(&mut prefix).map_err(ScipIngestError::Gzip)?;
    file.seek(SeekFrom::Start(0))
        .map_err(ScipIngestError::Gzip)?;
    let gzip = (prefix_len == prefix.len() && prefix == [0x1f, 0x8b])
        || path.extension().and_then(|e| e.to_str()) == Some("gz");
    let limit = if gzip {
        MAX_SCIP_COMPRESSED_PAYLOAD_BYTES
    } else {
        MAX_SCIP_PAYLOAD_BYTES
    };
    if size > limit as u64 {
        return Err(oversized(
            if gzip {
                ScipPayloadKind::Compressed
            } else {
                ScipPayloadKind::Raw
            },
            size.min(usize::MAX as u64) as usize,
            limit,
        ));
    }
    if gzip {
        parse_gzip_reader(file, project_root)
    } else {
        parse_raw_reader(file, project_root)
    }
}

fn push_symbol_relationships(
    symbol_name: &str,
    relationships: &[types::Relationship],
    out: &mut Vec<RelationshipFact>,
) {
    if symbol_name.is_empty() {
        return;
    }
    for relationship in relationships {
        if relationship.symbol.is_empty() {
            continue;
        }
        if relationship.is_implementation {
            out.push(RelationshipFact {
                source: symbol_name.to_string(),
                target: relationship.symbol.clone(),
                kind: "inheritance".to_string(),
            });
        }
        if relationship.is_type_definition {
            out.push(RelationshipFact {
                source: symbol_name.to_string(),
                target: relationship.symbol.clone(),
                kind: "type_of".to_string(),
            });
        }
        if relationship.is_reference {
            out.push(RelationshipFact {
                source: symbol_name.to_string(),
                target: relationship.symbol.clone(),
                kind: "call".to_string(),
            });
        }
    }
}

fn process_document_symbol(
    symbol: &types::SymbolInformation,
    document: &types::Document,
    source_text: Option<&str>,
    encoding: Option<PositionEncoding>,
    facts: &mut CompactScipFacts,
) {
    let signature = symbol
        .signature_documentation
        .as_ref()
        .map(|value| value.text.clone())
        .filter(|value| !value.is_empty());
    let range = document
        .occurrences
        .iter()
        .find(|occurrence| occurrence.symbol == symbol.symbol && is_definition(occurrence))
        .and_then(|occurrence| range_from_occurrence(occurrence, source_text?, encoding?));
    if range.is_none() {
        facts.definitions_without_range += 1;
    }
    facts.definitions.push(DefinitionFact {
        symbol: symbol.symbol.clone(),
        file_path: document.relative_path.clone(),
        byte_range: range,
        qualified_name: if symbol.display_name.is_empty() {
            symbol.symbol.clone()
        } else {
            symbol.display_name.clone()
        },
        signature,
    });
    push_symbol_relationships(
        &symbol.symbol,
        &symbol.relationships,
        &mut facts.relationships,
    );
}

fn count_unmatched_occurrences(
    occurrences: &[types::Occurrence],
    known_symbols: &HashSet<&str>,
) -> usize {
    occurrences
        .iter()
        .filter(|occurrence| {
            !occurrence.symbol.is_empty()
                && !is_definition(occurrence)
                && !known_symbols.contains(occurrence.symbol.as_str())
        })
        .count()
}

fn project_index(index: &types::Index, project_root: Option<&Path>) -> CompactScipFacts {
    let mut facts = CompactScipFacts::default();
    let known_symbols: HashSet<&str> = index
        .documents
        .iter()
        .flat_map(|document| document.symbols.iter())
        .map(|symbol| symbol.symbol.as_str())
        .filter(|symbol| !symbol.is_empty())
        .collect();

    for document in &index.documents {
        facts.files.push(document.relative_path.clone());
        let source_text = source_text_for_document(document, project_root);
        let source_text = source_text.as_deref();
        let encoding = document.position_encoding.enum_value().ok();
        for symbol in &document.symbols {
            process_document_symbol(symbol, document, source_text, encoding, &mut facts);
        }
        facts.unmatched_references +=
            count_unmatched_occurrences(&document.occurrences, &known_symbols);
    }
    facts.files.sort();
    facts.files.dedup();
    facts
}

fn is_definition(occurrence: &types::Occurrence) -> bool {
    occurrence.symbol_roles & (SymbolRole::Definition as i32) != 0
        || occurrence.symbol_roles & (SymbolRole::ForwardDefinition as i32) != 0
}

fn source_text_for_document<'a>(
    document: &'a types::Document,
    project_root: Option<&Path>,
) -> Option<Cow<'a, str>> {
    if !document.text.is_empty() {
        return Some(Cow::Borrowed(document.text.as_str()));
    }
    let root = project_root?;
    let relative = Path::new(&document.relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let source_path: PathBuf = root.join(relative);
    let metadata = std::fs::metadata(&source_path).ok()?;
    if metadata.len() > MAX_SCIP_PAYLOAD_BYTES as u64 {
        return None;
    }
    let mut source = String::with_capacity(metadata.len() as usize);
    File::open(source_path)
        .ok()?
        .read_to_string(&mut source)
        .ok()?;
    Some(Cow::Owned(source))
}

fn range_from_occurrence(
    occurrence: &types::Occurrence,
    text: &str,
    encoding: PositionEncoding,
) -> Option<(usize, usize)> {
    let (start_line, start_character, end_line, end_character) = occurrence_range(occurrence)?;
    let start = position_to_byte_offset(text, encoding, start_line, start_character)?;
    let end = position_to_byte_offset(text, encoding, end_line, end_character)?;
    (start <= end).then_some((start, end))
}

fn occurrence_range(occurrence: &types::Occurrence) -> Option<(i32, i32, i32, i32)> {
    // The typed oneof takes precedence over the deprecated range field, even
    // when the typed value is malformed. Falling back in that case would make
    // the same occurrence mean two different things to different consumers.
    if let Some(typed_range) = &occurrence.typed_range {
        return match typed_range {
            occurrence::Typed_range::SingleLineRange(range) => Some((
                range.line,
                range.start_character,
                range.line,
                range.end_character,
            )),
            occurrence::Typed_range::MultiLineRange(range) => Some((
                range.start_line,
                range.start_character,
                range.end_line,
                range.end_character,
            )),
            _ => None,
        };
    }

    match occurrence.range.as_slice() {
        [start_line, start_character, end_character] => {
            Some((*start_line, *start_character, *start_line, *end_character))
        }
        [start_line, start_character, end_line, end_character] => {
            Some((*start_line, *start_character, *end_line, *end_character))
        }
        _ => None,
    }
}

fn position_to_byte_offset(
    text: &str,
    encoding: PositionEncoding,
    line: i32,
    character: i32,
) -> Option<usize> {
    if line < 0 || character < 0 {
        return None;
    }
    let character = character as usize;
    let (line_start, line_end) = source_line_bounds(text, line as usize)?;
    let line_text = text.get(line_start..line_end)?;

    match encoding {
        PositionEncoding::UTF8CodeUnitOffsetFromLineStart => line_text
            .is_char_boundary(character)
            .then_some(line_start + character),
        PositionEncoding::UTF16CodeUnitOffsetFromLineStart => {
            let mut units = 0;
            for (byte_offset, value) in line_text.char_indices() {
                if units == character {
                    return Some(line_start + byte_offset);
                }
                units += value.len_utf16();
            }
            (units == character).then_some(line_end)
        }
        PositionEncoding::UTF32CodeUnitOffsetFromLineStart => {
            let mut units = 0;
            for (byte_offset, _) in line_text.char_indices() {
                if units == character {
                    return Some(line_start + byte_offset);
                }
                units += 1;
            }
            (units == character).then_some(line_end)
        }
        PositionEncoding::UnspecifiedPositionEncoding => None,
    }
}

fn source_line_bounds(text: &str, line: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut line_start = 0;
    let mut current_line = 0;
    for (offset, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        if current_line == line {
            let mut line_end = offset;
            if line_end > line_start && bytes[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            return Some((line_start, line_end));
        }
        current_line += 1;
        line_start = offset + 1;
    }
    (current_line == line).then_some((line_start, text.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::Message;

    #[test]
    fn test_ingest_projects_definition_and_type_relationship() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        document.text = "xxFoo\n".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust-analyzer pkg 1.0 src/lib.rs/Foo#".to_string();
        symbol.display_name = "Foo".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.range = vec![0, 2, 0, 5];
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.definitions.len(), 1);
        assert_eq!(facts.definitions[0].byte_range, Some((2, 5)));
    }

    #[test]
    fn test_ingest_projects_typed_multiline_range_to_utf8_bytes() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        document.text = "αx\n🙂β\n".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust pkg 1 src/lib.rs/Foo#".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.set_multi_line_range(types::MultiLineRange {
            start_line: 0,
            start_character: 2,
            end_line: 1,
            end_character: 6,
            ..types::MultiLineRange::new()
        });
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.definitions[0].byte_range, Some((2, 10)));
    }

    #[test]
    fn test_ingest_maps_utf16_unicode_positions_to_utf8_bytes() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        document.text = "a😀é\n".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF16CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust pkg 1 src/lib.rs/Foo#".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.set_single_line_range(types::SingleLineRange {
            line: 0,
            start_character: 1,
            end_character: 3,
            ..types::SingleLineRange::new()
        });
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.definitions[0].byte_range, Some((1, 5)));
    }

    #[test]
    fn test_ingest_maps_utf8_unicode_positions_to_utf8_bytes() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        document.text = "éx\n".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust pkg 1 src/lib.rs/Foo#".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.set_single_line_range(types::SingleLineRange {
            line: 0,
            start_character: 0,
            end_character: 2,
            ..types::SingleLineRange::new()
        });
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.definitions[0].byte_range, Some((0, 2)));
    }

    #[test]
    fn test_ingest_maps_utf32_unicode_positions_to_utf8_bytes() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        document.text = "a😀é\n".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF32CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust pkg 1 src/lib.rs/Foo#".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.set_single_line_range(types::SingleLineRange {
            line: 0,
            start_character: 1,
            end_character: 3,
            ..types::SingleLineRange::new()
        });
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.definitions[0].byte_range, Some((1, 7)));
    }

    #[test]
    fn test_ingest_prefers_typed_range_over_legacy_range() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        document.text = "abcdef\n".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust pkg 1 src/lib.rs/Foo#".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.range = vec![0, 4, 0, 6];
        occurrence.set_single_line_range(types::SingleLineRange {
            line: 0,
            start_character: 1,
            end_character: 3,
            ..types::SingleLineRange::new()
        });
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.definitions[0].byte_range, Some((1, 3)));
    }

    #[test]
    fn test_ingest_resolves_source_less_ranges_from_project_root() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("src/lib.rs");
        std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        std::fs::write(&source_path, "xxFoo\n").unwrap();

        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust pkg 1 src/lib.rs/Foo#".to_string();
        symbol.display_name = "Foo".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.range = vec![0, 2, 0, 5];
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let scip_path = directory.path().join("index.scip");
        std::fs::write(&scip_path, index.write_to_bytes().unwrap()).unwrap();
        let facts = ingest_file_from_root(&scip_path, Some(directory.path())).unwrap();
        assert_eq!(facts.definitions[0].byte_range, Some((2, 5)));
    }

    #[test]
    fn test_ingest_projects_relationship_reference_as_call_fact() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        let mut source = types::SymbolInformation::new();
        source.symbol = "rust pkg 1 src/lib.rs/source#".to_string();
        let mut target = types::SymbolInformation::new();
        target.symbol = "rust pkg 1 src/lib.rs/target#".to_string();
        let mut relationship = types::Relationship::new();
        relationship.symbol = target.symbol.clone();
        relationship.is_reference = true;
        source.relationships.push(relationship);
        document.symbols.extend([source, target]);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(
            facts.relationships,
            vec![RelationshipFact {
                source: "rust pkg 1 src/lib.rs/source#".to_string(),
                target: "rust pkg 1 src/lib.rs/target#".to_string(),
                kind: "call".to_string(),
            }]
        );
    }

    #[test]
    fn test_ingest_does_not_project_columns_without_source_text() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "rust pkg 1 src/lib.rs/Foo#".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.range = vec![3, 2, 4, 5];
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.definitions[0].byte_range, None);
        assert_eq!(facts.definitions_without_range, 1);
    }

    #[test]
    fn test_ingest_counts_only_symbols_unresolved_by_global_symbol_set() {
        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/lib.rs".to_string();
        let mut known = types::SymbolInformation::new();
        known.symbol = "local known".to_string();
        document.symbols.push(known);

        let mut known_reference = types::Occurrence::new();
        known_reference.symbol = "local known".to_string();
        document.occurrences.push(known_reference);
        let mut unresolved_reference = types::Occurrence::new();
        unresolved_reference.symbol = "local missing".to_string();
        document.occurrences.push(unresolved_reference);
        index.documents.push(document);

        let facts = ingest_bytes(&index.write_to_bytes().unwrap(), false).unwrap();
        assert_eq!(facts.unmatched_references, 1);
    }

    #[test]
    fn test_ingest_gzip_payload() {
        let index = types::Index::new();
        let raw = index.write_to_bytes().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &raw).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(
            ingest_bytes(&compressed, true).unwrap(),
            CompactScipFacts::default()
        );
    }

    #[test]
    fn test_ingest_rejects_oversized_raw_payload() {
        let payload = oversized_unknown_field_payload();

        assert!(matches!(
            ingest_bytes(&payload, false),
            Err(ScipIngestError::Oversized {
                kind: ScipPayloadKind::Raw,
                ..
            })
        ));
    }

    #[test]
    fn test_ingest_rejects_oversized_gzip_payload() {
        let raw = oversized_unknown_field_payload();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &raw).unwrap();
        let compressed = encoder.finish().unwrap();

        assert!(matches!(
            ingest_bytes(&compressed, true),
            Err(ScipIngestError::Oversized {
                kind: ScipPayloadKind::Decompressed,
                ..
            })
        ));
    }

    fn oversized_unknown_field_payload() -> Vec<u8> {
        let mut payload = vec![0xa2, 0x06]; // field 100, length-delimited
        append_varint(&mut payload, (MAX_SCIP_PAYLOAD_BYTES + 1) as u64);
        payload.resize(payload.len() + MAX_SCIP_PAYLOAD_BYTES + 1, 0);
        payload
    }

    fn append_varint(output: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            output.push((value as u8) | 0x80);
            value >>= 7;
        }
        output.push(value as u8);
    }
}
