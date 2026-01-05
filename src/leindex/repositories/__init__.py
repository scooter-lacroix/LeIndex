"""
Repository Pattern for Business Logic Separation.

ARCHITECTURAL FIX (Issue #3 - Missing Repository Pattern):
-------------------------------------------------------
This module implements the Repository Pattern to separate business logic
from data access logic.

PROBLEM SOLVED:
- Business logic was scattered across DAL implementations
- Testing was difficult due to tight coupling
- Data access logic mixed with business rules

BENEFITS:
- Clear separation of concerns
- Business logic in one place, data access in another
- Easier to test with mock repositories
- Repositories can be swapped without affecting business logic
- Transactions and business rules are centralized

REPOSITORY PATTERN ARCHITECTURE:
--------------------------------
1. Repository (Abstract Base): Defines the contract
2. FileRepository: Handles file-related business operations
3. SearchRepository: Handles search operations with business rules
4. IndexRepository: Handles indexing operations with business logic
5. RepositoryFactory: Creates repository instances with proper DAL injection

USAGE EXAMPLE:
--------------
    # Create a repository with DAL injection
    dal = get_dal_instance()
    file_repo = FileRepository(dal)

    # Business logic is encapsulated in the repository
    try:
        file_repo.add_file_with_validation(
            file_path="/path/to/file.py",
            file_type="file",
            extension="py",
            content="...",
            metadata={"language": "python"}
        )
    except RepositoryError as e:
        logger.error(f"Failed to add file: {e}")
"""

from .base import Repository, RepositoryError, NotFoundError, ValidationError
from .file_repository import FileRepository
from .search_repository import SearchRepository
from .index_repository import IndexRepository
from .quality_metrics_repository import QualityMetricsRepository
from .repository_factory import RepositoryFactory

__all__ = [
    "Repository",
    "RepositoryError",
    "NotFoundError",
    "ValidationError",
    "FileRepository",
    "SearchRepository",
    "IndexRepository",
    "QualityMetricsRepository",
    "RepositoryFactory",
]
