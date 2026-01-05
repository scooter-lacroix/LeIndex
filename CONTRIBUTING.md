# Contributing to LeIndex

Thank you for your interest in contributing to LeIndex! This document provides guidelines and instructions for contributing.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Workflow](#development-workflow)
- [Coding Standards](#coding-standards)
- [Testing Guidelines](#testing-guidelines)
- [Documentation](#documentation)
- [Submitting Changes](#submitting-changes)
- [Release Process](#release-process)

---

## Code of Conduct

Please read and follow our [Code of Conduct](CODE_OF_CONDUCT.md). We expect all contributors to be respectful and inclusive.

---

## Getting Started

### Prerequisites

- Python 3.10 or higher
- Git
- Virtual environment (recommended)

### Setup Development Environment

```bash
# Fork and clone the repository
git clone https://github.com/yourusername/leindex.git
cd leindex

# Create virtual environment
python -m venv .venv
source .venv/bin/activate  # On Windows: .venv\Scripts\activate

# Install in development mode
pip install -e ".[dev]"

# Run tests to verify setup
pytest tests/

# Run linter
ruff check src/leindex
```

### Project Structure

```
leindex/
├── src/leindex/              # Main package
│   ├── core_engine/          # Search & indexing engine
│   ├── storage/              # SQLite/DuckDB backends
│   ├── search/               # Search backends (LEANN, Tantivy)
│   ├── registry/             # Project registry & config
│   ├── repositories/         # Data access layer
│   ├── server.py             # MCP server
│   └── cli.py                # CLI tools
├── tests/                    # Test suite
│   ├── unit/                 # Unit tests
│   └── integration/          # Integration tests
├── docs/                     # Documentation
├── examples/                 # Usage examples
├── pyproject.toml           # Project configuration
└── README.md                # Project overview
```

---

## Development Workflow

### 1. Create a Branch

```bash
git checkout -b feature/your-feature-name
# or
git checkout -b fix/your-bug-fix
```

### 2. Make Changes

- Write code following [Coding Standards](#coding-standards)
- Add tests following [Testing Guidelines](#testing-guidelines)
- Update documentation as needed

### 3. Test Your Changes

```bash
# Run all tests
pytest tests/

# Run specific test file
pytest tests/unit/test_search.py

# Run with coverage
pytest --cov=leindex tests/

# Check coverage percentage
pytest --cov=leindex --cov-report=term-missing tests/
```

### 4. Code Quality Checks

```bash
# Linting
ruff check src/leindex

# Type checking
mypy src/leindex

# Format code
ruff format src/leindex

# Run all checks
pre-commit run --all-files
```

### 5. Commit Your Changes

Follow our [Commit Message Conventions](#commit-message-conventions):

```bash
git add .
git commit -m "feat(search): add support for regex search in filenames"
```

### 6. Push and Create Pull Request

```bash
git push origin feature/your-feature-name
```

Then create a pull request on GitHub.

---

## Coding Standards

### Python Style Guide

- Follow [PEP 8](https://pep8.org/) style guide
- Use [Ruff](https://docs.astral.sh/ruff/) for linting
- Maximum line length: 100 characters
- Use type hints for all functions

### Example Code Style

```python
"""Search module for LeIndex."""

from typing import List, Optional
from dataclasses import dataclass


@dataclass
class SearchResult:
    """Represents a single search result."""

    file: str
    line: int
    content: str
    score: float


def search_files(
    query: str,
    path: str,
    max_results: int = 100,
    case_sensitive: bool = False,
) -> List[SearchResult]:
    """
    Search for files matching the query.

    Args:
        query: Search query string
        path: Root directory to search
        max_results: Maximum number of results to return
        case_sensitive: Whether search should be case-sensitive

    Returns:
        List of search results

    Raises:
        ValueError: If query is empty
        FileNotFoundError: If path doesn't exist
    """
    if not query:
        raise ValueError("Query cannot be empty")

    # Implementation here
    return []
```

### Documentation Standards

- All modules must have docstrings
- All public functions/classes must have docstrings
- Use Google style docstrings
- Include type hints
- Document parameters, returns, and exceptions

### Import Organization

```python
# Standard library imports
import os
import sys
from pathlib import Path
from typing import List, Optional

# Third-party imports
import tantivy
from leann import Index

# Local imports
from leindex.core_engine import engine
from leindex.search import base
```

---

## Testing Guidelines

### Test Coverage

- Maintain >95% test coverage
- Write both unit tests and integration tests
- Test edge cases and error conditions

### Unit Tests

Test individual functions and classes in isolation.

```python
import pytest
from leindex.search.tantivy_backend import TantivyBackend


class TestTantivyBackend:
    """Test suite for TantivyBackend."""

    @pytest.fixture
    def backend(self):
        """Create a backend instance for testing."""
        return TantivyBackend()

    def test_add_document(self, backend):
        """Test adding a document to the index."""
        doc = {"path": "/test.py", "content": "def test(): pass"}
        backend.add_document(doc)
        assert backend.count() == 1

    def test_search_returns_results(self, backend):
        """Test that search returns results."""
        backend.add_document({"path": "/test.py", "content": "hello world"})
        results = backend.search("hello")
        assert len(results) > 0

    def test_search_with_empty_query_raises_error(self, backend):
        """Test that empty query raises ValueError."""
        with pytest.raises(ValueError):
            backend.search("")
```

### Integration Tests

Test multiple components working together.

```python
import pytest
from leindex import LeIndex


@pytest.mark.asyncio
async def test_end_to_end_search():
    """Test complete search workflow."""
    # Create indexer
    indexer = LeIndex("/tmp/test-project")

    # Index files
    await indexer.index()

    # Search
    results = await indexer.search("test")

    # Verify
    assert len(results) > 0
    assert results[0].file.endswith("test.py")

    # Cleanup
    await indexer.close()
```

### Running Tests

```bash
# Run all tests
pytest tests/

# Run with coverage
pytest --cov=leindex tests/

# Run specific test
pytest tests/unit/test_search.py::test_search

# Run with verbose output
pytest -v tests/

# Stop on first failure
pytest -x tests/
```

---

## Documentation

### Code Documentation

- Document all public APIs
- Include usage examples in docstrings
- Keep documentation in sync with code

### User Documentation

- Update README.md for user-facing changes
- Update INSTALLATION.md for installation changes
- Update API.md for API changes
- Update TROUBLESHOOTING.md for common issues

### Example Documentation

```python
def search(
    query: str,
    backend: str = "semantic",
    limit: int = 100,
) -> List[SearchResult]:
    """
    Search for code matching the query.

    This method performs semantic search using the specified backend.

    Args:
        query: Search query string (e.g., "authentication logic")
        backend: Search backend to use ("semantic", "tantivy", "regex")
        limit: Maximum number of results to return

    Returns:
        List of search results sorted by relevance

    Raises:
        ValueError: If query is empty
        BackendError: If search backend fails

    Examples:
        >>> indexer = LeIndex("~/my-project")
        >>> results = await indexer.search("authentication")
        >>> for result in results[:5]:
        ...     print(f"{result.file}:{result.line}")
        ...     print(result.content)
    """
```

---

## Commit Message Conventions

Follow [Conventional Commits](https://www.conventionalcommits.org/):

### Format

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

### Types

- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `test`: Adding or updating tests
- `chore`: Maintenance tasks
- `perf`: Performance improvements

### Examples

```bash
feat(search): add support for regex search in filenames

fix(indexer): handle empty files gracefully

docs(api): update search API documentation

refactor(storage): simplify SQLite connection handling

test(search): add integration tests for Tantivy backend
```

---

## Submitting Changes

### Pull Request Guidelines

1. **Title**: Use clear, descriptive title
   - Good: "feat(search): add hybrid search combining semantic and full-text"
   - Bad: "update search code"

2. **Description**: Explain the what and why
   - What changes were made?
   - Why were they needed?
   - How were they tested?

3. **Linked Issues**: Reference related issues
   - "Fixes #123"
   - "Related to #456"

4. **Checks**: Ensure all checks pass
   - Tests pass
   - Coverage >95%
   - Linting passes
   - Type checking passes

### Pull Request Template

```markdown
## Description
Brief description of changes

## Type of Change
- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation update

## Testing
- [ ] Unit tests added/updated
- [ ] Integration tests added/updated
- [ ] All tests pass
- [ ] Coverage >95%

## Checklist
- [ ] Code follows style guidelines
- [ ] Self-review completed
- [ ] Documentation updated
- [ ] No new warnings generated
```

---

## Release Process

### Version Bumping

Follow [Semantic Versioning](https://semver.org/):
- **MAJOR**: Breaking changes
- **MINOR**: New features (backwards compatible)
- **PATCH**: Bug fixes (backwards compatible)

### Release Steps

1. Update version in `pyproject.toml`
2. Update `CHANGELOG.md`
3. Create release branch: `git checkout -b release/v1.0.0`
4. Tag release: `git tag v1.0.0`
5. Push tag: `git push origin v1.0.0`
6. Create GitHub release with changelog
7. Publish to PyPI: `python -m build && twine upload dist/*`

---

## Getting Help

- **Documentation**: Check [README.md](README.md) and [API.md](API.md)
- **Issues**: Search [GitHub Issues](https://github.com/yourusername/leindex/issues)
- **Discussions**: Use [GitHub Discussions](https://github.com/yourusername/leindex/discussions)
- **Discord**: Join our [community](https://discord.gg/leindex)

---

## Recognition

Contributors will be recognized in:
- CONTRIBUTORS.md file
- Release notes
- Project README

Thank you for contributing to LeIndex! 🎉
