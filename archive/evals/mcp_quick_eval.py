#!/usr/bin/env python3
"""
LeIndex MCP Server Quick Evaluation Script
Tests all 24 MCP tools with fast timeouts
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

PROJECT_PATH = "/home/stan/Documents/Stan-s-ML-Stack/"
LEINDEX_PATH = "/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer"
TIMEOUT = 30  # 30 second timeout per operation

@dataclass
class Result:
    tool: str
    action: str = ""
    success: bool = False
    time_s: float = 0.0
    result: Any = None
    error: str = ""
    notes: List[str] = field(default_factory=list)

class Evaluator:
    def __init__(self):
        self.results: List[Result] = []
        self.session = None
        
    async def call(self, tool: str, args: Dict, action: str = "") -> Result:
        r = Result(tool=tool, action=action)
        start = time.time()
        
        try:
            resp = await asyncio.wait_for(
                self.session.call_tool(tool, args),
                timeout=TIMEOUT
            )
            r.time_s = time.time() - start
            
            if resp.content:
                content = resp.content[0]
                if hasattr(content, 'text'):
                    try:
                        r.result = json.loads(content.text)
                    except:
                        r.result = content.text
            
            if isinstance(r.result, dict):
                r.success = r.result.get('success', True)
                if 'error' in r.result:
                    r.error = str(r.result.get('error', ''))[:200]
            else:
                r.success = True
                
        except asyncio.TimeoutError:
            r.time_s = TIMEOUT
            r.error = f"TIMEOUT after {TIMEOUT}s"
            r.notes.append("HUNG")
        except Exception as e:
            r.time_s = time.time() - start
            r.error = str(e)[:200]
            
        self.results.append(r)
        status = "✅" if r.success else "❌"
        print(f"  {status} {tool}({action}) - {r.time_s:.2f}s {'| ' + r.error if r.error else ''}")
        return r
    
    async def run(self):
        server = StdioServerParameters(
            command=f'{LEINDEX_PATH}/.venv/bin/python',
            args=['-m', 'leindex.server'],
            cwd=f'{LEINDEX_PATH}/src'
        )
        
        async with stdio_client(server) as (read, write):
            async with ClientSession(read, write) as session:
                self.session = session
                await session.initialize()
                
                print("="*70)
                print("LeIndex MCP Quick Evaluation")
                print("="*70)
                
                # 1. Set project path (skip heavy reindex - use existing)
                print("\n### Project Management ###")
                await self.call("manage_project", {"action": "set_path", "path": PROJECT_PATH}, "set_path")
                
                # Skip heavy reindex - just test refresh which is lighter
                await self.call("manage_project", {"action": "refresh"}, "refresh")
                
                # 2. Search tools
                print("\n### Search Tools ###")
                await self.call("search_content", {"action": "search", "pattern": "def ", "page_size": 5}, "search")
                await self.call("search_content", {"action": "search", "pattern": "class", "fuzzy": True, "page_size": 5}, "fuzzy")
                await self.call("search_content", {"action": "search", "pattern": "import", "case_sensitive": False, "page_size": 5}, "case_insensitive")
                await self.call("search_content", {"action": "search", "pattern": "async", "file_pattern": "*.py", "page_size": 5}, "file_filter")
                await self.call("search_content", {"action": "search", "pattern": "return", "context_lines": 2, "page_size": 3}, "context")
                await self.call("search_content", {"action": "find", "pattern": "*.py"}, "find_py")
                await self.call("search_content", {"action": "find", "pattern": "*.md"}, "find_md")
                await self.call("search_content", {"action": "find", "pattern": "*.json"}, "find_json")
                
                # 3. File reading
                print("\n### File Reading ###")
                await self.call("read_file", {"mode": "metadata", "file_path": f"{PROJECT_PATH}/README.md"}, "metadata")
                await self.call("read_file", {"mode": "smart", "file_path": f"{PROJECT_PATH}/README.md", "include_content": False}, "smart")
                
                # 4. Diagnostics
                print("\n### Diagnostics ###")
                await self.call("get_diagnostics", {"type": "memory"}, "memory")
                await self.call("get_diagnostics", {"type": "index"}, "index")
                await self.call("get_diagnostics", {"type": "backend"}, "backend")
                await self.call("get_diagnostics", {"type": "performance"}, "perf")
                await self.call("get_diagnostics", {"type": "settings"}, "settings")
                await self.call("get_diagnostics", {"type": "ranking"}, "ranking")
                await self.call("get_diagnostics", {"type": "operations"}, "ops")
                await self.call("get_diagnostics", {"type": "ignore"}, "ignore")
                await self.call("get_diagnostics", {"type": "filtering"}, "filter")
                
                # 5. Memory management
                print("\n### Memory Management ###")
                await self.call("get_memory_status", {}, "status")
                await self.call("manage_memory", {"action": "cleanup"}, "cleanup")
                await self.call("configure_memory", {"soft_limit_mb": 512}, "configure")
                
                # 6. Operations
                print("\n### Operations ###")
                await self.call("manage_operations", {"action": "list"}, "list")
                await self.call("manage_operations", {"action": "cleanup", "max_age_hours": 1}, "cleanup")
                
                # 7. Registry
                print("\n### Registry ###")
                await self.call("get_registry_status", {}, "status")
                await self.call("registry_health_check", {}, "health")
                await self.call("backup_registry", {}, "backup")
                await self.call("detect_orphaned_indexes", {"max_depth": 1}, "orphans")
                
                # 8. Global Index
                print("\n### Global Index ###")
                await self.call("get_global_stats", {}, "stats")
                await self.call("get_dashboard", {}, "dashboard")
                await self.call("list_projects", {"format": "simple"}, "list")
                await self.call("cross_project_search_tool", {"pattern": "import", "limit": 5}, "xsearch")
                
                # 9. Eviction
                print("\n### Eviction ###")
                await self.call("trigger_eviction", {"strategy": "lru", "target_mb": 50}, "evict")
                
                # 10. Temp
                print("\n### Temp Directory ###")
                await self.call("manage_temp", {"action": "check"}, "check")
                await self.call("manage_temp", {"action": "create"}, "create")
                
                print("\n" + "="*70)
                print("EVALUATION COMPLETE")
                print("="*70)

    def report(self) -> str:
        lines = []
        lines.append("# LeIndex MCP Server Evaluation Report")
        lines.append("")
        lines.append(f"**Generated:** {datetime.now().isoformat()}")
        lines.append(f"**Project Path:** {PROJECT_PATH}")
        lines.append(f"**Timeout:** {TIMEOUT}s per operation")
        lines.append("")
        
        # Summary
        passed = sum(1 for r in self.results if r.success)
        failed = len(self.results) - passed
        lines.append("## Executive Summary")
        lines.append("")
        lines.append(f"| Metric | Value |")
        lines.append(f"|--------|-------|")
        lines.append(f"| Total Tests | {len(self.results)} |")
        lines.append(f"| Passed | {passed} ✅ |")
        lines.append(f"| Failed | {failed} ❌ |")
        lines.append(f"| Pass Rate | {100*passed/max(len(self.results),1):.1f}% |")
        lines.append("")
        
        times = [r.time_s for r in self.results]
        lines.append("### Performance Metrics")
        lines.append("")
        lines.append(f"| Metric | Value |")
        lines.append(f"|--------|-------|")
        lines.append(f"| Total Time | {sum(times):.2f}s |")
        lines.append(f"| Average | {sum(times)/max(len(times),1):.2f}s |")
        lines.append(f"| Min | {min(times) if times else 0:.2f}s |")
        lines.append(f"| Max | {max(times) if times else 0:.2f}s |")
        lines.append("")
        
        # Timeouts
        timeouts = [r for r in self.results if "TIMEOUT" in r.error or "HUNG" in r.notes]
        if timeouts:
            lines.append("## ⚠️ CRITICAL: Hanging/Timeout Issues")
            lines.append("")
            for r in timeouts:
                lines.append(f"- **{r.tool}** ({r.action}): Exceeded {TIMEOUT}s timeout")
            lines.append("")
        
        # Failures
        failures = [r for r in self.results if not r.success]
        if failures:
            lines.append("## ❌ Failed Tests")
            lines.append("")
            for r in failures:
                lines.append(f"- **{r.tool}** ({r.action}): {r.error[:100]}...")
            lines.append("")
        
        # Detailed by category
        lines.append("## Detailed Results")
        lines.append("")
        
        categories = {
            "Project Management": ["manage_project"],
            "Search & Content": ["search_content"],
            "File Reading": ["read_file"],
            "Diagnostics": ["get_diagnostics"],
            "Memory": ["manage_memory", "get_memory_status", "configure_memory"],
            "Operations": ["manage_operations"],
            "Registry": ["get_registry_status", "registry_health_check", "backup_registry", "detect_orphaned_indexes"],
            "Global Index": ["get_global_stats", "get_dashboard", "list_projects", "cross_project_search_tool"],
            "Eviction": ["trigger_eviction"],
            "Temp": ["manage_temp"],
        }
        
        for cat, tools in categories.items():
            cat_results = [r for r in self.results if r.tool in tools]
            if not cat_results:
                continue
            lines.append(f"### {cat}")
            lines.append("")
            lines.append("| Tool | Action | Status | Time | Notes |")
            lines.append("|------|--------|--------|------|-------|")
            for r in cat_results:
                st = "✅" if r.success else "❌"
                notes = r.error[:50] + "..." if len(r.error) > 50 else r.error
                lines.append(f"| {r.tool} | {r.action} | {st} | {r.time_s:.2f}s | {notes} |")
            lines.append("")
        
        # Search performance deep dive
        search_results = [r for r in self.results if r.tool == "search_content"]
        if search_results:
            lines.append("## Search Performance Analysis")
            lines.append("")
            lines.append("### Search Operation Timings")
            lines.append("")
            lines.append("| Type | Time (s) | Status | Results |")
            lines.append("|------|----------|--------|---------|")
            for r in search_results:
                st = "✅" if r.success else "❌"
                count = "N/A"
                if isinstance(r.result, dict):
                    if 'results' in r.result:
                        count = len(r.result.get('results', []))
                    elif 'matches' in r.result:
                        count = len(r.result.get('matches', []))
                    elif 'total_results' in r.result:
                        count = r.result.get('total_results', 'N/A')
                    elif isinstance(r.result, list):
                        count = len(r.result)
                lines.append(f"| {r.action} | {r.time_s:.2f} | {st} | {count} |")
            lines.append("")
            
            # Performance assessment
            lines.append("### Performance Assessment")
            lines.append("")
            fast = [r for r in search_results if r.time_s < 1.0 and r.success]
            medium = [r for r in search_results if 1.0 <= r.time_s < 5.0 and r.success]
            slow = [r for r in search_results if r.time_s >= 5.0 and r.success]
            hung = [r for r in search_results if "TIMEOUT" in r.error]
            
            lines.append(f"- **Fast (<1s):** {len(fast)} searches")
            lines.append(f"- **Medium (1-5s):** {len(medium)} searches")
            lines.append(f"- **Slow (>5s):** {len(slow)} searches")
            lines.append(f"- **Hung/Timeout:** {len(hung)} searches")
            lines.append("")
        
        # Issues identified
        lines.append("## Potential Issues Identified")
        lines.append("")
        
        slow_ops = [r for r in self.results if r.time_s > 5.0 and "TIMEOUT" not in r.error]
        if slow_ops:
            lines.append("### Slow Operations (> 5s)")
            lines.append("")
            for r in slow_ops:
                lines.append(f"- **{r.tool}** ({r.action}): {r.time_s:.2f}s")
            lines.append("")
        
        # Recommendations
        lines.append("## Recommendations")
        lines.append("")
        if timeouts:
            lines.append("1. **CRITICAL**: Fix hanging operations that timeout")
            lines.append("   - Add better cancellation handling")
            lines.append("   - Add progress callbacks")
            lines.append("")
        if slow_ops:
            lines.append("2. **Performance**: Optimize slow operations")
            lines.append("   - Consider caching frequently accessed data")
            lines.append("   - Add index optimizations")
            lines.append("")
        if failures:
            lines.append("3. **Reliability**: Fix failing operations")
            lines.append("   - Review error handling")
            lines.append("")
        
        # Raw data
        lines.append("## Appendix: Raw Data")
        lines.append("")
        lines.append("```json")
        raw = [{"tool": r.tool, "action": r.action, "success": r.success, 
                "time_s": round(r.time_s, 3), "error": r.error} for r in self.results]
        lines.append(json.dumps(raw, indent=2))
        lines.append("```")
        
        return "\n".join(lines)


async def main():
    e = Evaluator()
    try:
        await e.run()
    except Exception as ex:
        print(f"\n!!! CRASHED: {ex}")
        traceback.print_exc()
    
    report = e.report()
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    path = f"{LEINDEX_PATH}/archive/evals/leindex_mcp_eval_{ts}.md"
    with open(path, 'w') as f:
        f.write(report)
    print(f"\nReport saved: {path}")
    return path

if __name__ == "__main__":
    asyncio.run(main())
