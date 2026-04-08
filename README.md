# feishu2acp

一个本地常驻的 Rust 桥接服务：

- 飞书机器人通过长连接接收消息
- 普通文本消息自动路由到 `acpx -> codex`
- 飞书 `markdown` 文件消息会作为结构化文档块送进当前会话
- `/...` 用来控制当前会话上下文
- `acpx` 负责把消息转成 ACP/ACPX 调用并复用本地长会话
- 飞书和 `acpx/codex` 都通过 trait 解耦，便于 mock 和单测

## 架构

核心分层：

- `src/application`
  - 命令解析
  - 回复渲染
  - 业务服务 `BridgeService`
- `src/ports`
  - `ChannelClient`
  - `ChannelRuntime`
  - `AcpxGateway`
  - `ShellExecutor`
  - `ConversationRepository`
  - `ProcessRunner`
- `src/adapters`
  - `feishu.rs`: 飞书长连接接入与消息回复
  - `acpx.rs`: acpx CLI 适配器
  - `shell.rs`: 本地 shell 执行器
  - `repository.rs`: 内存/文件状态存储
  - `process.rs`: 外部进程运行器

## 功能

- 普通文本消息自动进入当前 Codex session
- 飞书 `.md/.markdown` 文件会下载到本地媒体目录，并以文档块形式送入当前 session
- agent 可以通过 `<feishu2acp-document file_name="xxx.md">...</feishu2acp-document>` 返回 markdown 文件，桥接层会回飞书 `file` 消息
  - 由于飞书 IM 文件接口不接受 `.md` 扩展名，回传时会使用 `.md.txt` 作为传输文件名，桥接层在入站时会还原成逻辑上的 `*.md`
- `/` 显示帮助
- `/cd <dir>` 切换工作目录
- `/pwd` 查看当前上下文
- `/agent <name>` 切换当前 agent
- `/permissions <approve-all|approve-reads|deny-all>`
- `/session new [name]`
- `/session use [name|default]`
- `/session show [name]`
- `/session close [name]`
- `/session list`
- `/session history [limit]`
- `/status`
- `/mode <mode>`
- `/model <model>`
- `/set <key> <value>`
- `/exec <text>` 一次性任务
- `/shell <command>` 本地 shell 命令
- `/cancel`

## 环境变量

必填：

- `FEISHU_APP_ID`
- `FEISHU_APP_SECRET`

常用可选项：

- `FEISHU2ACP_PERMISSION_MODE`，默认 `approve-reads`
- `FEISHU2ACP_STATE_PATH`
- `FEISHU2ACP_REPLY_CHUNK_CHARS`
- `FEISHU2ACP_MEDIA_DIR`
- `FEISHU2ACP_MAX_MARKDOWN_BYTES`，默认 `1048576`
- `FEISHU2ACP_ENABLE_MARKDOWN_INPUT`，默认 `true`
- `FEISHU2ACP_ENABLE_MARKDOWN_OUTPUT`，默认 `true`

固定行为：

- 默认工作目录 = 服务启动时的当前目录
- 默认 agent = `codex`
- 命令前缀 = `/`

acpx：

- `ACPX_PROGRAM`，默认 `acpx`
- `ACPX_PROGRAM_ARGS`
- `ACPX_TIMEOUT_SECS`
- `ACPX_TTL_SECS`

如果你没有全局安装 `acpx`，可直接这样配：

```env
ACPX_PROGRAM=npx
ACPX_PROGRAM_ARGS=["acpx@latest"]
```

shell 根据操作系统自动选择：

Windows：

```text
powershell -NoProfile -Command
```

Linux/macOS：

```text
sh -lc
```

## 运行

先确保：

1. 飞书应用已开启机器人消息事件
2. 本机可用 `acpx codex ...`
3. Codex CLI 和对应 ACP 适配器可正常工作

启动：

```bash
cargo run
```

向指定飞书群发送 markdown 测试文件：

```bash
cargo run --bin send_markdown_file -- --file README.md --note "请总结这个文档"
```

如果不传参数，脚本会给默认群 `oc_f5f9c8e4001155b3d3fd395426388ce4` 发送一份内置 smoke-test markdown。

## 测试

```bash
cargo test
```

当前单测覆盖：

- 命令解析
- 回复渲染
- 核心服务行为
- acpx CLI 命令构造与输出解析
- 文件/内存状态存储
- 飞书入站事件翻译
- shell 适配器
- markdown 文档块收发与回复协议解析

## 备注

- 仓库内 vendored 了一份 `lark-websocket-protobuf`，并改成自带 `protoc`，确保本地可直接编译
- 飞书 SDK 和 `acpx` 均只存在于边缘适配层，核心逻辑可完全通过 mock 测试
