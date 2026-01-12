# TDD Red Phase Complete - DashboardData Missing Attributes Bug

## Phase Status: RED (Failing Tests Created)

### Test File Created
**File:** `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/tests/global_index/test_tier1_metadata_dashboard.py`

### Test Results Summary
```
Tests run: 6
Successes: 1
Failures: 5
Errors: 0
```

### Failed Tests (Expected - This is the RED Phase)

1. **test_dashboard_data_has_average_health_score_field** - FAILED
   - Asserts that `DashboardData` dataclass has `average_health_score` field
   - Actual: Field not found in `{'last_updated', 'projects', 'total_files', 'total_projects', 'total_symbols', 'languages'}`

2. **test_dashboard_data_has_total_size_mb_field** - FAILED
   - Asserts that `DashboardData` dataclass has `total_size_mb` field
   - Actual: Field not found in DashboardData

3. **test_dashboard_data_matches_global_stats_fields** - FAILED
   - Asserts that DashboardData has all numeric fields from GlobalStats
   - Actual: Missing `{'average_health_score', 'total_size_mb'}`

4. **test_get_dashboard_data_has_average_health_score_attribute** - FAILED
   - Asserts that `get_dashboard_data()` returns DashboardData with `average_health_score`
   - Actual: `AttributeError: 'DashboardData' object has no attribute 'average_health_score'`

5. **test_get_dashboard_data_has_total_size_mb_attribute** - FAILED
   - Asserts that `get_dashboard_data()` returns DashboardData with `total_size_mb`
   - Actual: `AttributeError: 'DashboardData' object has no attribute 'total_size_mb'`

### Bug Confirmed

The bug is in two places:

1. **DashboardData dataclass definition** (lines 112-133 in `tier1_metadata.py`)
   - Missing `average_health_score: float` attribute
   - Missing `total_size_mb: float` attribute

2. **get_dashboard_data() method** (lines 281-288 in `tier1_metadata.py`)
   - Does not populate these fields from GlobalStats when creating DashboardData instances
   - GlobalStats HAS these fields (lines 108-109) but they are not transferred

### Code Locations

**Source File to Fix:**
`/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/global_index/tier1_metadata.py`

**DashboardData class (lines 112-133):**
```python
@dataclass
class DashboardData:
    total_projects: int
    total_symbols: int
    total_files: int
    languages: Dict[str, int]
    projects: List[ProjectMetadata]
    last_updated: float
    # MISSING: average_health_score: float
    # MISSING: total_size_mb: float
```

**get_dashboard_data() method (lines 281-288):**
```python
dashboard = DashboardData(
    total_projects=stats.total_projects,
    total_symbols=stats.total_symbols,
    total_files=stats.total_files,
    languages=stats.languages,
    projects=projects_list,
    last_updated=last_updated
    # MISSING: average_health_score=stats.average_health_score
    # MISSING: total_size_mb=stats.total_size_mb
)
```

### Next Phase: GREEN

The fix requires:
1. Add `average_health_score: float` to DashboardData dataclass
2. Add `total_size_mb: float` to DashboardData dataclass
3. Update `get_dashboard_data()` to populate these fields from GlobalStats
4. Run tests to confirm they pass

### Test Command
```bash
python -m pytest tests/global_index/test_tier1_metadata_dashboard.py -v
```

Or using the module's test runner:
```bash
python tests/global_index/test_tier1_metadata_dashboard.py
```
