# 飞书 <-> ACP 双向图片能力设计

> 2026-04-08 更新：仓库当前已经按“长期版块模型”补齐 `markdown` 文档传输能力。
> 图片能力的核心抽象仍然沿用本文方案，后续可继续在同一块模型上扩展。

## 背景

当前项目已经支持：

- 飞书文本消息进入当前 agent session
- `acpx -> codex` 的持续会话复用
- agent 文本结果回复到飞书

当前项目还不支持：

- 用户在飞书发送图片给 agent
- agent 返回图片到飞书
- 文本 + 图片的结构化多模态消息

这份文档描述未来要补齐的“双向图片发送”能力。

## 目标

### 用户侧体验

目标交互包括：

1. 用户在飞书发送一张图片，bot 将图片连同上下文送给当前 agent。
2. 用户在飞书发送图片后，再补一句文本说明，agent 能拿到图片和说明。
3. agent 在回答中返回图片时，bot 能把图片发回飞书。
4. 一次回复中如果同时包含文本和图片，飞书侧至少能按顺序完整收到。

### 系统侧目标

- 保持现有会话绑定模型不变，继续按飞书会话复用 agent session。
- 不把图片能力写死到 `codex`，仍然通过 port / adapter 抽象解耦。
- 当底层 agent 或模型不支持图片输入时，能够明确降级。
- 当飞书或 agent 的图片格式不满足要求时，能够返回可理解错误。

## 非目标

本阶段不要求：

- 视频、音频、文件等非图片富媒体
- 卡片式 UI 交互
- 流式图片增量更新
- OCR、标注、压缩增强等额外媒体处理能力

## 当前缺口

基于现有代码，图片链路的主要缺口如下：

- [src/adapters/feishu.rs](/home/pc/proj/self-projs/feishu2acp/src/adapters/feishu.rs) 当前只接收 `text` 消息，非文本会直接忽略。
- [src/domain/mod.rs](/home/pc/proj/self-projs/feishu2acp/src/domain/mod.rs) 的 `InboundMessage` 只有纯文本字段，没有结构化内容块。
- [src/ports/mod.rs](/home/pc/proj/self-projs/feishu2acp/src/ports/mod.rs) 的 `ChannelClient` 只有 `send_text`，`AcpxGateway` 的 `prompt/exec` 也只接受字符串。
- [src/adapters/acpx.rs](/home/pc/proj/self-projs/feishu2acp/src/adapters/acpx.rs) 当前只把 prompt 当作纯文本传给 `acpx`。
- [src/application/service.rs](/home/pc/proj/self-projs/feishu2acp/src/application/service.rs) 的业务流只处理文本 prompt 和文本 reply。

## 协议假设

设计假设如下：

- ACP 协议层支持结构化内容块，并支持 `image` 类型内容。
- 图片能力不是所有 agent 的默认能力，能力协商必须保留。
- `acpx` 是否已经完整支持“结构化图片输入/输出”，实现前需要再次确认。

如果 `acpx` 当前只支持文本 CLI 入口，则需要二选一：

1. 扩展 `acpx` 的结构化输入输出能力。
2. 在本项目中新增一个直接面向 ACP 的 adapter，而不是继续只走纯文本 CLI。

## 目标数据模型

建议在 domain 层引入统一消息块模型。

```rust
pub enum MessageBlock {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        source: ImageSource,
        alt: Option<String>,
    },
}

pub enum ImageSource {
    Bytes(Vec<u8>),
    Base64(String),
    Uri(String),
    LocalPath(PathBuf),
}

pub struct InboundMessage {
    pub conversation: ConversationKey,
    pub reply_target: ReplyTarget,
    pub blocks: Vec<MessageBlock>,
}

pub struct AgentReply {
    pub blocks: Vec<MessageBlock>,
}
```

设计原则：

- 文本和图片都使用同一套块模型，避免文本链路和图片链路分叉。
- `alt` 文本保留给飞书展示和无障碍兜底。
- `ImageSource` 不强制单一存储形式，方便后续兼容 Feishu 下载结果、ACP base64 内容、agent 输出本地文件路径等来源。

## 端口改造

建议端口层改成“消息块”而不是“纯文本”。

### ChannelClient

从：

```rust
async fn send_text(&self, target: &ReplyTarget, text: &str) -> Result<(), BridgeError>;
```

改为类似：

```rust
async fn send_message(
    &self,
    target: &ReplyTarget,
    blocks: &[MessageBlock],
) -> Result<(), BridgeError>;
```

### AcpxGateway

从：

