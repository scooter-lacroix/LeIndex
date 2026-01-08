# Product Guidelines: LeIndex

## Communication Style & Tone

### Voice: Friendly, Playful, & Approachable ✨

LeIndex speaks to developers like a knowledgeable colleague who's excited about code search. We're formal enough to be trustworthy, but casual enough to be fun.

**Key Characteristics:**

1. **Friendly & Conversational**
   - Use "you" and "we" to build connection
   - Write as if talking to a fellow developer over coffee
   - Be enthusiastic without being hype-y
   - Example: "You'll be searching in under 2 minutes. It's easier than making coffee! ☕"

2. **Playful Personality**
   - Emojis are encouraged! ✨ They add warmth and personality
   - Use humor sparingly but effectively
   - Celebrate wins and acknowledge pain points
   - Example: "Boom! You're now searching your codebase at the speed of thought. 🎉"

3. **Clear & Accessible**
   - Explain technical concepts simply
   - Provide concrete examples for abstract ideas
   - Assume intelligence but not domain expertise
   - Avoid jargon unless absolutely necessary (then explain it)

4. **Empathetic to Developer Pain**
   - Acknowledge real problems developers face
   - Show understanding of frustration with existing tools
   - Position LeIndex as the solution to actual pain points
   - Example: "Text-based search tools miss code that uses different terminology. We've been there."

**What This Looks Like:**

✅ **Good:** "LeIndex isn't just another code search tool. It's your intelligent code companion that actually understands what you're looking for! 🎯"

❌ **Bad:** "LeIndex is a semantic code search system utilizing vector embeddings and natural language processing."

✅ **Good:** "That's literally it. No Docker. No databases. No headaches. Just works. ✨"

❌ **Bad:** "Installation requires no additional infrastructure components."

---

## Core Design Principles (Prioritized)

### 1️⃣ SIMPLICITY FIRST (Highest Priority) 🎯

**Everything necessary is packaged. Users never manually install dependencies.**

**What This Means:**
- `pip install leindex` or `install.sh` handles ALL dependencies automatically
- No "first, install Tantivy, then configure PyTorch, then..."
- Installers bundle everything needed
- Zero manual configuration for 90% of use cases
- Works out of the box on any system with Python 3.10+

**In Practice:**
- Dependencies like `tantivy`, `leann`, `sentence-transformers` are all specified in `pyproject.toml`
- Install scripts detect system and install appropriate packages
- Users shouldn't need to read installation docs (but they're there if needed)
- "It just works" isn't a slogan—it's a requirement

**Tradeoffs We Accept:**
- Larger install size (better than complex setup)
- Longer install time (one-time cost, worth it)
- Occasional dependency conflicts (we handle them in installers)

---

### 2️⃣ PERFORMANCE OBSESSION (Second Priority) ⚡

**Speed is not optional. Fast is a feature, not a nice-to-have.**

**What This Means:**
- Index 50K files in <60 seconds (target: <30 seconds)
- Search latency: P50 <100ms, P95 <500ms
- No blocking operations in async contexts
- Every millisecond counts in indexing and search
- Optimize for the 90th percentile, not just average

