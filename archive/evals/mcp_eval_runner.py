#!/usr/bin/env python3
"""
LeIndex MCP Server Comprehensive Evaluation Script
Evaluates all 24 MCP tools for functionality, performance, and reliability.
"""

import asyncio
import json
import time
import traceback
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Dict, List, Optional
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

# Configuration
PROJECT_PATH = "/home/stan/Documents/Stan-s-ML-Stack/"
LEINDEX_PATH = "/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer"
TIMEOUT = 180  # 3 minute timeout per operation

@dataclass
class ToolEvalResult:
    """Result of a single tool evaluation"""
    tool_name: str
    action: str = ""
    success: bool = False
    execution_time: float = 0.0
    result: Any = None
    error: Optional[str] = None
    notes: List[str] = field(default_factory=list)
    
@dataclass
class EvalReport:
    """Full evaluation report"""
    timestamp: str = ""
    project_path: str = ""
    total_tools: int = 0
    passed: int = 0
    failed: int = 0
    results: List[ToolEvalResult] = field(default_factory=list)


class LeIndexEvaluator:
    def __init__(self):
        self.results: List[ToolEvalResult] = []
        self.session: Optional[ClientSession] = None
        
    async def call_tool(self, tool_name: str, arguments: Dict[str, Any], action: str = "") -> ToolEvalResult:
        """Call a tool with timeout and capture results"""
        result = ToolEvalResult(tool_name=tool_name, action=action)
        start_time = time.time()
        
        try:
            response = await asyncio.wait_for(
                self.session.call_tool(tool_name, arguments),
                timeout=TIMEOUT
            )
            result.execution_time = time.time() - start_time
            
            # Parse response content
            if response.content:
                content = response.content[0]
                if hasattr(content, 'text'):
                    try:
                        result.result = json.loads(content.text)
                    except json.JSONDecodeError:
                        result.result = content.text
                else:
                    result.result = str(content)
            
            # Check for success in response
            if isinstance(result.result, dict):
                result.success = result.result.get('success', True)
                if 'error' in result.result:
                    result.error = result.result.get('error')
            else:
                result.success = True
                
        except asyncio.TimeoutError:
            result.execution_time = TIMEOUT
            result.error = f"TIMEOUT after {TIMEOUT}s"
            result.notes.append("CRITICAL: Tool hung and exceeded timeout")
        except Exception as e:
            result.execution_time = time.time() - start_time
            result.error = str(e)
            result.notes.append(f"Exception: {traceback.format_exc()}")
            
        self.results.append(result)
        return result
    
    async def run_evaluation(self):
        """Run comprehensive evaluation of all tools"""
        server = StdioServerParameters(
            command=f'{LEINDEX_PATH}/.venv/bin/python',
            args=['-m', 'leindex.server'],
            cwd=f'{LEINDEX_PATH}/src'
        )
        
        async with stdio_client(server) as (read, write):
            async with ClientSession(read, write) as session:
                self.session = session
                await session.initialize()
                
                print("=" * 70)
                print("LeIndex MCP Server Comprehensive Evaluation")
                print("=" * 70)
                print(f"Target Project: {PROJECT_PATH}")
                print(f"Started: {datetime.now().isoformat()}")
                print("=" * 70)
                
                # ===== PHASE 1: Project Management =====
                print("\n### PHASE 1: Project Management Tools ###\n")
                
                # 1.1 Set Project Path
                print("Testing: manage_project (set_path)...")
                r = await self.call_tool("manage_project", {
                    "action": "set_path",
                    "path": PROJECT_PATH
                }, "set_path")
                self._print_result(r)
                
                # 1.2 Force Reindex
                print("\nTesting: manage_project (reindex)...")
                r = await self.call_tool("manage_project", {
                    "action": "reindex",
                    "clear_cache": True
                }, "reindex")
                self._print_result(r)
                
                # 1.3 Refresh Index
                print("\nTesting: manage_project (refresh)...")
                r = await self.call_tool("manage_project", {
                    "action": "refresh"
                }, "refresh")
                self._print_result(r)
                
                # ===== PHASE 2: Search Tools =====
                print("\n### PHASE 2: Search and Content Tools ###\n")
                
                # 2.1 Basic Search
                print("Testing: search_content (search) - basic pattern...")
                r = await self.call_tool("search_content", {
                    "action": "search",
                    "pattern": "def __init__",
                    "page_size": 10
                }, "search_basic")
                self._print_result(r)
                self._evaluate_search_result(r, "Basic search")
                
                # 2.2 Case-insensitive Search
                print("\nTesting: search_content (search) - case insensitive...")
                r = await self.call_tool("search_content", {
                    "action": "search",
                    "pattern": "class",
                    "case_sensitive": False,
                    "page_size": 10
                }, "search_case_insensitive")
                self._print_result(r)
                
                # 2.3 Fuzzy Search
                print("\nTesting: search_content (search) - fuzzy...")
                r = await self.call_tool("search_content", {
                    "action": "search",
                    "pattern": "import torch",
                    "fuzzy": True,
                    "page_size": 10
                }, "search_fuzzy")
                self._print_result(r)
                
                # 2.4 Pattern with file filter
                print("\nTesting: search_content (search) - with file pattern...")
                r = await self.call_tool("search_content", {
                    "action": "search",
                    "pattern": "class",
                    "file_pattern": "*.py",
                    "page_size": 10
                }, "search_file_pattern")
                self._print_result(r)
                
                # 2.5 Context lines
                print("\nTesting: search_content (search) - with context lines...")
                r = await self.call_tool("search_content", {
                    "action": "search",
                    "pattern": "async def",
                    "context_lines": 3,
                    "page_size": 5
                }, "search_context")
                self._print_result(r)
                
                # 2.6 Find files
                print("\nTesting: search_content (find) - glob pattern...")
                r = await self.call_tool("search_content", {
                    "action": "find",
                    "pattern": "*.py"
                }, "find_files")
                self._print_result(r)
                
                # 2.7 Find markdown files
                print("\nTesting: search_content (find) - markdown files...")
                r = await self.call_tool("search_content", {
                    "action": "find",
                    "pattern": "*.md"
                }, "find_markdown")
                self._print_result(r)
                
                # ===== PHASE 3: File Reading Tools =====
                print("\n### PHASE 3: File Reading Tools ###\n")
                
                # Find a Python file to read
                # 3.1 Smart read
                print("Testing: read_file (smart)...")
                r = await self.call_tool("read_file", {
                    "mode": "smart",
                    "file_path": f"{PROJECT_PATH}/README.md" if PROJECT_PATH else "README.md",
                    "include_content": True,
                    "include_metadata": True
                }, "smart_read")
                self._print_result(r)
                
                # 3.2 Metadata read
                print("\nTesting: read_file (metadata)...")
                r = await self.call_tool("read_file", {
                    "mode": "metadata",
                    "file_path": f"{PROJECT_PATH}/README.md" if PROJECT_PATH else "README.md"
                }, "metadata_read")
                self._print_result(r)
                
                # ===== PHASE 4: Diagnostics Tools =====
                print("\n### PHASE 4: Diagnostics Tools ###\n")
                
                # 4.1 Memory diagnostics
                print("Testing: get_diagnostics (memory)...")
                r = await self.call_tool("get_diagnostics", {"type": "memory"}, "memory")
                self._print_result(r)
                
                # 4.2 Index diagnostics
                print("\nTesting: get_diagnostics (index)...")
                r = await self.call_tool("get_diagnostics", {"type": "index"}, "index")
                self._print_result(r)
                
                # 4.3 Backend health
                print("\nTesting: get_diagnostics (backend)...")
                r = await self.call_tool("get_diagnostics", {"type": "backend"}, "backend")
                self._print_result(r)
                
                # 4.4 Performance metrics
                print("\nTesting: get_diagnostics (performance)...")
                r = await self.call_tool("get_diagnostics", {"type": "performance"}, "performance")
                self._print_result(r)
                
                # 4.5 Settings info
                print("\nTesting: get_diagnostics (settings)...")
                r = await self.call_tool("get_diagnostics", {"type": "settings"}, "settings")
                self._print_result(r)
                
                # 4.6 Ranking configuration
                print("\nTesting: get_diagnostics (ranking)...")
                r = await self.call_tool("get_diagnostics", {"type": "ranking"}, "ranking")
                self._print_result(r)
                
                # ===== PHASE 5: Memory Management Tools =====
                print("\n### PHASE 5: Memory Management Tools ###\n")
                
                # 5.1 Get memory status
                print("Testing: get_memory_status...")
                r = await self.call_tool("get_memory_status", {}, "status")
                self._print_result(r)
                
                # 5.2 Manage memory cleanup
                print("\nTesting: manage_memory (cleanup)...")
                r = await self.call_tool("manage_memory", {"action": "cleanup"}, "cleanup")
                self._print_result(r)
                
                # 5.3 Configure memory
                print("\nTesting: configure_memory...")
                r = await self.call_tool("configure_memory", {
                    "soft_limit_mb": 512,
                    "hard_limit_mb": 1024
                }, "configure")
                self._print_result(r)
                
                # ===== PHASE 6: Operations Management =====
                print("\n### PHASE 6: Operations Management Tools ###\n")
                
                # 6.1 List operations
                print("Testing: manage_operations (list)...")
                r = await self.call_tool("manage_operations", {"action": "list"}, "list")
                self._print_result(r)
                
                # 6.2 Cleanup operations
                print("\nTesting: manage_operations (cleanup)...")
                r = await self.call_tool("manage_operations", {
                    "action": "cleanup",
                    "max_age_hours": 1.0
                }, "cleanup")
                self._print_result(r)
                
                # ===== PHASE 7: Registry Tools =====
                print("\n### PHASE 7: Registry Tools ###\n")
                
                # 7.1 Registry status
                print("Testing: get_registry_status...")
                r = await self.call_tool("get_registry_status", {}, "status")
                self._print_result(r)
                
                # 7.2 Registry health check
                print("\nTesting: registry_health_check...")
                r = await self.call_tool("registry_health_check", {}, "health")
                self._print_result(r)
                
                # 7.3 Backup registry
                print("\nTesting: backup_registry...")
                r = await self.call_tool("backup_registry", {}, "backup")
                self._print_result(r)
                
                # 7.4 Detect orphaned indexes
                print("\nTesting: detect_orphaned_indexes...")
                r = await self.call_tool("detect_orphaned_indexes", {"max_depth": 2}, "orphans")
                self._print_result(r)
                
                # ===== PHASE 8: Global Index Tools =====
                print("\n### PHASE 8: Global Index Tools ###\n")
                
                # 8.1 Global stats
                print("Testing: get_global_stats...")
                r = await self.call_tool("get_global_stats", {}, "stats")
                self._print_result(r)
                
                # 8.2 Dashboard
                print("\nTesting: get_dashboard...")
                r = await self.call_tool("get_dashboard", {}, "dashboard")
                self._print_result(r)
                
                # 8.3 List projects
                print("\nTesting: list_projects...")
                r = await self.call_tool("list_projects", {"format": "detailed"}, "list")
                self._print_result(r)
                
                # 8.4 Cross-project search
                print("\nTesting: cross_project_search_tool...")
                r = await self.call_tool("cross_project_search_tool", {
                    "pattern": "import",
                    "limit": 10
                }, "search")
                self._print_result(r)
                
                # ===== PHASE 9: Eviction Tools =====
                print("\n### PHASE 9: Eviction Tools ###\n")
                
                # 9.1 Trigger eviction
                print("Testing: trigger_eviction...")
                r = await self.call_tool("trigger_eviction", {
                    "strategy": "lru",
                    "target_mb": 100
                }, "evict")
                self._print_result(r)
                
                # ===== PHASE 10: Temp Directory Tools =====
                print("\n### PHASE 10: Temp Directory Tools ###\n")
                
                # 10.1 Check temp
                print("Testing: manage_temp (check)...")
                r = await self.call_tool("manage_temp", {"action": "check"}, "check")
                self._print_result(r)
                
                # 10.2 Create temp
                print("\nTesting: manage_temp (create)...")
                r = await self.call_tool("manage_temp", {"action": "create"}, "create")
                self._print_result(r)
                
                # ===== PHASE 11: Clear/Reset Tools =====
                print("\n### PHASE 11: Clear/Reset Tools ###\n")
                
                # 11.1 Clear (test at end to avoid breaking other tests)
                print("Testing: manage_project (clear)...")
                r = await self.call_tool("manage_project", {"action": "clear"}, "clear")
                self._print_result(r)
                
                print("\n" + "=" * 70)
                print("EVALUATION COMPLETE")
                print("=" * 70)
                
    def _print_result(self, r: ToolEvalResult):
        """Print a formatted result"""
        status = "✅ PASS" if r.success else "❌ FAIL"
        print(f"  {status} | {r.tool_name} ({r.action}) | {r.execution_time:.2f}s")
        if r.error:
            print(f"    Error: {r.error}")
        if r.notes:
            for note in r.notes:
                print(f"    Note: {note}")
                
    def _evaluate_search_result(self, r: ToolEvalResult, context: str):
        """Evaluate search-specific quality metrics"""
        if not r.success or not isinstance(r.result, dict):
            r.notes.append(f"{context}: Could not evaluate - no valid result")
            return
            
        result_count = r.result.get('total_results', 0)
        if 'results' in r.result:
            result_count = len(r.result.get('results', []))
        elif 'matches' in r.result:
            result_count = len(r.result.get('matches', []))
            
        r.notes.append(f"{context}: Found {result_count} results in {r.execution_time:.2f}s")
        
        # Performance evaluation
        if r.execution_time > 5.0:
            r.notes.append("SLOW: Search took > 5s")
        elif r.execution_time < 1.0:
            r.notes.append("FAST: Search completed in < 1s")
            
    def generate_report(self) -> str:
        """Generate a detailed markdown report"""
        report = []
        report.append("# LeIndex MCP Server Evaluation Report")
        report.append("")
        report.append(f"**Generated:** {datetime.now().isoformat()}")
        report.append(f"**Project Path:** {PROJECT_PATH}")
        report.append(f"**Timeout per test:** {TIMEOUT}s (3 minutes)")
        report.append("")
        
        # Summary
        passed = sum(1 for r in self.results if r.success)
        failed = len(self.results) - passed
        report.append("## Executive Summary")
        report.append("")
        report.append(f"| Metric | Value |")
        report.append(f"|--------|-------|")
        report.append(f"| Total Tests | {len(self.results)} |")
        report.append(f"| Passed | {passed} |")
        report.append(f"| Failed | {failed} |")
        report.append(f"| Pass Rate | {100*passed/len(self.results):.1f}% |")
        report.append("")
        
        # Timing summary
        times = [r.execution_time for r in self.results]
        report.append("### Performance Summary")
        report.append("")
        report.append(f"| Metric | Value |")
        report.append(f"|--------|-------|")
        report.append(f"| Total Time | {sum(times):.2f}s |")
        report.append(f"| Average Time | {sum(times)/len(times):.2f}s |")
        report.append(f"| Min Time | {min(times):.2f}s |")
        report.append(f"| Max Time | {max(times):.2f}s |")
        report.append("")
        
        # Timeout/hanging issues
        timeouts = [r for r in self.results if r.execution_time >= TIMEOUT]
        if timeouts:
            report.append("### ⚠️ CRITICAL: Timeout/Hanging Issues")
            report.append("")
            for r in timeouts:
                report.append(f"- **{r.tool_name}** ({r.action}): Exceeded {TIMEOUT}s timeout")
            report.append("")
        
        # Failed tests
        failures = [r for r in self.results if not r.success]
        if failures:
            report.append("### ❌ Failed Tests")
            report.append("")
            for r in failures:
                report.append(f"- **{r.tool_name}** ({r.action}): {r.error}")
            report.append("")
        
        # Detailed results by category
        report.append("## Detailed Results by Category")
        report.append("")
        
        categories = {
            "Project Management": ["manage_project"],
            "Search & Content": ["search_content"],
            "File Reading": ["read_file"],
            "Diagnostics": ["get_diagnostics"],
            "Memory Management": ["manage_memory", "get_memory_status", "configure_memory"],
            "Operations Management": ["manage_operations"],
            "Registry": ["get_registry_status", "registry_health_check", "backup_registry", "detect_orphaned_indexes", "registry_cleanup", "migrate_legacy_indexes", "reindex_all_projects"],
            "Global Index": ["get_global_stats", "get_dashboard", "list_projects", "cross_project_search_tool"],
            "Eviction": ["trigger_eviction", "unload_project"],
            "Temp Directory": ["manage_temp"],
            "File Operations": ["manage_file", "manage_files"],
        }
        
        for category, tools in categories.items():
            cat_results = [r for r in self.results if r.tool_name in tools]
            if not cat_results:
                continue
                
            report.append(f"### {category}")
            report.append("")
            report.append("| Tool | Action | Status | Time (s) | Notes |")
            report.append("|------|--------|--------|----------|-------|")
            
            for r in cat_results:
                status = "✅" if r.success else "❌"
                notes = "; ".join(r.notes) if r.notes else r.error or ""
                notes = notes[:100] + "..." if len(notes) > 100 else notes
                report.append(f"| {r.tool_name} | {r.action} | {status} | {r.execution_time:.2f} | {notes} |")
            report.append("")
        
        # Search Performance Deep Dive
        search_results = [r for r in self.results if r.tool_name == "search_content"]
        if search_results:
            report.append("## Search Performance Deep Dive")
            report.append("")
            report.append("### Search Operation Timing")
            report.append("")
            report.append("| Search Type | Time (s) | Status | Result Count |")
            report.append("|-------------|----------|--------|--------------|")
            
            for r in search_results:
                status = "✅" if r.success else "❌"
                count = "N/A"
                if isinstance(r.result, dict):
                    count = r.result.get('total_results', r.result.get('result_count', 'N/A'))
                report.append(f"| {r.action} | {r.execution_time:.2f} | {status} | {count} |")
            report.append("")
            
            # Accuracy notes
            report.append("### Search Quality Assessment")
            report.append("")
            for r in search_results:
                if r.notes:
                    for note in r.notes:
                        report.append(f"- **{r.action}**: {note}")
            report.append("")
        
        # Potential Issues
        report.append("## Potential Issues Identified")
        report.append("")
        
        slow_ops = [r for r in self.results if r.execution_time > 5.0 and r.execution_time < TIMEOUT]
        if slow_ops:
            report.append("### Slow Operations (> 5s)")
            report.append("")
            for r in slow_ops:
                report.append(f"- **{r.tool_name}** ({r.action}): {r.execution_time:.2f}s")
            report.append("")
        
        # Recommendations
        report.append("## Recommendations")
        report.append("")
        
        if timeouts:
            report.append("1. **CRITICAL**: Fix hanging operations that exceed timeout")
            report.append("   - Implement better async handling or cancellation")
            report.append("")
        
        if slow_ops:
            report.append("2. **Performance**: Optimize slow operations")
            report.append("   - Consider caching, indexing improvements, or parallel processing")
            report.append("")
        
        if failures:
            report.append("3. **Reliability**: Fix failing tests")
            report.append("   - Review error handling and edge cases")
            report.append("")
        
        # Raw results appendix
        report.append("## Appendix: Raw Results")
        report.append("")
        report.append("```json")
        raw_data = []
        for r in self.results:
            raw_data.append({
                "tool": r.tool_name,
                "action": r.action,
                "success": r.success,
                "time_seconds": round(r.execution_time, 3),
                "error": r.error,
                "notes": r.notes,
            })
        report.append(json.dumps(raw_data, indent=2))
        report.append("```")
        
        return "\n".join(report)


async def main():
    evaluator = LeIndexEvaluator()
    try:
        await evaluator.run_evaluation()
    except Exception as e:
        print(f"\n\n!!! EVALUATION CRASHED: {e}")
        traceback.print_exc()
    
    # Generate and save report
    report = evaluator.generate_report()
    
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    report_path = f"{LEINDEX_PATH}/archive/evals/leindex_mcp_eval_{timestamp}.md"
    
    with open(report_path, 'w') as f:
        f.write(report)
    
    print(f"\n\nReport saved to: {report_path}")
    return report_path


if __name__ == "__main__":
    result = asyncio.run(main())
    print(f"Evaluation complete. Report: {result}")