```rust
async fn prompt(&self, selector: &SessionSelector, prompt: &str) -> Result<PromptResponse, BridgeError>;
```

改为类似：

```rust
async fn prompt(
    &self,
    selector: &SessionSelector,
    blocks: &[MessageBlock],
) -> Result<AgentReply, BridgeError>;
```

如果保留文本快捷接口，也应由结构化接口向下兼容，而不是相反。

## 飞书 -> agent 方向

### 最小可行链路

1. 飞书收到图片消息。
2. `feishu.rs` 将消息翻译为 `InboundMessage { blocks: [Image] }`。
3. 应用层把图片块转交给 `AcpxGateway`。
4. ACP / agent 返回文本或图片结果。
5. bot 将结果回发飞书。

### 图片获取

实现阶段需要支持：

- 根据飞书消息内容拿到图片资源标识
- 下载原始字节
- 识别或补齐 `mime_type`
- 做大小和格式校验

建议限制：

- 首期仅允许 `image/png`、`image/jpeg`、`image/webp`
- 首期拒绝 `svg`
- 单张图片大小设置硬限制，避免进程内存被打爆

### 文本 + 图片组合

飞书未必天然把“文本 + 图片”作为同一个消息对象提供，因此建议分阶段做：

- Phase 1: 支持“纯图片消息”或“纯文本消息”
- Phase 2: 支持同一线程下短时间窗口内的图片 + 文本合并

合并策略建议：

- 只在同一 `ConversationKey` 内合并
- 只合并连续、未被 bot 消费的用户消息
- 使用很短的窗口，例如 2 到 5 秒
- 超出窗口则按独立消息处理

如果不做自动合并，也可以定义显式命令：

- `/image` 上传后一条文本说明
- `/attach last` 使用上一张图作为本次 prompt 附件

## agent -> 飞书 方向

### 最小可行链路

1. `AcpxGateway` 返回结构化内容块。
2. 应用层按块顺序渲染。
3. 文本块发文本消息，图片块上传飞书后发图片消息。

### 图片发送策略

建议支持以下来源：

- ACP 响应直接给出 base64 图片
- ACP 响应给出本地文件路径
- ACP 响应给出可读取 URI

飞书发送时统一转换成：

1. 读取图片字节
2. 上传到飞书媒体接口
3. 再发送图片消息

### 文本和图片混排

首期不要追求复杂卡片，直接按顺序逐条发送：

- `Text`
- `Image`
- `Text`

好处是：

- 与当前 `reply chunk` 机制兼容
- 失败定位简单
- 不依赖飞书卡片模板

后续如需要更好的呈现，可以再补卡片消息渲染层。

## 应用层改造

[src/application/service.rs](/home/pc/proj/self-projs/feishu2acp/src/application/service.rs) 需要从“文本请求 -> 文本响应”改为“块请求 -> 块响应”。

建议新增：

- `handle_blocks(...)`
- `render_reply_blocks(...)`
- 文本命令仍然保留现有逻辑

处理原则：

- 命令消息继续只看文本首块
- 普通消息走多模态 prompt
- 如果请求里有图片但当前 agent 不支持图片，直接返回明确错误

## 配置项

建议新增以下配置：

- `FEISHU2ACP_MEDIA_DIR`
- `FEISHU2ACP_MAX_IMAGE_BYTES`
- `FEISHU2ACP_ALLOWED_IMAGE_MIME_TYPES`
- `FEISHU2ACP_ENABLE_IMAGE_INPUT`
- `FEISHU2ACP_ENABLE_IMAGE_OUTPUT`

说明：

- `MEDIA_DIR` 用于缓存下载的飞书图片或 agent 输出图片
- 输入输出开关便于灰度发布
- 大小限制必须可配置

## 持久化与缓存

首期不建议把图片二进制直接写入当前 `ConversationRepository`。

建议：

- 会话绑定仓储继续只存轻量上下文
- 图片文件存磁盘缓存目录
- 会话中只保存必要引用信息

如果后续要支持“引用上一张图继续对话”，可以追加一个轻量媒体索引仓储：

- `conversation_key`
- `message_id`
- `local_path`
- `mime_type`
- `created_at`
- `expires_at`

## 错误处理

需要新增几类可读错误：

- 图片类型不支持
- 图片过大
- 图片下载失败
- 图片上传飞书失败
- 当前 agent / model 不支持图片输入
- agent 返回了无法解析的图片块

用户侧错误文案应尽量具体，例如：

- “当前 agent 不支持图片输入，请切换到支持视觉能力的模型。”
- “图片过大，已超过 10 MB 限制。”

## 安全要求