**In Practice:**
- Profile before optimizing (measure, don't guess)
- Eliminate redundant I/O operations
- Cache aggressively but intelligently
- Use async/await throughout (no blocking calls)
- Parallel processing when beneficial
- Benchmark every performance change

**Non-Negotiables:**
- Indexing should never feel slow
- Search results should feel instant
- Startup time <2 seconds
- Memory footprint: <2GB during indexing, <500MB idle

**Monitoring:**
- Track indexing time for common repo sizes
- Log search latency percentiles
- Memory profiling on every release
- Performance tests block PRs

---

### 3️⃣ DEVELOPER EMPATHY (Third Priority) 🧠

**Design for the developer's mental model, not the system's convenience.**

**What This Means:**
- Clear, actionable error messages (not "Error 404: file not found")
- Sensible defaults that match developer expectations
- CLI tools work as developers expect (flags, options, help text)
- Documentation answers "how do I..." not just "what does..."
- Consider the entire developer journey, not just the happy path

**In Practice:**
- Error messages suggest fixes: "Can't find index. Run `leindex index /path/to/project` first."
- CLI flags follow POSIX conventions (`--verbose`, `-v`, `--help`)
- Examples in docs are copy-pasteable
- Common tasks are simple, advanced tasks are possible
- Fail gracefully with helpful messages

**Questions We Ask:**
- What would a developer expect this to do?
- What's the most intuitive way to use this?
- What error would confuse a developer?
- How can we make this 10x easier to use?

---

### 4️⃣ PRIVACY BY DEFAULT (Equal Priority to #3) 🔒

**Local-first, no telemetry, no data leaves the user's machine.**

**What This Means:**
- Everything runs locally on the user's machine
- No phone-home, no telemetry, no analytics
- No API keys, no cloud services, no external dependencies
- Works completely offline after installation
- User owns their code, their index, their data

**In Practice:**
- No network calls except for package installation
- No usage tracking, no error reporting servers
- No "sign up for an account" prompts
- All data stored locally (SQLite, messagepack files)
- Installation works on air-gapped systems

**What We Don't Do:**
- No "improve LeIndex by sending anonymous usage data"
- No "create an account to sync your settings"
- No "cloud backup for your indexes"
- No "join our community newsletter" prompts

**Tradeoffs:**
- We can't see how users use LeIndex (and that's okay)
- We can't automatically detect errors (rely on GitHub issues)
- We can't provide cloud sync (users can build on top if needed)

---

## Code Quality Standards

### Production-Ready Rigor 🛡️

LeIndex is a critical developer tool. Quality is not optional.

**Test Coverage:**
- **Minimum 95% coverage** for core indexing and search paths
- 100% coverage for critical error handling paths
- Integration tests for MCP server tools
- Performance tests block PRs that degrade indexing/search speed
- Property-based tests for complex algorithms (ignore patterns, ranking)

**Type Hints:**
- Type hints on **all** public APIs
- Type hints on internal functions with complex signatures
- Use `mypy` in strict mode
- No `Any` types without explicit justification
- Document why type checking is suppressed

**Error Handling:**
- Every `except` block logs the error with context
- User-facing errors are actionable (not just stack traces)
- Graceful degradation when possible (fallback search backends)
- Timeout protection on all I/O operations
- No silent failures

**Code Review Standards:**
- All PRs reviewed by at least one maintainer
- Tests required for all new features
- Documentation required for public API changes
- Performance benchmarks for indexing/search changes
- Changelog entry for all user-visible changes

**Documentation:**
- Every public function has a docstring
- Complex algorithms have explanatory comments
- README reflects current capabilities (not outdated features)
- API docs are auto-generated from docstrings
- Architecture diagrams for major components

---

## Versioning & Release Philosophy

### Agile/Continuous Deployment 🚀

**Frequent releases, clear communication, minimal ceremony.**

**Version Numbers:**
- Versions are identifiers, not contracts
- Format: `MAJOR.MINOR.PATCH` (e.g., `1.0.8`, `1.1.0`, `2.0.0`)
- Increment based on scope of change, not semver dogma
- Focus on communication, not version number rules

**Release Cadence:**
- Release when ready (not time-based)
- Small, frequent releases preferred over large batches
- Hotfixes released immediately for critical bugs
- Feature releases can include multiple improvements

**Changelog Requirements:**
- **EVERY release includes a changelog entry**
- Categorize changes: Added, Changed, Fixed, Removed
- Mention breaking changes clearly at the top
- Include migration guides for significant changes
- Link to relevant issues/PRs

**Deprecation Policy:**
- Deprecate features before removing (one release minimum)
- Document why something is deprecated
- Provide migration path to replacement
- Remove deprecated features in next major version

**Communication:**
- GitHub releases for every version
- Clear summary of what's new/changed
- Upgrade instructions when needed
- Known issues listed transparently

---

## Visual Identity & Formatting Standards

### Playful & Engaging 🎨

**Documentation Style:**
- Use **emojis** to add warmth and visual breaks ✨
- Use **headings liberally** to organize content
- Use **code blocks** for all examples
- Use **bold** for emphasis (not all caps)
- Use **lists** for readability (not walls of text)

