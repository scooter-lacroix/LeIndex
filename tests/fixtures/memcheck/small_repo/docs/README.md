# Small Repo Fixture

This is a deterministic fixture project used for memory measurement testing.

## Structure

- `src/` - Source code
  - `models/` - Data models (user, project, document, config, session)
  - `handlers/` - Request handlers
  - `utils/` - Utility functions
- `config/` - Configuration files
- `tests/` - Test files
- `docs/` - Documentation

## Purpose

This fixture is used by the LeIndex memcheck harness to measure memory behavior
during indexing, search, and reindex operations.
