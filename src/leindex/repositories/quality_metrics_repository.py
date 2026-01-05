"""
Quality Metrics Repository

Handles quality metrics tracking, evaluation runs, and performance monitoring.

Phase 3: Search Integration, Optimization, and Production Readiness
Spec: conductor/tracks/mcp_consolidation_local_vector_20251230/spec.md
"""

import uuid
from datetime import datetime, timedelta
from typing import Optional, Dict, Any, List
from dataclasses import dataclass

from .base import Repository, RepositoryError, NotFoundError


@dataclass
class EvalRun:
    """Represents an evaluation run for tracking search quality."""

    id: str
    run_type: str  # 'manual', 'automated', 'ab_test'
    description: Optional[str] = None
    query_count: int = 0
    started_at: Optional[datetime] = None
    completed_at: Optional[datetime] = None
    status: str = "running"  # 'running', 'completed', 'failed'
    config_json: Optional[Dict[str, Any]] = None
    created_at: Optional[datetime] = None


@dataclass
class EvalQueryResult:
    """Represents results from a single query in an evaluation run."""

    id: str
    eval_run_id: str
    query_text: str
    query_type: str  # 'semantic', 'regex', 'hybrid'
    confidence: Optional[float] = None
    backend_used: str = "merged"
    result_count: int = 0
    latency_ms: Optional[float] = None
    recall_at_k: Optional[float] = None
    precision_at_k: Optional[float] = None
    mrr: Optional[float] = None
    relevance_scores_json: Optional[Dict[str, Any]] = None
    is_relevant_predicted: Optional[bool] = None
    error_message: Optional[str] = None
    executed_at: Optional[datetime] = None
    created_at: Optional[datetime] = None


@dataclass
class QualityMetricsAggregated:
    """Aggregated quality metrics for reporting."""

    id: str
    metric_type: str  # 'recall', 'precision', 'mrr', 'latency'
    aggregation_period: str  # 'hourly', 'daily', 'weekly'
    period_start: datetime
    period_end: datetime
    backend: Optional[str] = None
    query_type: Optional[str] = None
    sample_count: int = 0
    mean_value: Optional[float] = None
    min_value: Optional[float] = None
    max_value: Optional[float] = None
    p50_value: Optional[float] = None
    p95_value: Optional[float] = None
    p99_value: Optional[float] = None
    std_dev: Optional[float] = None
    created_at: Optional[datetime] = None


@dataclass
class UserFeedback:
    """User feedback for search results."""

    id: str
    feedback_type: str  # 'rating', 'relevance', 'correction'
    query_text: Optional[str] = None
    file_path: Optional[str] = None
    line_number: Optional[int] = None
    rating: Optional[int] = None  # 1-5
    is_relevant: Optional[bool] = None
    session_id: Optional[str] = None
    metadata_json: Optional[Dict[str, Any]] = None
    created_at: Optional[datetime] = None


@dataclass
class SearchThreshold:
    """Quality threshold for alerting."""

    id: str
    metric_name: str  # 'recall', 'latency', 'error_rate'
    threshold_type: str  # 'min', 'max', 'target'
    threshold_value: float
    severity: str = "warning"  # 'info', 'warning', 'critical'
    is_active: bool = True
    description: Optional[str] = None
    last_triggered_at: Optional[datetime] = None
    trigger_count: int = 0
    created_at: Optional[datetime] = None
    updated_at: Optional[datetime] = None


