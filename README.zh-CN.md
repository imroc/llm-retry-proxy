# LLM Retry Proxy

[English](./README.md)

一个透明的本地反向代理，为任意 OpenAI 兼容的 LLM API 提供无限自动重试能力。专为团队共用 API 限速场景设计，解决 429 限速导致 AI CLI 工具会话频繁中断的问题。

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
- **Prometheus 指标** — 监控重试频率和上游状态
- **单二进制** — 低内存占用，Rust 实现

## 快速开始

```bash
# 编译
make build

# 创建配置
cp config.example.toml config.toml
# 编辑 config.toml，配置你的 API 地址

# 运行
./target/release/llm-retry-proxy --config config.toml --log-level info
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

## 安装

```bash
make install
```

交互式安装二进制文件，可选配置 systemd/launchd 服务自启。

## 许可证

[MIT](./LICENSE)
