# LeIndex

<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="600"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![MCP Server](https://img.shields.io/badge/MCP-Server-blue?style=for-the-badge)](https://modelcontextprotocol.io)
[![Tests](https://img.shields.io/badge/Tests-339%2F339-passing-success?style=for-the-badge)](https://github.com/scooter-lacroix/leindex)
[![License](https://img.shields.io/badge/License-MIT-yellow?style=for-the-badge)](LICENSE)
[![Version](https://img.shields.io/badge/Version-0.1.0-blue?style=for-the-badge)](CHANGELOG.md)

**纯 Rust 代码搜索与分析引擎**

*高速语义代码搜索，结合零拷贝解析、PDG 分析、重力式遍历与智能内存管理。*

</div>

---

## LeIndex 是什么？

**LeIndex** 是一个 **纯 Rust** 的代码搜索与分析系统。它将零拷贝解析、语义理解与高效存储组合在一起，用于快速理解大型代码库。

### 核心能力

- **零拷贝 AST 提取**：基于 tree-sitter，支持 12 种语言
- **程序依赖图（PDG）**：支持复杂代码关系分析与重力遍历
- **HNSW 向量检索**：生产级语义相似度搜索
- **自然语言查询**：支持意图识别（HowWorks / WhereHandled / Bottlenecks / Semantic / Text）
- **MCP Server**：原生 Model Context Protocol 集成
- **内存高效**：RSS 监控 + 缓存溢写/重载/预热
- **跨项目能力**：全局符号表支持跨项目解析
- **纯 Rust CLI**：核心命令 `index/search/analyze/diagnostics/serve`

---

## 架构

LeIndex 由 5 个生产可用 Rust crate 组成：

| Crate | 作用 | 状态 | 测试 |
|-------|------|------|------|
| **leparse** | 零拷贝 AST 提取 | ✅ Production Ready | 97/97 |
| **legraphe** | PDG 分析与重力遍历 | ✅ Production Ready | 38/38 |
| **lerecherche** | HNSW 语义检索 + NL 查询 | ✅ Production Ready | 87/87 |
| **lestockage** | SQLite 存储 + 跨项目能力 | ✅ Production Ready | 45/45 |
| **lepasserelle** | CLI + MCP Server | ✅ Production Ready | 72/72 |
| **Total** |  |  | **339/339** |

### 架构图

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                             LeIndex v0.1.0 Architecture                                 │
├─────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                         │
│  ┌──────────────────────┐  ┌──────────────────────┐                                     │
│  │     CLI Commands     │  │     MCP Server       │                                     │
│  │  index, search,      │  │    JSON-RPC 2.0      │                                     │
│  │  analyze, diag, serve│  │   (axum HTTP)        │                                     │
│  └──────────┬───────────┘  └──────────┬───────────┘                                     │
│             │                         │                                                 │
│             └────────────┬────────────┘                                                 │
│                          ▼                                                              │
│  ┌────────────────────────────────────────────────────────────────┐                     │
│  │                  LeIndex Orchestration                         │                     │
│  │              (lepasserelle - 675 lines)                        │                     │
│  │  • Project indexing • Search • Analysis • Diagnostics          │                     │
│  │  • Cache spilling/reloading/warming • Memory monitoring        │                     │
│  └─────┬─────────┬───────────┬───────────┬────────────┬───────────┘                     │
│        │         │           │           │            │                                 │
│  ┌─────▼───┐ ┌───▼────┐ ┌────▼────┐ ┌────▼────┐ ┌─────▼───────┐                         │
│  │ leparse │ │legraphe│ │lerech   │ │lestock  │ │   Cache     │                         │
│  │         │ │        │ │ erche   │ │ age     │ │ Management  │                         │
│  │12 langs │ │  PDG   │ │ HNSW    │ │ SQLite  │ │ RSS Monitor │                         │
│  │zero-copy│ │gravity │ │ NL Q    │ │ global  │ │ Spill/Reload│                         │
│  │ tree-   │ │traverse│ │ hybrid  │ │ symbols │ │ 4 Warm Strat│                         │
│  │ sitter  │ │ embed  │ │ semantic│ │ PDG     │ │             │                         │
│  └─────────┘ └────────┘ └─────────┘ └─────────┘ └─────────────┘                         │
│                                                                                         │
│  Technologies:                                                                          │
│  • Parsing: tree-sitter (12 langs) • Rayon parallel processing                          │
│  • Graph: petgraph StableGraph • Gravity traversal w/ priority queue                    │
│  • Search: HNSW (hnsw-rs) • Cosine similarity • NL query parser                         │
│  • Storage: SQLite + BLAKE3 hashing • Vector embeddings • Cross-project global symbols  │
│  • Server: axum + tokio • JSON-RPC 2.0 protocol                                         │
│                                                                                         │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

### 语言支持

| 语言 | 解析器 | 状态 |
|------|--------|------|
| Python | tree-sitter-python | ✅ Working |
| Rust | tree-sitter-rust | ✅ Working |
| JavaScript | tree-sitter-javascript | ✅ Working |
| TypeScript | tree-sitter-typescript | ✅ Working |
| Go | tree-sitter-go | ✅ Working |
| Java | tree-sitter-java | ✅ Working |
| C++ | tree-sitter-cpp | ✅ Working |
| C# | tree-sitter-c-sharp | ✅ Working |
| Ruby | tree-sitter-ruby | ✅ Working |
| PHP | tree-sitter-php | ✅ Working |
| Lua | tree-sitter-lua | ✅ Working |
| Scala | tree-sitter-scala | ✅ Working |

---

## 快速开始

### 前置要求

- **Rust 1.75+**（通过 [rustup.rs](https://rustup.rs/) 安装）
- **Cargo**（随 Rust 一起安装）

### 安装（推荐并列方式）

#### 一行安装脚本（Linux/macOS）

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

#### Cargo（crates.io）

```bash
cargo install leindex
```

#### Cargo（Git 源码）

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

#### 从源码构建

```bash
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
cargo build --release --bins
```

二进制路径：`target/release/leindex`

### 验证

```bash
leindex --version
# Output: LeIndex 0.1.0
```

### 基本使用

```bash
# 建立索引
leindex index /path/to/project

# 语义搜索
leindex search "authentication logic"

# 深度分析 + 上下文扩展
leindex analyze "how does the database connection work"

# 系统诊断
leindex diagnostics

# 启动 MCP Server
leindex serve --host 127.0.0.1 --port 3000
```

---

## MCP Server 集成

### 启动服务

```bash
leindex serve --host 127.0.0.1 --port 3000
```

提供接口：
- `POST /mcp`（JSON-RPC 2.0）
- `GET /mcp/tools/list`
- `GET /health`

### MCP 客户端配置示例

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["serve", "--host", "127.0.0.1", "--port", "3000"],
      "env": {}
    }
  }
}
```

### MCP 工具

| 工具 | 说明 |
|------|------|
| `deep_analyze` | 深度代码分析与上下文扩展 |
| `search` | 语义搜索 |
| `index` | 项目索引 |
| `context` | 基于重力遍历的上下文扩展 |
| `diagnostics` | 系统健康检查 |

---

## 缓存与内存管理

### 预热策略

- **All**：同时预热 PDG 与向量缓存
- **PDGOnly**：仅预热 PDG
- **SearchIndexOnly**：仅预热向量检索缓存
- **RecentFirst**：优先预热最近访问数据

### 内存机制

- RSS 监控（阈值 85%）
- 超阈值自动溢写缓存
- 从存储重载缓存
- 可配置预热策略

---

## 性能

### 基准（v0.1.0）

| 指标 | 目标 | 状态 |
|------|------|------|
| **索引速度** | 50K 文件 <60s | ✅ Achieved |
| **搜索延迟（P95）** | <100ms | ✅ Achieved |
| **内存优化** | 10x（400→32 bytes/node） | ✅ Achieved |
| **Token 效率** | 20% improvement | ✅ Achieved |

### 代码质量

| 指标 | 数值 |
|------|------|
| **测试** | 339/339 通过（100%） |
| **警告** | 0 clippy warnings |
| **文档** | 完整 rustdoc |
| **审查** | lerecherche 通过 Tzar 审查（18 项问题修复） |

---

## 技术栈

| 组件 | 技术 | 用途 |
|------|------|------|
| Parsing | tree-sitter | 零拷贝 AST 提取（12 语言） |
| Graph | petgraph | StableGraph PDG 构建 |
| Traversal | Custom | 重力遍历 + 优先队列 |
| Vector Search | hnsw-rs | HNSW ANN 检索 |
| NL Queries | Custom | 意图分类与模式匹配 |
| CLI | clap | 命令行解析 |
| MCP Server | axum | JSON-RPC 2.0 over HTTP |
| Async | tokio | 异步运行时 |
| Logging | tracing | 结构化日志 |
| Serialization | serde/bincode | 高效序列化 |
| Storage | SQLite | 本地持久化（WAL） |
| Hashing | BLAKE3 | 增量缓存哈希 |

---

## 文档

- [Installation Guide](INSTALLATION.md)
- [Architecture](ARCHITECTURE.md)
- [Migration Guide](docs/MIGRATION.md)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)

---

## 路线图

### 已完成 ✅

- [x] 12 语言零拷贝 AST 提取
- [x] PDG 构建 + 重力遍历
- [x] HNSW 语义检索
- [x] 自然语言查询
- [x] 跨项目符号解析
- [x] 纯 Rust CLI（5 个核心命令）
- [x] JSON-RPC 2.0 MCP 服务
- [x] 缓存管理（spill/reload/warm）
- [x] 339/339 测试通过

### v0.2.0（计划）

- [ ] 项目配置（TOML/JSON）
- [ ] 更细粒度错误恢复
- [ ] 完整性能基准套件
- [ ] 用户文档扩展

### v0.3.0（未来）

- [ ] 可选 Turso 远程数据库集成
- [ ] 更多语言解析器
- [ ] Web UI 代码探索

---

## 参与贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md)。

重点欢迎：
- 新语言解析器
- 性能优化
- 文档改进
- Bug 修复

---

## 许可证

MIT OR Apache-2.0（见 [LICENSE](LICENSE)）

---

## 致谢

LeIndex 基于以下优秀开源项目：

- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/)
- [petgraph](https://github.com/petgraph/petgraph)
- [hnsw-rs](https://github.com/jeromefroe/hnsw)
- [axum](https://github.com/tokio-rs/axum)
- [Model Context Protocol](https://modelcontextprotocol.io)

---

## 支持

- **GitHub Issues:** https://github.com/scooter-lacroix/leindex/issues
- **项目主页:** https://github.com/scooter-lacroix/leindex

---

<div align="center">

**Built with ❤️ and Rust for developers who love their code**

*⭐ 如果它对你有帮助，欢迎点 Star！*

</div>