class QualityMetricsRepository(Repository):
    """
    Repository for quality metrics tracking and evaluation.

    Provides methods for:
    - Managing evaluation runs and query results
    - Aggregating quality metrics
    - Recording user feedback
    - Configuring and checking quality thresholds
    """

    def __init__(self, dal):
        """
        Initialize quality metrics repository.

        Args:
            dal: Data access layer instance
        """
        super().__init__(dal)
        self._table_prefix = "eval_"

    def create_eval_run(
        self,
        run_type: str,
        description: Optional[str] = None,
        config_json: Optional[Dict[str, Any]] = None,
    ) -> EvalRun:
        """
        Create a new evaluation run.

        Args:
            run_type: Type of run ('manual', 'automated', 'ab_test')
            description: Optional description
            config_json: Optional configuration dictionary

        Returns:
            Created EvalRun instance

        Raises:
            RepositoryError: If creation fails
        """
        try:
            run_id = str(uuid.uuid4())
            now = datetime.utcnow()

            query = f"""
                INSERT INTO {self._table_prefix}eval_runs
                (id, run_type, description, query_count, status, config_json, started_at, created_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s)
            """

            self.dal.execute(
                query,
                (
                    run_id,
                    run_type,
                    description,
                    0,
                    "running",
                    self._dal_serialize(config_json),
                    now,
                    now,
                ),
            )

            return EvalRun(
                id=run_id,
                run_type=run_type,
                description=description,
                query_count=0,
                started_at=now,
                status="running",
                config_json=config_json,
                created_at=now,
            )
        except Exception as e:
            raise RepositoryError(f"Failed to create eval run: {e}")

    def complete_eval_run(self, run_id: str, query_count: int) -> bool:
        """
        Mark an evaluation run as completed.

        Args:
            run_id: The evaluation run ID
            query_count: Number of queries executed

        Returns:
            True if successful

        Raises:
            NotFoundError: If run not found
        """
        try:
            # Check if run exists
            check_query = f"SELECT id FROM {self._table_prefix}eval_runs WHERE id = %s"
            result = self.dal.execute(check_query, (run_id,))

            if not result:
                raise NotFoundError(f"Eval run {run_id} not found")

            update_query = f"""
                UPDATE {self._table_prefix}eval_runs
                SET status = 'completed', query_count = %s, completed_at = %s
                WHERE id = %s
            """

            self.dal.execute(update_query, (query_count, datetime.utcnow(), run_id))
            return True
        except NotFoundError:
            raise
        except Exception as e:
            raise RepositoryError(f"Failed to complete eval run: {e}")

    def record_query_result(
        self,
        eval_run_id: str,
        query_text: str,
        query_type: str,
        backend_used: str,
        result_count: int = 0,
        latency_ms: Optional[float] = None,
        recall_at_k: Optional[float] = None,
        precision_at_k: Optional[float] = None,
        mrr: Optional[float] = None,
        confidence: Optional[float] = None,
        error_message: Optional[str] = None,
    ) -> EvalQueryResult:
        """
        Record results from a query in an evaluation run.

        Args:
            eval_run_id: The evaluation run ID
            query_text: The query text
            query_type: Type of query ('semantic', 'regex', 'hybrid')
            backend_used: Backend used ('faiss', 'elasticsearch', 'zoekt', 'merged')
            result_count: Number of results returned
            latency_ms: Query latency in milliseconds
            recall_at_k: Recall at K metric
            precision_at_k: Precision at K metric
            mrr: Mean Reciprocal Rank
            confidence: Query type detection confidence
            error_message: Error message if query failed

        Returns:
            Created EvalQueryResult instance

        Raises:
            RepositoryError: If recording fails
        """
        try:
            result_id = str(uuid.uuid4())
            now = datetime.utcnow()

            query = f"""
                INSERT INTO {self._table_prefix}eval_query_results
                (id, eval_run_id, query_text, query_type, confidence, backend_used,
                 result_count, latency_ms, recall_at_k, precision_at_k, mrr,
                 error_message, executed_at, created_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
            """

            self.dal.execute(
                query,
                (
                    result_id,
                    eval_run_id,
                    query_text,
                    query_type,
                    confidence,
                    backend_used,
                    result_count,
                    latency_ms,
                    recall_at_k,
                    precision_at_k,
                    mrr,
                    error_message,
                    now,
                    now,
                ),
            )

            return EvalQueryResult(
                id=result_id,
                eval_run_id=eval_run_id,
                query_text=query_text,
                query_type=query_type,
                confidence=confidence,
                backend_used=backend_used,
                result_count=result_count,
                latency_ms=latency_ms,
                recall_at_k=recall_at_k,
                precision_at_k=precision_at_k,
                mrr=mrr,
                executed_at=now,
                created_at=now,
            )
        except Exception as e:
            raise RepositoryError(f"Failed to record query result: {e}")

    def get_query_results_by_run(self, run_id: str) -> List[EvalQueryResult]:
        """
        Get all query results for an evaluation run.

        Args:
            run_id: The evaluation run ID

        Returns:
            List of EvalQueryResult instances
        """
        try:
            query = f"""
                SELECT id, eval_run_id, query_text, query_type, confidence,
                       backend_used, result_count, latency_ms, recall_at_k,
                       precision_at_k, mrr, error_message, executed_at, created_at
                FROM {self._table_prefix}eval_query_results
                WHERE eval_run_id = %s
                ORDER BY executed_at ASC
            """

            results = self.dal.execute(query, (run_id,))

            return [
                EvalQueryResult(
                    id=row[0],
                    eval_run_id=row[1],
                    query_text=row[2],
                    query_type=row[3],
                    confidence=row[4],
                    backend_used=row[5],
                    result_count=row[6],
                    latency_ms=row[7],
                    recall_at_k=row[8],
                    precision_at_k=row[9],
                    mrr=row[10],
                    error_message=row[11],
                    executed_at=row[12],
                    created_at=row[13],
                )
                for row in results
            ]
        except Exception as e:
            raise RepositoryError(f"Failed to get query results: {e}")

    def get_aggregated_metrics(
        self,
        metric_type: str,
        aggregation_period: str,
        period_start: datetime,
        period_end: datetime,
        backend: Optional[str] = None,
        query_type: Optional[str] = None,
    ) -> List[QualityMetricsAggregated]:
        """
        Get aggregated quality metrics for a time period.

        Args:
            metric_type: Type of metric ('recall', 'precision', 'mrr', 'latency')
            aggregation_period: Period type ('hourly', 'daily', 'weekly')
            period_start: Start of period
            period_end: End of period
            backend: Optional backend filter
            query_type: Optional query type filter

        Returns:
            List of aggregated metrics
        """
        try:
            query = f"""
                SELECT id, metric_type, aggregation_period, period_start, period_end,
                       backend, query_type, sample_count, mean_value, min_value, max_value,
                       p50_value, p95_value, p99_value, std_dev, created_at
                FROM {self._table_prefix}quality_metrics_aggregated
                WHERE metric_type = %s
                  AND aggregation_period = %s
                  AND period_start >= %s
                  AND period_end <= %s
            """

            params = [metric_type, aggregation_period, period_start, period_end]

            if backend:
                query += " AND (backend = %s OR backend IS NULL)"
                params.append(backend)
            if query_type:
                query += " AND (query_type = %s OR query_type IS NULL)"
                params.append(query_type)

            query += " ORDER BY period_start DESC"

            results = self.dal.execute(query, tuple(params))

            return [
                QualityMetricsAggregated(
                    id=row[0],
                    metric_type=row[1],
                    aggregation_period=row[2],
                    period_start=row[3],
                    period_end=row[4],
                    backend=row[5],
                    query_type=row[6],
                    sample_count=row[7],
                    mean_value=row[8],
                    min_value=row[9],
                    max_value=row[10],
                    p50_value=row[11],
                    p95_value=row[12],
                    p99_value=row[13],
                    std_dev=row[14],
                    created_at=row[15],
                )
                for row in results
            ]
        except Exception as e:
            raise RepositoryError(f"Failed to get aggregated metrics: {e}")

    def record_user_feedback(
        self,
        feedback_type: str,
        query_text: Optional[str] = None,
        file_path: Optional[str] = None,
        line_number: Optional[int] = None,
        rating: Optional[int] = None,
        is_relevant: Optional[bool] = None,
        session_id: Optional[str] = None,
        metadata_json: Optional[Dict[str, Any]] = None,
    ) -> UserFeedback:
        """
        Record user feedback for search results.

        Args:
            feedback_type: Type of feedback ('rating', 'relevance', 'correction')
            query_text: The query that produced the result
            file_path: Path to the file that was rated
            line_number: Line number in the file
            rating: Rating 1-5
            is_relevant: Whether the result was relevant
            session_id: User session identifier
            metadata_json: Additional metadata

        Returns:
            Created UserFeedback instance
        """
        try:
            feedback_id = str(uuid.uuid4())
            now = datetime.utcnow()

            query = f"""
                INSERT INTO {self._table_prefix}user_feedback
                (id, query_text, file_path, line_number, rating, is_relevant,
                 feedback_type, session_id, metadata_json, created_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
            """

            self.dal.execute(
                query,
                (
                    feedback_id,
                    query_text,
                    file_path,
                    line_number,
                    rating,
                    is_relevant,
                    feedback_type,
                    session_id,
                    self._dal_serialize(metadata_json),
                    now,
                ),
            )

            return UserFeedback(
                id=feedback_id,
                feedback_type=feedback_type,
                query_text=query_text,
                file_path=file_path,
                line_number=line_number,
                rating=rating,
                is_relevant=is_relevant,
                session_id=session_id,
                metadata_json=metadata_json,
                created_at=now,
            )
        except Exception as e:
            raise RepositoryError(f"Failed to record user feedback: {e}")

    def create_threshold(
        self,
        metric_name: str,
        threshold_type: str,
        threshold_value: float,
        severity: str = "warning",
        description: Optional[str] = None,
    ) -> SearchThreshold:
        """
        Create a quality threshold for alerting.

        Args:
            metric_name: Metric to monitor ('recall', 'latency', 'error_rate')
            threshold_type: Type of threshold ('min', 'max', 'target')
            threshold_value: The threshold value
            severity: Alert severity ('info', 'warning', 'critical')
            description: Optional description

        Returns:
            Created SearchThreshold instance
        """
        try:
            threshold_id = str(uuid.uuid4())
            now = datetime.utcnow()

            query = f"""
                INSERT INTO {self._table_prefix}search_thresholds
                (id, metric_name, threshold_type, threshold_value, severity, is_active, description, created_at, updated_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s)
            """

            self.dal.execute(
                query,
                (
                    threshold_id,
                    metric_name,
                    threshold_type,
                    threshold_value,
                    severity,
                    True,
                    description,
                    now,
                    now,
                ),
            )

            return SearchThreshold(
                id=threshold_id,
                metric_name=metric_name,
                threshold_type=threshold_type,
                threshold_value=threshold_value,
                severity=severity,
                is_active=True,
                description=description,
                created_at=now,
                updated_at=now,
            )
        except Exception as e:
            raise RepositoryError(f"Failed to create threshold: {e}")

    def check_thresholds(
        self, metric_name: str, current_value: float
    ) -> List[Dict[str, Any]]:
        """
        Check all active thresholds for a metric and return any violations.

        Args:
            metric_name: The metric to check
            current_value: Current metric value

        Returns:
            List of threshold violations with details
        """
        try:
            query = f"""
                SELECT id, metric_name, threshold_type, threshold_value, severity, description
                FROM {self._table_prefix}search_thresholds
                WHERE metric_name = %s AND is_active = true
            """

            results = self.dal.execute(query, (metric_name,))
            violations = []

            for row in results:
                (
                    threshold_id,
                    _,
                    threshold_type,
                    threshold_value,
                    severity,
                    description,
                ) = row
                is_violation = False

                if threshold_type == "max" and current_value > threshold_value:
                    is_violation = True
                elif threshold_type == "min" and current_value < threshold_value:
                    is_violation = True
                elif threshold_type == "target" and current_value != threshold_value:
                    is_violation = True

                if is_violation:
                    # Update trigger count
                    update_query = f"""
                        UPDATE {self._table_prefix}search_thresholds
                        SET trigger_count = trigger_count + 1, last_triggered_at = %s, updated_at = %s
                        WHERE id = %s
                    """
                    self.dal.execute(
                        update_query,
                        (datetime.utcnow(), datetime.utcnow(), threshold_id),
                    )

                    violations.append(
                        {
                            "threshold_id": threshold_id,
                            "metric_name": metric_name,
                            "threshold_type": threshold_type,
                            "threshold_value": threshold_value,
                            "current_value": current_value,
                            "severity": severity,
                            "description": description,
                        }
                    )

            return violations
        except Exception as e:
            raise RepositoryError(f"Failed to check thresholds: {e}")

    def update_query_type_stats(
        self,
        date: datetime,
        query_type: str,
        increment: int = 1,
        confidence_sum: Optional[float] = None,
    ) -> bool:
        """
        Update query type detection statistics.

        Args:
            date: The date to update
            query_type: Type of query ('semantic', 'regex', 'hybrid')
            increment: Amount to increment count
            confidence_sum: Sum of confidence scores for averaging

        Returns:
            True if successful
        """
        try:
            # Try to insert, or update if exists
            insert_query = f"""
                INSERT INTO {self._table_prefix}query_type_stats
                (id, date, total_queries, semantic_count, regex_count, hybrid_count, created_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s)
                ON CONFLICT (date) DO UPDATE SET
                    total_queries = {self._table_prefix}query_type_stats.total_queries + %s,
                    semantic_count = {self._table_prefix}query_type_stats.semantic_count + %s,
                    regex_count = {self._table_prefix}query_type_stats.regex_count + %s,
                    hybrid_count = {self._table_prefix}query_type_stats.hybrid_count + %s
            """

            stats_id = str(uuid.uuid4())
            now = datetime.utcnow()

            if query_type == "semantic":
                self.dal.execute(
                    insert_query,
                    (
                        stats_id,
                        date,
                        increment,
                        increment,
                        0,
                        0,
                        now,
                        increment,
                        increment,
                        0,
                        0,
                    ),
                )
            elif query_type == "regex":
                self.dal.execute(
                    insert_query,
                    (
                        stats_id,
                        date,
                        increment,
                        0,
                        increment,
                        0,
                        now,
                        increment,
                        0,
                        increment,
                        0,
                    ),
                )
            elif query_type == "hybrid":
                self.dal.execute(
                    insert_query,
                    (
                        stats_id,
                        date,
                        increment,
                        0,
                        0,
                        increment,
                        now,
                        increment,
                        0,
                        0,
                        increment,
                    ),
                )
            else:
                self.dal.execute(
                    insert_query,
                    (stats_id, date, increment, 0, 0, 0, now, increment, 0, 0, 0),
                )

            return True
        except Exception as e:
            raise RepositoryError(f"Failed to update query type stats: {e}")

    def get_performance_summary(self, period_days: int = 7) -> Dict[str, Any]:
        """
        Get performance summary for the specified period.

        Args:
            period_days: Number of days to include in summary

        Returns:
            Dictionary with performance metrics
        """
        try:
            period_start = datetime.utcnow() - timedelta(days=period_days)

            # Get average latency
            latency_query = f"""
                SELECT AVG(latency_ms) as avg_latency,
                       PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY latency_ms) as p50_latency,
                       PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms) as p95_latency,
                       COUNT(*) as query_count
                FROM {self._table_prefix}eval_query_results
                WHERE executed_at >= %s AND latency_ms IS NOT NULL
            """
            latency_results = self.dal.execute(latency_query, (period_start,))

            # Get recall statistics
            recall_query = f"""
                SELECT AVG(recall_at_k) as avg_recall,
                       COUNT(*) as count
                FROM {self._table_prefix}eval_query_results
                WHERE executed_at >= %s AND recall_at_k IS NOT NULL
            """
            recall_results = self.dal.execute(recall_query, (period_start,))

            # Get query type distribution
            distribution_query = f"""
                SELECT query_type, COUNT(*) as count
                FROM {self._table_prefix}eval_query_results
                WHERE executed_at >= %s
                GROUP BY query_type
            """
            distribution_results = self.dal.execute(distribution_query, (period_start,))

            # Get success/failure rate
            error_query = f"""
                SELECT
                    COUNT(*) as total,
                    SUM(CASE WHEN error_message IS NULL THEN 1 ELSE 0 END) as success,
                    SUM(CASE WHEN error_message IS NOT NULL THEN 1 ELSE 0 END) as failures
                FROM {self._table_prefix}eval_query_results
                WHERE executed_at >= %s
            """
            error_results = self.dal.execute(error_query, (period_start,))

            return {
                "period_days": period_days,
                "latency": {
                    "avg_ms": latency_results[0][0] if latency_results else None,
                    "p50_ms": latency_results[0][1] if latency_results else None,
                    "p95_ms": latency_results[0][2] if latency_results else None,
                    "query_count": latency_results[0][3] if latency_results else 0,
                },
                "recall": {
                    "avg": recall_results[0][0] if recall_results else None,
                    "count": recall_results[0][1] if recall_results else 0,
                },
                "query_type_distribution": {
                    row[0]: row[1] for row in distribution_results
                }
                if distribution_results
                else {},
                "success_rate": {
                    "total": error_results[0][0] if error_results else 0,
                    "success": error_results[0][1] if error_results else 0,
                    "failures": error_results[0][2] if error_results else 0,
                    "rate": (error_results[0][1] / error_results[0][0] * 100)
                    if error_results and error_results[0][0] > 0
                    else 100,
                },
            }
        except Exception as e:
            raise RepositoryError(f"Failed to get performance summary: {e}")
