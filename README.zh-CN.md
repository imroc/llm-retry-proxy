# LLM Proxy

[English](./README.md)

一个轻量的本地 LLM API 代理，提供自动重试、协议转换和模型级路由能力。专为团队共用 API 限速场景设计，解决 429 限速导致 AI CLI 工具会话频繁中断的问题。

## 为什么需要？

使用 AI CLI 工具（CodeBuddy Code、Claude Code、Codex CLI、OpenCode）连接团队共用的模型 API 时，全局限速会导致 429 错误中断会话。大多数 CLI 工具的重试能力有限或缺失，需要手动回复"继续"才能恢复工作。

本代理位于 CLI 工具和 API 之间，对收到 429/5xx 错误的请求自动进行指数退避+抖动重试，CLI 工具完全感知不到错误。

## 特性

- **无限重试** — 持续重试直到客户端断开或请求成功
- **协议无关** — 兼容 OpenAI、Anthropic Messages 及任何 HTTP API 格式
- **透明无感** — CLI 工具无需支持重试，只需指向代理地址
- **流式支持** — SSE 流式透传，不缓冲完整响应
- **客户端感知** — 即时检测客户端断开（包括退避等待期间），立即停止重试
- **配置热加载** — 增删路由无需重启
- **模型级路由** — 在单个路由内将不同模型路由到不同上游，支持 Codex 等工具的单 provider 多模型场景
- **Prometheus 指标** — 按路由 + 模型维度监控重试频率和上游状态
- **单二进制** — 低内存占用，Rust 实现

## 快速开始

```bash
# 编译
make build

# 创建配置
cp config.example.toml config.toml
# 编辑 config.toml，配置你的 API 地址

# 运行
./target/release/llm-proxy --config config.toml --log-level info
```

将 AI CLI 工具的 API 地址指向 `http://127.0.0.1:8888/{路由名}/{API路径}`。

各 AI CLI 工具的配置指南见 [docs/tools/](./docs/tools.zh-CN.md)。

## 配置

```toml
[defaults]
max_retries = 9999           # 实际上无限
base_delay_ms = 1000         # 指数退避基数
max_delay_ms = 60000         # 退避上限
max_total_wait_ms = 0        # 0 = 依赖客户端断开
connect_timeout_secs = 30
retry_status_codes = [429, 500, 502, 503, 504, 408, 529]

[routes.myapi]
target = "https://api.example.com"
# 路由级覆盖（均可选）：
# max_retries = 500
# base_delay_ms = 2000
# max_delay_ms = 30000
```

完整示例见 [config.example.toml](./config.example.toml)。

### 模型级路由

当请求体包含 `model` 字段时，代理会在路由的 `models` 映射表中查找。如果命中，模型级配置覆盖路由级配置。这使得单个路由（provider）可以管理来自不同上游的多个模型 —— 非常适合 Codex 等只能在同一 provider 内切换模型的 AI CLI 工具。

```toml
[routes.tkehub]
target = "http://tkehub.woa.com"
transform = "responses_to_chat"  # 路由级默认

# GLM 原生支持 Responses API，直连无需转换
[routes.tkehub.models."tke/glm-latest"]
transform = "none"

# DeepSeek 走不同上游，需要模型名映射
[routes.tkehub.models."tke/deepseek-flash-latest"]
target = "https://tokenhub.tencentmaas.com"
upstream_model = "deepseek-chat"
rewrite_response_model = true
max_delay_ms = 30000
```

模型级字段（全部可选，仅指定的字段覆盖路由级配置）：

| 字段 | 说明 |
|------|------|
| `target` | 上游 API 地址 |
| `transform` | `"responses_to_chat"` 或 `"none"` 显式禁用 |
| `upstream_model` | 改写请求体中的 `model` 字段 |
| `rewrite_response_model` | 将响应中的 `model` 字段回写为客户端原始模型名（默认 false） |
| 重试参数 | `max_retries`、`base_delay_ms`、`max_delay_ms`、`max_total_wait_ms`、`connect_timeout_secs`、`retry_status_codes` |

## 安装

```bash
make install
```

交互式安装二进制文件，可选配置 systemd/launchd 服务自启。

## 许可证

[MIT](./LICENSE)
