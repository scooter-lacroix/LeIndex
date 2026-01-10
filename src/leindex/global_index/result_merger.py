"""
Result Merger for Federated Queries.

Merges results from multiple project indexes for cross-project queries.
"""

import logging
from dataclasses import dataclass
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)


@dataclass
class SearchResult:
    """A search result from a single project."""
    project_id: str
    file_path: str
    line_number: int
    content: str
    score: float
    match_type: str  # "semantic", "lexical"


class ResultMerger:
    """
    Merge results from multiple project indexes.

    Handles:
    - Cross-project search results
    - Result ranking across projects
    - Deduplication
    - Limit enforcement
    """

    def merge_search_results(
        self,
        project_results: Dict[str, List[SearchResult]],
        limit: Optional[int] = None
    ) -> List[SearchResult]:
        """
        Merge search results from multiple projects.

        Args:
            project_results: Dict mapping project_id to list of results
            limit: Maximum number of results to return

        Returns:
            Merged and ranked list of search results
        """
        # Flatten results
        all_results = []
        for project_id, results in project_results.items():
            for result in results:
                # Ensure project_id is set
                if result.project_id != project_id:
                    result = SearchResult(
                        project_id=project_id,
                        file_path=result.file_path,
                        line_number=result.line_number,
                        content=result.content,
                        score=result.score,
                        match_type=result.match_type
                    )
                all_results.append(result)

        # Sort by score (descending)
        all_results.sort(key=lambda r: r.score, reverse=True)

        # Deduplicate (same file_path and line_number)
        seen = set()
        deduplicated = []
        for result in all_results:
            key = (result.project_id, result.file_path, result.line_number)
            if key not in seen:
                seen.add(key)
                deduplicated.append(result)

        # Apply limit
        if limit and len(deduplicated) > limit:
            deduplicated = deduplicated[:limit]

        logger.info(
            f"Merged {len(all_results)} results from {len(project_results)} projects, "
            f"deduplicated to {len(deduplicated)} results"
        )

        return deduplicated

    def merge_dependency_graphs(
        self,
        project_graphs: Dict[str, Dict[str, List[str]]]
    ) -> Dict[str, Dict[str, List[str]]]:
        """
        Merge dependency graphs from multiple projects.

        Args:
            project_graphs: Dict mapping project_id to dependency graph

        Returns:
            Merged dependency graph
        """
        # For dependency graphs, we just combine them
        # Each project has its own dependencies
        return {
            project_id: deps
            for project_id, deps in project_graphs.items()
        }

    def merge_pattern_matches(
        self,
        project_matches: Dict[str, List[Dict[str, Any]]],
        limit: Optional[int] = None
    ) -> List[Dict[str, Any]]:
        """
        Merge pattern matches from multiple projects.

        Args:
            project_matches: Dict mapping project_id to list of matches
            limit: Maximum number of matches to return

        Returns:
            Merged list of pattern matches
        """
        # Flatten results
        all_matches = []
        for project_id, matches in project_matches.items():
            for match in matches:
                # Add project_id if not present
                if 'project_id' not in match:
                    match = {**match, 'project_id': project_id}
                all_matches.append(match)

        # Sort by score if present
        if all_matches and all('score' in m for m in all_matches):
            all_matches.sort(key=lambda m: m.get('score', 0), reverse=True)

        # Apply limit
        if limit and len(all_matches) > limit:
            all_matches = all_matches[:limit]

        logger.info(
            f"Merged {len(all_matches)} matches from {len(project_matches)} projects"
        )

        return all_matches

    def merge_aggregate_exports(
        self,
        project_exports: Dict[str, Dict[str, List[str]]]
    ) -> Dict[str, Dict[str, List[str]]]:
        """
        Merge aggregated exports from multiple projects.

        Args:
            project_exports: Dict mapping project_id to exports dict

        Returns:
            Merged exports dict with project_id prefixes
        """
        # Prefix exports with project_id to avoid collisions
        merged = {}
        for project_id, exports in project_exports.items():
            for export_type, symbols in exports.items():
                # Add project_id prefix to symbols
                prefixed_symbols = [
                    f"{project_id}:{symbol}" for symbol in symbols
                ]
                merged[export_type] = merged.get(export_type, []) + prefixed_symbols

        return merged

    def rank_cross_project_results(
        self,
        results: List[SearchResult],
        method: str = "score"
    ) -> List[SearchResult]:
        """
        Rank cross-project search results.

        Args:
            results: List of search results
            method: Ranking method ("score", "relevance", "recency")

        Returns:
            Ranked list of search results
        """
        if method == "score":
            # Already sorted by score
            return sorted(results, key=lambda r: r.score, reverse=True)

        elif method == "relevance":
            # Combine score with other factors
            # (placeholder for more sophisticated ranking)
            return sorted(
                results,
                key=lambda r: (
                    r.score,
                    -len(r.file_path)  # Prefer shorter paths
                ),
                reverse=True
            )

        elif method == "recency":
            # Sort by last indexed time (if available)
            # (placeholder - requires metadata)
            return sorted(results, key=lambda r: r.score, reverse=True)

        else:
            logger.warning(f"Unknown ranking method: {method}, using score")
            return sorted(results, key=lambda r: r.score, reverse=True)
