# LeIndex（中文说明）

LeIndex 是一个 Rust 代码智能系统，核心能力包括：
- 索引（index）
- 图分析（PDG）
- 语义搜索（search）
- 深度分析（analyze）
- MCP 集成（给 AI 助手调用）
- 可选的 5 阶段分析模式（phase）

> 说明：5-phase 是 LeIndex 的一个“分析模式”，不是全部功能。

---

## 快速安装

### 推荐安装脚本

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

### Cargo 安装（当前可用）

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

---

## 基本使用

```bash
# 1) 建立索引
leindex index /path/to/project

# 2) 语义搜索
leindex search "认证逻辑在哪里处理"

# 3) 深度分析
leindex analyze "登录会话失效流程"

# 4) 可选：5 阶段分析（范围收敛）
leindex phase --all --path /path/to/project
```

---

## MCP 模式

```bash
leindex mcp
# 或
leindex serve --host 127.0.0.1 --port 47268
```

常用 MCP 工具：
- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`
- `leindex_phase_analysis`（别名：`phase_analysis`）

---

## 推荐工作流

1. 先 `index` 建立索引。
2. 用 `search` / `analyze` 定位核心代码。
3. 需要快速全局梳理时再用 `phase`。
4. 对关键文件进行人工阅读，做最终判断。

---

更多文档：
- `ARCHITECTURE.md`
- `RUST_ARCHITECTURE.md`
- `API.md`
- `INSTALLATION.md`
