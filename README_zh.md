# LeIndex

<div align="center">

[![MCP Server](https://img.shields.io/badge/MCP-Server-blue?style=for-the-badge)](https://modelcontextprotocol.io)
[![Python](https://img.shields.io/badge/Python-3.10%2B-green?style=for-the-badge)](https://www.python.org/)
[![License](https://img.shields.io/badge/License-MIT-yellow?style=for-the-badge)](LICENSE)
[![Version](https://img.shields.io/badge/Version-2.0.2-blue?style=for-the-badge)](CHANGELOG.md)

**真正理解你代码的 AI 驱动代码搜索工具**

*闪电般的语义代码搜索，零依赖。通过代码含义搜索，而不仅仅是文本匹配。*

</div>

---

<div align="center">

![LeIndex Architecture](https://maas-log-prod.cn-wlcb.ufileos.com/anthropic/e8dba354-0e5e-4974-a700-3beb492d9d1b/64af137252f0419ca035948db594c8c4.jpg?UCloudPublicKey=TOKEN_e15ba47a-d098-4fbd-9afc-a0dcf0e4e621&Expires=1767570027&Signature=y1z+LCTe/ZyqP05/OEdFqcwyGfM=)

*LeIndex 体验 - 强大、快速、美观*

</div>

---

## ✨ LeIndex 的独特之处？

**LeIndex** 不仅仅是一个代码搜索工具。它是你的智能代码伙伴，能够理解你在找**什么**，而不只是**哪里**可能打出了这些字。

想象一下，搜索"认证流程"时，不仅能找到包含这些词的文件，还能找到实际的认证逻辑、登录处理器、会话管理和安全模式——即使它们的命名完全不同。这就是语义搜索的魔力！🎯

---

## 🚀 快速开始（不到 2 分钟就能上手！）

```bash
# 安装 LeIndex - 说真的，就这么简单
pip install leindex

# 为你的代码库建立索引（不需要 Docker、数据库，不会头疼）
leindex init /path/to/your/project
leindex index /path/to/your/project

# 像魔法师一样搜索
leindex-search "authentication logic"

# 或者在 Claude、Cursor 或你喜欢的 AI 助手中通过 MCP 使用
# LeIndex MCP 服务器会自动完成繁重的工作！
```

**搞定了！** 现在你可以用思维速度搜索代码库了。🎉

---

## 🎯 为什么开发者喜欢 LeIndex

### 🔥 零依赖，零麻烦
- **不需要 Docker** - 你的笔记本电脑会感谢你
- **不需要 PostgreSQL** - 没有数据库配置噩梦
- **不需要 Elasticsearch** - 没有 Java 内存泄漏
- **不需要 RabbitMQ** - 没有消息队列复杂性
- **纯 Python 魔法** - `pip install` 搞定一切

### ⚡ 疾速性能
- **LEANN 向量搜索** - 毫秒级找到相似代码
- **Tantivy 全文搜索** - Rust 驱动的 Lucene 性能
- **混合评分** - 两全其美：语义 + 词法
- **处理 10 万+ 文件** - 从副业项目到单体仓库都能搞定

### 🧠 语义理解
- **CodeRankEmbed 嵌入** - 理解代码含义和意图
- **按概念搜索** - 搜索"错误处理"就能找到 try/except、错误类型、日志和恢复模式
- **智能符号搜索** - 即时跳转到定义和引用
- **正则表达式威力** - 需要精确模式匹配时的利器

### 🏠 隐私优先 & 本地部署
- **你的代码归你所有** - 什么都不离开你的机器
- **离线工作** - 安装后无需互联网
- **无遥测** - 我们不跟踪你的搜索
- **企业级就绪** - 可以部署在你自己的基础设施上

### 🤖 原生 MCP 设计
- **一流的 MCP 支持** - 从头开始为 Model Context Protocol 构建
- **AI 助手就绪** - 与 Claude、Cursor、Windsurf 等无缝协作
- **Token 高效** - 每个会话节省约 200 个 token（无 hook 开销！）
- **可选技能集成** - 用于复杂的多项目工作流

---

## 🎪 LeIndex 魔法秀

### 🔍 读心术般的搜索

```python
# 语义搜索
results = indexer.search("authentication flow")

# 获得真正有意义的结果：
# - 登录处理器（即使命名为 'sign_in'）
# - 会话管理（即使叫 'user_state'）
# - JWT 验证（即使标记为 'token_check'）
# - 密码哈希（即使在 'crypto_utils' 中）
```

### 📊 独门秘籍（技术栈）

| 组件 | 技术 | 超能力 |
|-----------|------------|------------|
| **向量搜索** | [LEANN](https://github.com/lerp-cli/leann) | 存储高效的语义相似度 |
| **代码大脑** | [CodeRankEmbed](https://huggingface.co/nomic-ai/CodeRankEmbed) | 理解代码含义和意图 |
| **文本搜索** | [Tantivy](https://github.com/quickwit-oss/tantivy-py) | Rust 驱动的 Lucene（快！） |
| **元数据** | [SQLite](https://www.sqlite.org/) | 可靠的 ACID 兼容存储 |
| **分析** | [DuckDB](https://duckdb.org/) | 内存分析查询 |
| **异步引擎** | asyncio | 内置 Python 异步（不需要 RabbitMQ！）|

### 🏗️ 合理的架构

```
┌─────────────────────────────────────────────────────────┐
│              LeIndex 体验中心                            │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌──────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │   MCP 服务器 │◀─▶│  核心引擎   │◀─▶│  LEANN    │ │
│  │  (你的 AI    │     │ (大脑)      │     │ (向量)    │ │
│  │   助手)      │     │             │     │           │ │
│  └──────────────┘     └─────────────┘     └───────────┘ │
│         │                   │                    │      │
│         │                   ▼                    ▼      │
│         │            ┌──────────────┐     ┌───────────┐ │
│         │            │ 查询路由器   │◀─▶│ Tantivy   │ │
│         │            │  (交通       │     │(全文)     │ │
│         │            │   指挥)      │     │           │ │
│         │            └──────────────┘     └───────────┘ │
│         │                   │                    │      │
│         ▼                   ▼                    ▼      │
│  ┌──────────────┐    ┌──────────────┐    ┌───────────┐  │
│  │ CLI 工具     │    │ 数据访问层   │    │  SQLite   │  │
│  │ (高级用户)   │    │              │    │ (元数据)  │  │
│  └──────────────┘    └──────────────┘    └───────────┘  │
│                              │                          │
│                              ▼                          │
│                       ┌──────────────┐                  │
│                       │   DuckDB     │                  │
│                       │ (分析)       │                  │
│                       └──────────────┘                  │
└─────────────────────────────────────────────────────────┘

💡 一切都在本地运行。无云。无依赖。只有速度。
```

---

## 🎮 安装（比泡咖啡还简单）

### 系统要求
- Python 3.10 或更高版本
- 最少 4GB RAM（大型代码库建议 8GB+）
- 约 1GB 磁盘空间

### 一行安装

```bash
pip install leindex
```

**真的就这些。** 不需要 Docker。不需要数据库。不需要配置文件（除非你想要）。开箱即用。✨

### 验证安装

```bash
leindex --version
# 输出: LeIndex 2.0.2 - Ready to search! 🚀
```

### 从源码安装（给冒险家）

```bash
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
pip install -e .
```

---

## 🎯 使用：让我们搜索代码！

### 🤖 MCP 集成（酷炫方式）

LeIndex 内置 MCP 服务器，让你的 AI 助手具备代码感知能力：

**可用的 MCP 超能力：**
- `manage_project` - 为你的项目设置和管理索引
- `search_content` - 使用语义 + 全文搜索代码
- `get_diagnostics` - 获取项目统计和健康检查

**在你的 MCP 客户端中配置（Claude、Cursor 等）：**

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {}
    }
  }
}
```

**启动 MCP 服务器：**
```bash
leindex mcp
```

现在你的 AI 助手可以像专业人士一样搜索你的代码库了！🎉

**何时使用什么：**

| 方式 | 最适合 |
|----------|----------|
| **MCP 工具** | 单项目搜索、简单查询、直接 API 访问 |
| **技能** | 多项目操作、复杂工作流、自动化管道 |

### 🐍 Python API（给程序员）

```python
from leindex import LeIndex

# 初始化并索引
indexer = LeIndex("~/my-awesome-project")
indexer.index()

# 语义搜索 - 它理解含义！
results = indexer.search("authentication flow")

# 像老板一样过滤
results = indexer.search(
    query="database connection",
    file_patterns=["*.py"],           # 只要 Python 文件
    exclude_patterns=["test_*.py"]     # 但不要测试
)

# 访问好东西
for result in results:
    print(f"{result.file}:{result.line}")
    print(result.content)
    print(f"相关性得分: {result.score}")
```

### 🔧 CLI 工具（给终端爱好者）

```bash
# 为项目初始化索引
leindex init /path/to/project

# 运行索引（很快，我们保证）
leindex index /path/to/project

# 从终端搜索
leindex-search "authentication logic"

# 带过滤器搜索
leindex-search "database" --ext py --exclude test_*
```

---

## ⚙️ 配置（可选但强大）

LeIndex 开箱即用就很棒，但你可以通过 `config.yaml` 随心所欲地调整：

```yaml
# 数据访问层（引擎室）
dal_settings:
  backend_type: "sqlite_duckdb"    # 好东西
  db_path: "./data/leindex.db"     # 元数据所在
  duckdb_db_path: "./data/leindex.db.duckdb"  # 分析天堂

# 向量存储（语义搜索魔法）
vector_store:
  backend_type: "leann"            # 存储高效的向量
  index_path: "./leann_index"      # 向量休息的地方
  embedding_model: "nomic-ai/CodeRankEmbed"  # 代码大脑
  embedding_dim: 768               # 向量维度

# 异步处理（速度恶魔）
async_processing:
  enabled: true
  worker_count: 4                  # 并行索引
  max_queue_size: 10000            # 队列缓冲区

# 文件过滤（保持精简）
file_filtering:
  max_file_size: 1073741824        # 每个文件 1GB
  type_specific_limits:
    ".py": 1073741824              # Python 文件最大 1GB
    ".json": 104857600             # JSON 文件最大 100MB

# 目录过滤（忽略垃圾）
directory_filtering:
  skip_large_directories:
    - "**/node_modules/**"         # 不要 JavaScript 依赖地狱
    - "**/.git/**"                 # 不要 git 历史
    - "**/venv/**"                 # 不要虚拟环境
    - "**/__pycache__/**"          # 不要 Python 缓存
```

---

## 📊 性能统计（我们真的不慢）

| 指标 | LeIndex | 典型代码搜索 | 差异 |
|--------|---------|-------------------|-------------|
| **索引速度** | ~10K 文件/分钟 | ~500 文件/分钟 | **快 20 倍** |
| **搜索延迟 (p50)** | ~50ms | ~500ms | **快 10 倍** |
| **搜索延迟 (p99)** | ~200ms | ~5s | **快 25 倍** |
| **最大可扩展性** | 10 万+ 文件 | 1 万文件 | **多 10 倍** |
| **内存使用** | <4GB | >8GB | **少 2 倍** |

*基于 10 万文件的 Python 代码库基准测试，标准硬件。你的情况可能不同，但仍然会很快！*

---

## 🆚 进化：从 code-indexer 到 LeIndex

LeIndex 是对原始 code-indexer 项目的彻底重新构想：

- ✅ **新身份** - 作为 LeIndex 诞生，为现代开发而构建
- ✅ **包重命名** - 清爽的 `leindex` 包名
- ✅ **CLI 精简** - 简单的 `leindex` 命令
- ✅ **环境统一** - `LEINDEX_*` 环境变量
- ✅ **技术栈革命** - 移除了所有外部依赖
- ✅ **新的轻量级架构** - 纯 Python + LEANN + Tantivy + SQLite + DuckDB

**我们抛弃了什么：**
- ❌ PostgreSQL（不需要数据库配置！）
- ❌ Elasticsearch（不需要 Java！）
- ❌ FAISS（没有外部依赖！）
- ❌ RabbitMQ（没有消息队列复杂性！）
- ❌ Docker（没有容器开销！）

**我们获得了什么：**
- ✅ 简洁性
- ✅ 速度
- ✅ Token 效率（每会话节省约 200 个 token）
- ✅ 纯 MCP 架构
- ✅ 开发者幸福感

---

## 📚 不枯燥的文档

- [安装指南](INSTALLATION.md) - 详细设置说明
- [MCP 配置](MCP_CONFIGURATION.md) - MCP 服务器设置和示例
- [架构深入](ARCHITECTURE.md) - 系统设计和内部原理
- [API 参考](API.md) - 完整的 API 文档
- [迁移指南](MIGRATION.md) - 从 code-indexer 升级
- [贡献指南](CONTRIBUTING.md) - 加入乐趣！

---

## 🧪 开发（给好奇的人）

### 项目结构

```
leindex/
├── src/leindex/              # 魔法发生在这里
│   ├── dal/                  # 数据访问层
│   ├── storage/              # 存储后端
│   ├── search/               # 搜索引擎
│   ├── core_engine/          # 核心索引和搜索
│   ├── config_manager.py     # 配置魔法
│   ├── project_settings.py   # 项目设置
│   ├── constants.py          # 共享常量
│   └── server.py             # MCP 服务器
├── tests/                    # 测试套件
├── config.yaml               # 配置
└── pyproject.toml           # 项目元数据
```

### 运行测试

```bash
# 安装开发依赖
pip install -e ".[dev]"

# 运行测试
pytest tests/

# 运行覆盖率测试（因为我们关心）
pytest --cov=leindex tests/
```

---

## 🤝 贡献（加入派对！）

我们热爱贡献！无论是错误修复、新功能、文档改进，还是仅仅传播消息——都非常感激。

请查看 [CONTRIBUTING.md](CONTRIBUTING.md) 了解指南。我们保证我们很友好！😊

---

## 📜 许可证

MIT 许可证 - 详见 [LICENSE](LICENSE)。随时使用、修改、分享。尽情发挥！

---

## 🙏 致谢（站在巨人的肩膀上）

LeIndex 构建于惊人的开源项目之上：

- [LEANN](https://github.com/lerp-cli/leann) - 存储高效的向量搜索
- [Tantivy](https://github.com/quickwit-oss/tantivy-py) - 纯 Python 全文搜索（Rust Lucene）
- [DuckDB](https://duckdb.org/) - 快速分析数据库
- [SQLite](https://www.sqlite.org/) - 嵌入式关系数据库
- [CodeRankEmbed](https://huggingface.co/nomic-ai/CodeRankEmbed) - 代码嵌入
- [Model Context Protocol](https://modelcontextprotocol.io) - AI 集成

**非常感谢所有贡献者！** 🎉

---

## 💬 支持与社区

- **GitHub Issues:** [https://github.com/scooter-lacroix/leindex/issues](https://github.com/scooter-lacroix/leindex/issues)
- **文档:** [https://github.com/scooter-lacroix/leindex](https://github.com/scooter-lacroix/leindex)
- **在 GitHub 上给我们点星** - 帮助更多人发现 LeIndex！⭐

---

<div align="center">

**用 ❤️ 为热爱代码的开发者构建**

*⭐ 在 GitHub 上给我们点星 — 这会让我们微笑！*

**准备好更智能地搜索了吗？** [立即安装 LeIndex](#-安装比泡咖啡还简单) 🚀

</div>