- 严格限制允许的 MIME 类型
- 严格限制最大文件大小
- 不信任文件扩展名，只信任探测出的 MIME
- 不直接执行图片关联路径
- 本地缓存目录应可定期清理

## 分阶段落地建议

### Phase 0

- 完成 domain / ports 的结构化消息改造设计
- 确认 `acpx` 是否已有结构化图片输入输出能力

### Phase 1

- 支持飞书图片输入
- 支持 agent 图片输出
- 不做图片 + 文本自动合并
- 只支持顺序多条回复

### Phase 2

- 支持同线程短窗口内的图片 + 文本合并
- 支持缓存最近一张图并继续追问

### Phase 3

- 评估卡片消息渲染
- 评估流式进度 + 最终图片回传

## 验收标准

至少满足以下验收项：

1. 用户发送一张 PNG 图片，agent 能收到并返回文本分析。
2. 用户发送图片后紧接一条文本说明，系统能把两者作为同一次多模态请求处理。
3. agent 返回一张图片时，飞书侧能收到可正常展示的图片消息。
4. agent 同时返回文本和图片时，飞书侧能按顺序完整收到。
5. 当模型不支持图片输入时，用户能收到明确提示，而不是静默失败。

## Markdown 文档扩展

`markdown` 不应作为图片能力的特例实现，而应直接落在统一块模型里。

### 块模型扩展

当前长期版实现已经使用：

```rust
pub enum MessageBlock {
    Text { text: String },
    Image { mime_type: String, source: BinarySource, alt: Option<String> },
    Document {
        mime_type: String,
        file_name: String,
        source: BinarySource,
        extracted_text: Option<String>,
    },
}
```

其中 `markdown` 约定为：

- `mime_type = "text/markdown"`
- `file_name` 保留原始文件名
- `source` 保存本地缓存路径或字节
- `extracted_text` 保存 UTF-8 文本，供当前 `acpx` CLI 适配器内联成 prompt

### 飞书 -> agent

长期版实际链路：

1. 飞书收到 `message_type = "file"`。
2. 仅放行 `.md/.markdown`，并校验大小、UTF-8、纯文本特征。
3. 下载文件到 `FEISHU2ACP_MEDIA_DIR/inbound/`。
4. 生成 `MessageBlock::Document`。
5. `AcpxGateway` 目前会把 `Document` 块格式化成文本 prompt：

```text
附加 markdown 文档 `README.md`：
<markdown-document file_name="README.md">
...markdown 原文...
</markdown-document>
```

这样即使 `acpx` 还没有结构化文件协议，也能先跑通长期版 domain/ports 设计。

### agent -> 飞书

长期版同时预留了文档回传能力。

当前实现使用一个桥接协议让纯文本 CLI 输出也能带文件：

```text
<feishu2acp-document file_name="plan.md">
# Plan
...
</feishu2acp-document>
```

桥接层会把它解析成 `MessageBlock::Document`，随后：

1. 上传飞书 `im.v1.file`
2. 发送 `msg_type = "file"` 消息

文本与文件可以混排，按块顺序依次回飞书。

备注：

- 飞书 IM 文件接口当前不接受 `.md` 扩展名上传，因此回传时使用 `.md.txt` 作为传输文件名
- 入站解析时会把 `.md.txt` 还原成逻辑上的 `*.md`

### 配置项

文档能力新增了这些配置：

- `FEISHU2ACP_MEDIA_DIR`
- `FEISHU2ACP_MAX_MARKDOWN_BYTES`
- `FEISHU2ACP_ENABLE_MARKDOWN_INPUT`
- `FEISHU2ACP_ENABLE_MARKDOWN_OUTPUT`

### 测试脚本

仓库新增：

- `cargo run --bin send_markdown_file -- --file README.md --note "请总结这个文档"`

默认会发送到群：

- `oc_f5f9c8e4001155b3d3fd395426388ce4`

## 开放问题

- `acpx` 当前是否已经支持结构化图片 prompt 和结构化图片 reply？
- 飞书图片消息是否需要额外鉴权或下载 token 刷新？
- 图片 + 文本是否要做自动合并，还是要求用户显式命令触发？
- agent 返回本地路径时，是否允许读取工作目录外文件？

## 实现备注

实现时应优先保持边界清晰：

- 飞书媒体下载/上传逻辑只放在 adapter 层
- ACP 内容块转换只放在 gateway adapter 层
- 应用层只编排消息块，不关心飞书或 ACP 的具体字段细节

这样后续即使更换飞书 SDK 或 `acpx` 接入方式，也不会把多模态逻辑散落到整个项目里。
