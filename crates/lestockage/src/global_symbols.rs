// Global symbol table for cross-project resolution
//
// This module provides the data structures and operations for managing
// symbols across multiple projects, enabling cross-project symbol resolution.

use crate::schema::Storage;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Global symbol identifier (BLAKE3 hash)
pub type GlobalSymbolId = String;

/// Global symbol record
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalSymbol {
    /// Unique identifier for the symbol
    pub symbol_id: GlobalSymbolId,
    /// ID of the project containing the symbol
    pub project_id: String,
    /// Name of the symbol
    pub symbol_name: String,
    /// Type of the symbol (function, class, etc.)
    pub symbol_type: SymbolType,
    /// Code signature or declaration
    pub signature: Option<String>,
    /// Path to the file containing the symbol
    pub file_path: String,
    /// Byte range in the source file
    pub byte_range: (usize, usize),
    /// Complexity score of the symbol
    pub complexity: u32,
    /// Whether the symbol is publicly accessible
    pub is_public: bool,
}

/// Symbol type classification
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SymbolType {
    /// A function definition
    Function,
    /// A class definition
    Class,
    /// A method definition
    Method,
    /// A variable definition
    Variable,
    /// A module or file
    Module,
    /// A struct definition
    Struct,
    /// An enum definition
    Enum,
    /// A trait or interface definition
    Trait,
}

impl SymbolType {
    /// Return the string representation of the symbol type.
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolType::Function => "function",
            SymbolType::Class => "class",
            SymbolType::Method => "method",
            SymbolType::Variable => "variable",
            SymbolType::Module => "module",
            SymbolType::Struct => "struct",
            SymbolType::Enum => "enum",
            SymbolType::Trait => "trait",
        }
    }

    /// Create a symbol type from its string representation.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "function" => Some(SymbolType::Function),
            "class" => Some(SymbolType::Class),
            "method" => Some(SymbolType::Method),
            "variable" => Some(SymbolType::Variable),
            "module" => Some(SymbolType::Module),
            "struct" => Some(SymbolType::Struct),
            "enum" => Some(SymbolType::Enum),
            "trait" => Some(SymbolType::Trait),
            _ => None,
        }
    }
}

/// External reference between symbols
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalRef {
    /// Unique identifier for the reference
    pub ref_id: String,
    /// ID of the project containing the source symbol
    pub source_project_id: String,
    /// ID of the source symbol
    pub source_symbol_id: GlobalSymbolId,
    /// ID of the project containing the target symbol
    pub target_project_id: String,
    /// ID of the target symbol
    pub target_symbol_id: GlobalSymbolId,
    /// Type of the reference (call, inheritance, etc.)
    pub ref_type: RefType,
}

/// Reference type between symbols
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RefType {
    /// Function or method call
    Call,
    /// Class or interface inheritance
    Inheritance,
    /// Module or file import
    Import,
    /// General usage reference
    Use,
    /// Data dependency
    DataDependency,
}

impl RefType {
    /// Return the string representation of the reference type.
    pub fn as_str(&self) -> &'static str {
        match self {
            RefType::Call => "call",
            RefType::Inheritance => "inheritance",
            RefType::Import => "import",
            RefType::Use => "use",
            RefType::DataDependency => "data_dependency",
        }
    }

    /// Create a reference type from its string representation.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "call" => Some(RefType::Call),
            "inheritance" => Some(RefType::Inheritance),
            "import" => Some(RefType::Import),
            "use" => Some(RefType::Use),
            "data_dependency" => Some(RefType::DataDependency),
            _ => None,
        }
    }
}

/// Project dependency
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectDep {
    /// Unique identifier for the dependency
    pub dep_id: String,
    /// ID of the project that has the dependency
    pub project_id: String,
    /// ID of the project that is depended upon
    pub depends_on_project_id: String,
    /// Type of the dependency (direct, dev, etc.)
    pub dependency_type: DepType,
}

/// Dependency type between projects
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DepType {
    /// Direct production dependency
    Direct,
    /// Transitive dependency (dependency of a dependency)
    Transitive,
    /// Development-only dependency
    Dev,
    /// Build-time dependency
    Build,
}

