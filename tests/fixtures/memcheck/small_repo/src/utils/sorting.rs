//! Sorting utilities.

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// Sort specification.
pub struct SortSpec {
    pub field: String,
    pub direction: SortDirection,
}

impl SortSpec {
    pub fn asc(field: impl Into<String>) -> Self {
        Self { field: field.into(), direction: SortDirection::Ascending }
    }

    pub fn desc(field: impl Into<String>) -> Self {
        Self { field: field.into(), direction: SortDirection::Descending }
    }
}

/// Parse a sort string like "name:asc" or "created_at:desc".
pub fn parse_sort(s: &str) -> Option<SortSpec> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [field] => Some(SortSpec::asc(*field)),
        [field, "asc"] => Some(SortSpec::asc(*field)),
        [field, "desc"] => Some(SortSpec::desc(*field)),
        _ => None,
    }
}