**Tone Markers:**
- ✨ Excitement/New Features
- ⚡ Performance
- 🔒 Privacy/Security
- 🎯 Focus/Goals
- 🛡️ Quality/Reliability
- 🚀 Speed/Deployment
- 🧠 Understanding/Intelligence
- 💾 Data/Storage
- ⚙️ Configuration/Setup

**Code Comments:**
- Be concise but explanatory
- Explain **why**, not just **what**
- Use humor sparingly (production code is serious business)
- Document non-obvious performance optimizations
- Reference GitHub issues for complex algorithms

**README Style:**
- Start with what it does, not how it works
- Use emojis in section headers
- Provide quick start immediately (don't bury the lede)
- Include badges (version, Python, license, etc.)
- Use code blocks for commands (not screenshots)

**Example Formatting:**

```markdown
## ⚡ Blazing Fast Performance

LeIndex indexes your codebase in seconds, not minutes. Here's how:

- **Parallel Processing**: Index multiple files simultaneously
- **Smart Caching**: Never index the same file twice
- **Incremental Updates**: Only process what changed

**Result:** 50K files indexed in <30 seconds! 🎉
```

**What We Avoid:**
- All caps headings (feels like shouting)
- Over-formatting (too many emojis is chaos)
- Walls of text (break it up!)
- Screenshot-heavy docs (code is copy-pasteable)
- Corporate jargon ("synergy," "leverage," "paradigm shift")

---

## Design Patterns & Architectural Guidelines

### Core Architectural Principles

**1. Modular Over Monolithic**
- Each component has a single responsibility
- Clear interfaces between modules (storage, search, indexing)
- Easy to swap backends (LEANN ↔ FAISS, Tantivy ↔ Elasticsearch)
- Plugin architecture for search strategies

**2. Async-First**
- All I/O operations use async/await
- No blocking calls in async functions
- Use `asyncio.to_thread()` for CPU-bound work
- Event loop never blocks (performance requirement)

**3. Fail Gracefully**
- Fallback search backends if primary fails
- Degrade functionality rather than crash
- Log errors but continue operating
- User can always search with grep as last resort

**4. Test-Driven Where It Matters**
- Test public APIs comprehensively
- Test error cases thoroughly
- Test performance characteristics
- Don't test implementation details (they change)

---

## Community & Contribution Guidelines

### How We Work Together

**For Contributors:**
- Friendly, welcoming community
- Constructive feedback on PRs
- Mentorship for new contributors
- Recognition for contributions (changelog, credits)

**Issue Triage:**
- Respond to all issues within 48 hours
- Reproducible bugs get prioritized
- Feature requests tagged and discussed
- PRs reviewed within one week

**Code Review Culture:**
- Respectful, constructive feedback
- Explain **why** changes are requested
- Help contributors improve their PRs
- No "perfect is the enemy of good"

**Release Process:**
- Maintainers approve releases
- Changelog updated automatically from PRs
- Version bump based on scope of changes
- GitHub release created with summary

---

## Quality Assurance Checklist

Before merging any PR to `main`, verify:

### Functionality
- [ ] Tests pass locally and in CI
- [ ] Manual testing completed for user-facing changes
- [ ] Edge cases considered and handled
- [ ] Error messages are clear and actionable

### Performance
- [ ] No regression in indexing speed (measure it!)
- [ ] No regression in search latency (benchmark it!)
- [ ] Memory usage within acceptable limits
- [ ] No new blocking operations in async contexts

### Documentation
- [ ] Changelog updated
- [ ] API docs updated (if public API changed)
- [ ] README updated (if user-facing features changed)
- [ ] Examples tested and working

### Code Quality
- [ ] Type hints added/updated
- [ ] Error handling comprehensive
- [ ] Logging added for debugging
- [ ] Code follows existing patterns

### Privacy & Security
- [ ] No new external dependencies (or justified)
- [ ] No telemetry or analytics added
- [ ] No data leaves user's machine
- [ ] No hardcoded credentials or secrets

---

**Remember:** LeIndex is a developer tool built by developers, for developers. Every design decision should make a developer's life easier, faster, or more joyful. That's our north star. 🌟