impl DepType {
    /// Return the string representation of the dependency type.
    pub fn as_str(&self) -> &'static str {
        match self {
            DepType::Direct => "direct",
            DepType::Transitive => "transitive",
            DepType::Dev => "dev",
            DepType::Build => "build",
        }
    }

    /// Create a dependency type from its string representation.
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(DepType::Direct),
            "transitive" => Some(DepType::Transitive),
            "dev" => Some(DepType::Dev),
            "build" => Some(DepType::Build),
            _ => None,
        }
    }
}

/// Global symbol table operations
pub struct GlobalSymbolTable<'a> {
    db: &'a Storage,
}

impl<'a> GlobalSymbolTable<'a> {
    /// Create new global symbol table
    pub fn new(db: &'a Storage) -> Self {
        Self { db }
    }

    /// Get the underlying storage
    pub fn storage(&self) -> &Storage {
        self.db
    }

    /// Generate a unique symbol ID using BLAKE3 hash
    pub fn generate_symbol_id(
        project_id: &str,
        symbol_name: &str,
        signature: Option<&str>,
    ) -> GlobalSymbolId {
        use blake3::Hasher;
        let mut hasher = Hasher::new();
        hasher.update(project_id.as_bytes());
        hasher.update(b"::");
        hasher.update(symbol_name.as_bytes());
        if let Some(sig) = signature {
            hasher.update(sig.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    /// Insert or update a global symbol
    pub fn upsert_symbol(&self, symbol: &GlobalSymbol) -> Result<(), GlobalSymbolError> {
        let byte_start = symbol.byte_range.0 as i64;
        let byte_end = symbol.byte_range.1 as i64;
        let is_public = if symbol.is_public { 1 } else { 0 };

        self.db
            .conn()
            .execute(
                "INSERT INTO global_symbols (
                symbol_id, project_id, symbol_name, symbol_type,
                signature, file_path, byte_range_start, byte_range_end,
                complexity, is_public
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(project_id, symbol_name, signature) DO UPDATE SET
                file_path = excluded.file_path,
                byte_range_start = excluded.byte_range_start,
                byte_range_end = excluded.byte_range_end,
                complexity = excluded.complexity,
                is_public = excluded.is_public",
                params![
                    &symbol.symbol_id,
                    &symbol.project_id,
                    &symbol.symbol_name,
                    symbol.symbol_type.as_str(),
                    &symbol.signature,
                    &symbol.file_path,
                    byte_start,
                    byte_end,
                    symbol.complexity as i64,
                    is_public,
                ],
            )
            .map_err(GlobalSymbolError::from)?;

        Ok(())
    }

    /// Batch insert symbols
    pub fn upsert_symbols_batch(&self, symbols: &[GlobalSymbol]) -> Result<(), GlobalSymbolError> {
        // Use rusqlite's transaction method which can be called on &Connection
        self.db.conn().execute("BEGIN IMMEDIATE TRANSACTION", [])?;

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for symbol in symbols {
                let byte_start = symbol.byte_range.0 as i64;
                let byte_end = symbol.byte_range.1 as i64;
                let is_public = if symbol.is_public { 1 } else { 0 };

                if let Err(e) = self.db.conn().execute(
                    "INSERT INTO global_symbols (
                        symbol_id, project_id, symbol_name, symbol_type,
                        signature, file_path, byte_range_start, byte_range_end,
                        complexity, is_public
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                    ON CONFLICT(project_id, symbol_name, signature) DO UPDATE SET
                        file_path = excluded.file_path,
                        byte_range_start = excluded.byte_range_start,
                        byte_range_end = excluded.byte_range_end,
                        complexity = excluded.complexity,
                        is_public = excluded.is_public",
                    params![
                        &symbol.symbol_id,
                        &symbol.project_id,
                        &symbol.symbol_name,
                        symbol.symbol_type.as_str(),
                        &symbol.signature,
                        &symbol.file_path,
                        byte_start,
                        byte_end,
                        symbol.complexity as i64,
                        is_public,
                    ],
                ) {
                    self.db.conn().execute("ROLLBACK", []).ok();
                    return Err::<(), rusqlite::Error>(e);
                }
            }
            Result::<(), rusqlite::Error>::Ok(())
        }));

        match result {
            Ok(inner) => inner.map_err(GlobalSymbolError::from),
            Err(_) => {
                self.db.conn().execute("ROLLBACK", []).ok();
                Err(GlobalSymbolError::Sqlite(
                    rusqlite::Error::ExecuteReturnedResults,
                ))
            }
        }?;

        self.db
            .conn()
            .execute("COMMIT", [])
            .map_err(GlobalSymbolError::from)?;
        Ok(())
    }

    /// Resolve symbol by name (returns all matches across projects)
    pub fn resolve_by_name(&self, name: &str) -> Result<Vec<GlobalSymbol>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT symbol_id, project_id, symbol_name, symbol_type, signature,
                    file_path, byte_range_start, byte_range_end, complexity, is_public
             FROM global_symbols
             WHERE symbol_name = ?1",
            )
            .map_err(GlobalSymbolError::from)?;

        let symbols = stmt
            .query_map(params![name], |row| {
                Ok(GlobalSymbol {
                    symbol_id: row.get(0)?,
                    project_id: row.get(1)?,
                    symbol_name: row.get(2)?,
                    symbol_type: SymbolType::from_str_name(row.get::<_, String>(3)?.as_str())
                        .unwrap_or(SymbolType::Function),
                    signature: row.get(4)?,
                    file_path: row.get(5)?,
                    byte_range: (
                        row.get::<_, i64>(6)? as usize,
                        row.get::<_, i64>(7)? as usize,
                    ),
                    complexity: row.get::<_, i64>(8)? as u32,
                    is_public: row.get::<_, i64>(9)? == 1,
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(symbols)
    }

    /// Resolve symbol by name and type
    pub fn resolve_by_name_and_type(
        &self,
        name: &str,
        symbol_type: SymbolType,
    ) -> Result<Vec<GlobalSymbol>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT symbol_id, project_id, symbol_name, symbol_type, signature,
                    file_path, byte_range_start, byte_range_end, complexity, is_public
             FROM global_symbols
             WHERE symbol_name = ?1 AND symbol_type = ?2",
            )
            .map_err(GlobalSymbolError::from)?;

        let symbols = stmt
            .query_map(params![name, symbol_type.as_str()], |row| {
                Ok(GlobalSymbol {
                    symbol_id: row.get(0)?,
                    project_id: row.get(1)?,
                    symbol_name: row.get(2)?,
                    symbol_type: SymbolType::from_str_name(row.get::<_, String>(3)?.as_str())
                        .unwrap_or(SymbolType::Function),
                    signature: row.get(4)?,
                    file_path: row.get(5)?,
                    byte_range: (
                        row.get::<_, i64>(6)? as usize,
                        row.get::<_, i64>(7)? as usize,
                    ),
                    complexity: row.get::<_, i64>(8)? as u32,
                    is_public: row.get::<_, i64>(9)? == 1,
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(symbols)
    }

    /// Get symbol by ID
    pub fn get_symbol(
        &self,
        symbol_id: &GlobalSymbolId,
    ) -> Result<Option<GlobalSymbol>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT symbol_id, project_id, symbol_name, symbol_type, signature,
                    file_path, byte_range_start, byte_range_end, complexity, is_public
             FROM global_symbols
             WHERE symbol_id = ?1",
            )
            .map_err(GlobalSymbolError::from)?;

        let result = stmt
            .query_row(params![symbol_id], |row| {
                Ok(GlobalSymbol {
                    symbol_id: row.get(0)?,
                    project_id: row.get(1)?,
                    symbol_name: row.get(2)?,
                    symbol_type: SymbolType::from_str_name(row.get::<_, String>(3)?.as_str())
                        .unwrap_or(SymbolType::Function),
                    signature: row.get(4)?,
                    file_path: row.get(5)?,
                    byte_range: (
                        row.get::<_, i64>(6)? as usize,
                        row.get::<_, i64>(7)? as usize,
                    ),
                    complexity: row.get::<_, i64>(8)? as u32,
                    is_public: row.get::<_, i64>(9)? == 1,
                })
            })
            .optional()
            .map_err(GlobalSymbolError::from)?;

        Ok(result)
    }

    /// Get all symbols for a project
    pub fn get_project_symbols(
        &self,
        project_id: &str,
    ) -> Result<Vec<GlobalSymbol>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT symbol_id, project_id, symbol_name, symbol_type, signature,
                    file_path, byte_range_start, byte_range_end, complexity, is_public
             FROM global_symbols
             WHERE project_id = ?1",
            )
            .map_err(GlobalSymbolError::from)?;

        let symbols = stmt
            .query_map(params![project_id], |row| {
                Ok(GlobalSymbol {
                    symbol_id: row.get(0)?,
                    project_id: row.get(1)?,
                    symbol_name: row.get(2)?,
                    symbol_type: SymbolType::from_str_name(row.get::<_, String>(3)?.as_str())
                        .unwrap_or(SymbolType::Function),
                    signature: row.get(4)?,
                    file_path: row.get(5)?,
                    byte_range: (
                        row.get::<_, i64>(6)? as usize,
                        row.get::<_, i64>(7)? as usize,
                    ),
                    complexity: row.get::<_, i64>(8)? as u32,
                    is_public: row.get::<_, i64>(9)? == 1,
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(symbols)
    }

    /// Add external reference
    pub fn add_external_ref(&self, reference: &ExternalRef) -> Result<(), GlobalSymbolError> {
        self.db
            .conn()
            .execute(
                "INSERT INTO external_refs (
                ref_id, source_project_id, source_symbol_id,
                target_project_id, target_symbol_id, ref_type
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &reference.ref_id,
                    &reference.source_project_id,
                    &reference.source_symbol_id,
                    &reference.target_project_id,
                    &reference.target_symbol_id,
                    reference.ref_type.as_str(),
                ],
            )
            .map_err(GlobalSymbolError::from)?;

        Ok(())
    }

    /// Get all outgoing references from a symbol
    pub fn get_outgoing_refs(
        &self,
        symbol_id: &GlobalSymbolId,
    ) -> Result<Vec<ExternalRef>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT ref_id, source_project_id, source_symbol_id,
                    target_project_id, target_symbol_id, ref_type
             FROM external_refs
             WHERE source_symbol_id = ?1",
            )
            .map_err(GlobalSymbolError::from)?;

        let refs = stmt
            .query_map(params![symbol_id], |row| {
                Ok(ExternalRef {
                    ref_id: row.get(0)?,
                    source_project_id: row.get(1)?,
                    source_symbol_id: row.get(2)?,
                    target_project_id: row.get(3)?,
                    target_symbol_id: row.get(4)?,
                    ref_type: RefType::from_str_name(row.get::<_, String>(5)?.as_str())
                        .unwrap_or(RefType::Call),
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(refs)
    }

    /// Get all incoming references to a symbol
    pub fn get_incoming_refs(
        &self,
        symbol_id: &GlobalSymbolId,
    ) -> Result<Vec<ExternalRef>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT ref_id, source_project_id, source_symbol_id,
                    target_project_id, target_symbol_id, ref_type
             FROM external_refs
             WHERE target_symbol_id = ?1",
            )
            .map_err(GlobalSymbolError::from)?;

        let refs = stmt
            .query_map(params![symbol_id], |row| {
                Ok(ExternalRef {
                    ref_id: row.get(0)?,
                    source_project_id: row.get(1)?,
                    source_symbol_id: row.get(2)?,
                    target_project_id: row.get(3)?,
                    target_symbol_id: row.get(4)?,
                    ref_type: RefType::from_str_name(row.get::<_, String>(5)?.as_str())
                        .unwrap_or(RefType::Call),
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(refs)
    }

    /// Add project dependency
    pub fn add_project_dep(&self, dep: &ProjectDep) -> Result<(), GlobalSymbolError> {
        self.db.conn().execute(
            "INSERT INTO project_deps (dep_id, project_id, depends_on_project_id, dependency_type)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                &dep.dep_id,
                &dep.project_id,
                &dep.depends_on_project_id,
                dep.dependency_type.as_str(),
            ],
        ).map_err(GlobalSymbolError::from)?;

        Ok(())
    }

    /// Get all dependencies for a project
    pub fn get_project_deps(&self, project_id: &str) -> Result<Vec<ProjectDep>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT dep_id, project_id, depends_on_project_id, dependency_type
             FROM project_deps
             WHERE project_id = ?1",
            )
            .map_err(GlobalSymbolError::from)?;

        let deps = stmt
            .query_map(params![project_id], |row| {
                Ok(ProjectDep {
                    dep_id: row.get(0)?,
                    project_id: row.get(1)?,
                    depends_on_project_id: row.get(2)?,
                    dependency_type: DepType::from_str_name(row.get::<_, String>(3)?.as_str())
                        .unwrap_or(DepType::Direct),
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(deps)
    }

    /// Get all projects that depend on the given project (reverse dependencies)
    ///
    /// This is used for change propagation - when a project changes,
    /// we need to find all projects that depend on it.
    pub fn get_reverse_project_deps(
        &self,
        depends_on_project_id: &str,
    ) -> Result<Vec<ProjectDep>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT dep_id, project_id, depends_on_project_id, dependency_type
             FROM project_deps
             WHERE depends_on_project_id = ?1",
            )
            .map_err(GlobalSymbolError::from)?;

        let deps = stmt
            .query_map(params![depends_on_project_id], |row| {
                Ok(ProjectDep {
                    dep_id: row.get(0)?,
                    project_id: row.get(1)?,
                    depends_on_project_id: row.get(2)?,
                    dependency_type: DepType::from_str_name(row.get::<_, String>(3)?.as_str())
                        .unwrap_or(DepType::Direct),
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(deps)
    }

    /// Find public symbols (exported API)
    pub fn find_public_symbols(
        &self,
        project_id: &str,
    ) -> Result<Vec<GlobalSymbol>, GlobalSymbolError> {
        let mut stmt = self
            .db
            .conn()
            .prepare(
                "SELECT symbol_id, project_id, symbol_name, symbol_type, signature,
                    file_path, byte_range_start, byte_range_end, complexity, is_public
             FROM global_symbols
             WHERE project_id = ?1 AND is_public = 1",
            )
            .map_err(GlobalSymbolError::from)?;

        let symbols = stmt
            .query_map(params![project_id], |row| {
                Ok(GlobalSymbol {
                    symbol_id: row.get(0)?,
                    project_id: row.get(1)?,
                    symbol_name: row.get(2)?,
                    symbol_type: SymbolType::from_str_name(row.get::<_, String>(3)?.as_str())
                        .unwrap_or(SymbolType::Function),
                    signature: row.get(4)?,
                    file_path: row.get(5)?,
                    byte_range: (
                        row.get::<_, i64>(6)? as usize,
                        row.get::<_, i64>(7)? as usize,
                    ),
                    complexity: row.get::<_, i64>(8)? as u32,
                    is_public: row.get::<_, i64>(9)? == 1,
                })
            })
            .map_err(GlobalSymbolError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GlobalSymbolError::from)?;

        Ok(symbols)
    }

    /// Detect symbol name conflicts
    pub fn detect_conflicts(
        &self,
        symbol_name: &str,
    ) -> Result<Vec<GlobalSymbol>, GlobalSymbolError> {
        // This returns all symbols with the given name across all projects
        // If there are multiple, we have a potential conflict
        self.resolve_by_name(symbol_name)
    }
}

/// Errors for global symbol operations
#[derive(Debug, Error)]
pub enum GlobalSymbolError {
    /// Error originating from the underlying SQLite database
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The specified symbol ID was not found
    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    /// Multiple symbols match the name across different projects
    #[error("Ambiguous symbol: {0} found in {1} projects")]
    AmbiguousSymbol(String, usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_upsert_and_get_symbol() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Storage::open(temp_file.path()).unwrap();
        let table = GlobalSymbolTable::new(&db);

        let symbol = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("test_project", "foo", Some("fn()")),
            project_id: "test_project".to_string(),
            symbol_name: "foo".to_string(),
            symbol_type: SymbolType::Function,
            signature: Some("fn()".to_string()),
            file_path: "src/test.rs".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            is_public: true,
        };

        table.upsert_symbol(&symbol).unwrap();

        let retrieved = table.get_symbol(&symbol.symbol_id).unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.symbol_name, "foo");
        assert_eq!(retrieved.project_id, "test_project");
    }

    #[test]
    fn test_batch_insert() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Storage::open(temp_file.path()).unwrap();
        let table = GlobalSymbolTable::new(&db);

        let symbols = vec![
            GlobalSymbol {
                symbol_id: GlobalSymbolTable::generate_symbol_id("proj_a", "foo", None),
                project_id: "proj_a".to_string(),
                symbol_name: "foo".to_string(),
                symbol_type: SymbolType::Function,
                signature: None,
                file_path: "src/a.rs".to_string(),
                byte_range: (0, 50),
                complexity: 1,
                is_public: false,
            },
            GlobalSymbol {
                symbol_id: GlobalSymbolTable::generate_symbol_id("proj_b", "bar", None),
                project_id: "proj_b".to_string(),
                symbol_name: "bar".to_string(),
                symbol_type: SymbolType::Function,
                signature: None,
                file_path: "src/b.rs".to_string(),
                byte_range: (0, 50),
                complexity: 1,
                is_public: false,
            },
        ];

        table.upsert_symbols_batch(&symbols).unwrap();

        let proj_a_symbols = table.get_project_symbols("proj_a").unwrap();
        assert_eq!(proj_a_symbols.len(), 1);
        assert_eq!(proj_a_symbols[0].symbol_name, "foo");
    }

    #[test]
    fn test_resolve_by_name() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Storage::open(temp_file.path()).unwrap();
        let table = GlobalSymbolTable::new(&db);

        let symbol1 = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_a", "util", None),
            project_id: "proj_a".to_string(),
            symbol_name: "util".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        let symbol2 = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_b", "util", None),
            project_id: "proj_b".to_string(),
            symbol_name: "util".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/b.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        table.upsert_symbol(&symbol1).unwrap();
        table.upsert_symbol(&symbol2).unwrap();

        let results = table.resolve_by_name("util").unwrap();
        assert_eq!(results.len(), 2); // Found in both projects
    }

    #[test]
    fn test_detect_conflicts() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Storage::open(temp_file.path()).unwrap();
        let table = GlobalSymbolTable::new(&db);

        let symbol1 = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_a", "util", None),
            project_id: "proj_a".to_string(),
            symbol_name: "util".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        let symbol2 = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_b", "util", None),
            project_id: "proj_b".to_string(),
            symbol_name: "util".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/b.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        table.upsert_symbol(&symbol1).unwrap();
        table.upsert_symbol(&symbol2).unwrap();

        let conflicts = table.detect_conflicts("util").unwrap();
        assert_eq!(conflicts.len(), 2); // Conflict detected
    }

    #[test]
    fn test_external_refs() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Storage::open(temp_file.path()).unwrap();
        let table = GlobalSymbolTable::new(&db);

        // Create source and target symbols
        let source = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_a", "caller", None),
            project_id: "proj_a".to_string(),
            symbol_name: "caller".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        let target = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_b", "callee", None),
            project_id: "proj_b".to_string(),
            symbol_name: "callee".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/b.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        table.upsert_symbol(&source).unwrap();
        table.upsert_symbol(&target).unwrap();

        // Add external reference
        let ext_ref = ExternalRef {
            ref_id: "ref_123".to_string(),
            source_project_id: "proj_a".to_string(),
            source_symbol_id: source.symbol_id.clone(),
            target_project_id: "proj_b".to_string(),
            target_symbol_id: target.symbol_id.clone(),
            ref_type: RefType::Call,
        };

        table.add_external_ref(&ext_ref).unwrap();

        // Verify we can retrieve the reference
        let outgoing = table.get_outgoing_refs(&source.symbol_id).unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].target_symbol_id, target.symbol_id);

        let incoming = table.get_incoming_refs(&target.symbol_id).unwrap();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].source_symbol_id, source.symbol_id);
    }

    #[test]
    fn test_project_deps() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Storage::open(temp_file.path()).unwrap();
        let table = GlobalSymbolTable::new(&db);

        let dep = ProjectDep {
            dep_id: "dep_1".to_string(),
            project_id: "proj_a".to_string(),
            depends_on_project_id: "proj_b".to_string(),
            dependency_type: DepType::Direct,
        };

        table.add_project_dep(&dep).unwrap();

        let deps = table.get_project_deps("proj_a").unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].depends_on_project_id, "proj_b");
    }

    #[test]
    fn test_find_public_symbols() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Storage::open(temp_file.path()).unwrap();
        let table = GlobalSymbolTable::new(&db);

        let public_symbol = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_a", "public_fn", None),
            project_id: "proj_a".to_string(),
            symbol_name: "public_fn".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/lib.rs".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            is_public: true,
        };

        let private_symbol = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("proj_a", "private_fn", None),
            project_id: "proj_a".to_string(),
            symbol_name: "private_fn".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/internal.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: false,
        };

        table.upsert_symbol(&public_symbol).unwrap();
        table.upsert_symbol(&private_symbol).unwrap();

        let public_symbols = table.find_public_symbols("proj_a").unwrap();
        assert_eq!(public_symbols.len(), 1);
        assert_eq!(public_symbols[0].symbol_name, "public_fn");
    }
}
