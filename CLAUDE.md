# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

kiro-rs 是一个用 Rust 编写的 Anthropic Claude API 兼容代理服务，将 Anthropic API 请求转换为 Kiro API 请求。支持多凭据管理、自动故障转移、流式响应和 Web 管理界面。

**技术栈**: Rust (Axum 0.8 + Tokio) + React 19 + TypeScript + Tailwind CSS

## 常用命令

```bash
# 使用 Makefile（推荐）
make help          # 查看所有可用命令
make release       # 构建前端 + 后端（release）
make dev           # 开发运行（自动构建前端，启用 sensitive-logs）
make check         # 格式化 + lint + 测试
make ui            # 仅构建前端
make ui-dev        # 前端开发服务器

# 手动构建（必须先构建前端）
cd admin-ui && pnpm install && pnpm build
cargo build --release

# 开发运行
cargo run -- -c config.json --credentials credentials.json

# 测试
cargo test
cargo test <test_name>           # 运行单个测试

# 代码检查
cargo fmt          # 格式化
cargo clippy       # lint

# 启用敏感日志构建（排障用，输出 token 用量等诊断信息）
cargo run --features sensitive-logs -- -c config.json --credentials credentials.json

# 前端开发
cd admin-ui
pnpm install
pnpm dev           # 开发服务器
pnpm build         # 生产构建

# 排障工具
python tools/test_400_improperly_formed.py  # 测试上游 400 错误场景
```

## 请求处理流程

```
POST /v1/messages (Anthropic 格式)
  → auth_middleware: 验证 x-api-key / Bearer token（subtle 常量时间比较）
  → post_messages handler:
      1. 判断 WebSearch 触发条件，决定本地处理或剔除后转发
      2. converter::convert_request() 转换为 Kiro 请求格式
      3. provider.call_api() 发送请求（含重试和故障转移）
      4. stream.rs 解析 AWS Event Stream → 转换为 Anthropic SSE 格式返回
```

## 核心设计模式

1. **Provider Pattern** - `kiro/provider.rs`: 统一的 API 提供者接口，处理请求转发和重试。支持凭据级代理（每个凭据可配独立 HTTP/SOCKS5 代理，缓存对应 HTTP Client 避免重复创建）
2. **Multi-Token Manager** - `kiro/token_manager.rs`: 多凭据管理，按优先级故障转移，后台异步刷新 Token（支持 Social 和 IdC 两种认证方式）。余额缓存动态 TTL：高频用户 10 分钟、低频用户 30 分钟、低余额（超额计费中）用户 10 分钟，过期时异步刷新不阻塞请求。因额度耗尽（InsufficientBalance / QuotaExceeded）被禁用的凭据记录重置时间（上游 `nextDateReset`，缺省为下月 1 日 00:00 UTC），到点后复查余额并自动重新启用（Kiro 额度按 UTC 自然月重置）
3. **Protocol Converter** - `anthropic/converter.rs`: Anthropic ↔ Kiro 双向协议转换，包括模型映射（sonnet/opus/haiku → Kiro 模型 ID）、JSON Schema 规范化（修复 MCP 工具的 `required: null` / `properties: null`）、工具占位符生成、图片格式转换。工具定义总大小超过 `compression.toolDefinitionsMaxBytes`（默认 256KB，0=不压缩；不受 `compression.enabled` 控制）时才简化 schema 并截断描述——旧的 20KB 硬编码会截掉 Claude Code 约四成工具说明，2026-10 实测上游可正常接受 ~246KB 完整工具定义
4. **Event Stream Parser** - `kiro/parser/`: AWS Event Stream 二进制协议解析（header + payload + CRC32C 校验）
5. **Streaming Response** - `anthropic/stream.rs`: 使用 `StreamContext` 实时将 Kiro 事件转换为 Anthropic SSE，最终 usage（`message_delta`）的总输入优先采用上游 `contextUsageEvent` 换算值（缺失时回退本地估算），cache 读写按同比例缩放，并透传上游 `meteringEvent` 的 credit；`message_start` 只能用本地估算
6. **Input Compressor** - `anthropic/compressor.rs`: 多层压缩管道（空白压缩 → thinking 截断 → tool_result 截断 → tool_use input 截断 → 历史截断），自动修复 tool_use/tool_result 配对以避免上游 400 错误
7. **Image Processor** - `image.rs`: 图片处理（缩放、GIF 抽帧、token 计算）。GIF 抽帧策略：最多 20 帧、最多 5fps、按时长自适应采样间隔，输出为 JPEG 静态帧序列

## 共享状态

```rust
AppState {
    api_key: String,                          // Anthropic API 认证密钥
    kiro_provider: Option<Arc<KiroProvider>>,  // 核心 API 提供者（Arc 线程安全共享）
    profile_arn: Option<String>,               // AWS Profile ARN
    compression_config: CompressionConfig,     // 输入压缩配置
}
```

通过 Axum `State` extractor 注入到所有 handler 中。

## 凭据故障转移与冷却

- 凭据按 `priority` 字段排序，优先使用高优先级凭据
- 同优先级内先用仍有基础额度的凭据，全部进入超额后才用超额（超额另行计费，基础额度月底作废）；亲和绑定在超额凭据上的会话，若同优先级（或更高）还有基础额度凭据会改绑。跨月后对超额中凭据补查一次余额，避免整月误判
- 请求失败时 `report_failure()` 触发故障转移到下一个可用凭据
- 冷却递增只针对"连续"触发：上次冷却结束超过 10 分钟再触发则从头计时（以前计数终身不清零，偶发 429 会让冷却永久停在 300s 上限）
- 账户临时停用（TEMPORARILY_SUSPENDED，对话或余额接口返回）与"拒答过多被限制"（kirodotdev/Kiro #12310，200 + refusal 说明含 "excessive content refusals"）：凭据冷却 1 小时并解除亲和绑定，到期自动复查；不重试被拒的请求、不跨凭据重放拒答
- Token 刷新：所有刷新路径（业务请求 / 后台 / 强制 / 余额）共用 `refresh_lock`，拿锁后重读凭据，写回时只合并 token 字段（避免冲掉 Admin 修改或并发轮换 refreshToken 导致 invalid_grant）；刷新失败短冷却；响应缺 `expiresIn` 按 3600s
- 模型列表为空（启动时拉取全部失败）时后台每分钟重试，期间模型按 best effort 映射而不是拒绝所有请求；余额后台查询失败 5 分钟内不再重复发起；`usageBreakdownList` 为空不视为额度耗尽
- 冷却分类管理：`FailureLimit` / `InsufficientBalance` / `ModelUnavailable` / `QuotaExceeded`
- `MODEL_TEMPORARILY_UNAVAILABLE` / `INSUFFICIENT_MODEL_CAPACITY` / `SERVICE_REQUEST_RATE_EXCEEDED` / "unexpectedly high load"（上游模型级过载 / 容量不足 / 服务级限流）与凭据无关：不禁用/不冷却凭据，单请求最多 4 次、1s→2s→4s 退避重试，仍失败返回 529 `overloaded_error` 交给客户端重试
- 按上游错误 `reason` 分类（对齐官方客户端）：402 `MONTHLY_REQUEST_COUNT` / `OVERAGE_REQUEST_LIMIT_EXCEEDED` 与 429 `MONTHLY_REQUEST_COUNT` → 额度耗尽禁用到重置；429 `DAILY_REQUEST_COUNT` → 冷却到下一个 UTC 日；其余 429 → 凭据冷却（优先用 `x-amzn-kiro-ratelimit-retry-after` / `retryAfterMilliseconds` 提示）；403 `TEMPORARILY_SUSPENDED` / "temporarily paused new users" → 1 小时冷却后复查；402 `CONVERSATION_LIMIT_EXCEEDED` → 400 不换凭据
- 返回客户端的错误一律用官方信封 `{"type":"error","error":{...}}` 与官方类型：上游 5xx/过载 → 529 `overloaded_error`，上游 429 → 429，额度耗尽 → 429 + `x-should-retry: false`，全池认证失败 → 500 `api_error`，未知模型 → 404 `not_found_error`，上下文超长以 "prompt is too long" 开头，请求体 JSON 错误 → 400、超限 → 413
- 流中途上游报错（error 帧 / 异常 / 建模错误事件）、连接中断或末尾帧不完整：流式发 Anthropic `error` 事件并停止（不再 message_stop 冒充正常结束），非流式返回对应状态码；`ContentLengthExceededException` 与未收完的工具调用按 `max_tokens` 结束

## API 端点

**代理端点**:
- `GET /v1/models` - 获取可用模型列表
- `POST /v1/messages` - 创建消息（Anthropic 格式）
- `POST /v1/messages/count_tokens` - Token 计数

**Admin API** (需配置 `adminApiKey`):
- 凭据 CRUD、状态监控、余额查询

## 重要注意事项

1. **构建顺序**: 必须先构建前端 `admin-ui`，再编译 Rust 后端（静态文件通过 `rust-embed` 嵌入，derive 宏为 `#[derive(Embed)]`）
2. **凭据格式**: 支持单凭据（向后兼容）和多凭据（数组格式，支持 priority 字段）
3. **重试策略**: 单凭据最多重试 2 次，单请求最多重试 3 次
4. **WebSearch 工具**: 仅当请求明确触发 WebSearch（`tool_choice` 强制 / 仅提供 `web_search` 单工具 / 消息前缀匹配）时走本地 WebSearch；否则从 `tools` 中剔除 `web_search` 后转发上游（避免误路由）
5. **安全**: 使用 `subtle` 库进行常量时间比较防止时序攻击；Admin API Key 空字符串视为未配置
6. **Prefill 处理**: Claude 4.x 已弃用 assistant prefill，末尾 assistant 消息被静默丢弃
7. **sensitive-logs 特性**: 编译时 feature flag，启用后输出 token 用量诊断日志和请求体大小（默认关闭，仅用于排障）
8. **网络错误分类**: 连接关闭/重置、发送失败等网络错误被归类为瞬态上游错误，返回 502（不记录请求体）。对话 API 的 HTTP Client 不设整体超时（只设 30s 连接超时 + 300s 读空闲超时，与官方 CLI 一致），否则超过时限的长回复会被截断
9. **Rust edition**: 项目使用 Rust 2024 edition
10. **图片处理**: GIF 会被抽帧并重编码为 JPEG 静态帧序列（最多 20 帧、最多 5fps），以降低请求体大小并提升内容识别效果。图片缩放规则：长边超过 4000px 或总像素超过 400 万时等比缩放
11. **上游限制（2026-10-12 实测）**: 请求体约 29~30MiB（`maxRequestBodyBytes` 默认 29MiB，超出本地返回 413 `request_too_large`，与官方一致）、单请求图片 100 张（`MAX_TOTAL_IMAGES`，超出时丢弃最旧的历史图片）、工具定义 ≥2MB 均可；真正的硬上限是模型上下文窗口。上游其余 400 透传为 400，不再变成 502。Write/Edit 分块写入策略（`compression.chunkedWritePolicy`）默认关闭：实测上游可完整返回单次 2600+ 行的 Write，不再需要"每次 ≤50 行"约束
12. **端点与版本截止**: 默认 `defaultEndpoint=cli`（上报 Kiro CLI 2.37.1）。2026-11-10 起上游对旧 target `AmazonQDeveloperStreamingService.SendMessage` 及无 target 的 `/mcp` 一律返回 403 `FEATURE_NOT_SUPPORTED`（"This version of Kiro is no longer supported"，与版本号无关）；现用 `AmazonCodeWhispererStreamingService.GenerateAssistantResponse` / `.InvokeMCP`，二者要求 profileArn（API 放请求体、MCP 放 `x-amzn-kiro-profile-arn` 头），IdC 凭据缺失时经 `ListAvailableProfiles` 自动获取并回写。2027-01-15 起上游拒绝 CLI ≤ 2.5.0。该 403 不计入凭据失败、直接返回 502，避免版本截止时把整个号池禁用。可选端点 `cli3` 模拟官方 CLI 3.x：对话由其内嵌 Node 引擎 KAS 发往新服务 KiroRuntimeService（`runtime.{region}.kiro.dev`，awsJson 1.0，Content-Type 必须为 `application/x-amz-json-1.0`，API/MCP 的 profileArn 都放请求体），UA 对齐官方 kiro-cli 3.2.0：`aws-sdk-js/1.0.0 ua/2.1 os/linux#… lang/js md/nodejs#… api/kiroruntime#1.0.0 m/E,N KiroCLI/3.2.0 KAS/0.66.95 os/linux md/appVersion-3.2.0 app/AmazonQ-For-CLI`（1.0.0 为 KRS 客户端包版本），并带 `x-amzn-kiro-agent-mode: vibe`（2026-11-11 实测对话 / WebSearch 均正常）；余额 / 模型列表与官方一致走控制面 KiroControlPlaneBearer（`GET https://management.{region}.kiro.dev/Get-Usage-Limits?origin=KIRO_CLI&profileArn=…` 与 `/List-Available-Models?origin&profileArn&nextToken`，Bearer + TokenType 头，UA `api/kirocontrolplanebearer#1.0.0`），响应结构与 q 端点相同，失败时回退 q 端点旧接口（2026-11-11 实测余额、22 个模型及模型 schema 与 q 一致）；`cli` 端点仍只走 q（ListAvailableModels 用 origin `KIRO_CLI`、带 profileArn、按 nextToken 翻页）。appVersion 3.x 只有配合这种 JS UA 才被接受，套在 Rust CLI 的 UA 上会被拒。API 区域优先取 profileArn 中的区域（与官方一致）。`ide` 端点上报 0.11.107 已被上游截止，不再注册：配置 / 凭据里的 ide 启动时自动迁移为 cli，Admin 也不再接受 ide。Token 刷新（AWS SSO OIDC）不带版本号，不受影响
13. **会话中途的 `role:"system"` 消息**: Claude Code 对新模型把环境信息（工作目录 / 平台 / Shell）和每轮提醒放在 `messages` 里的 `role:"system"` 条目（通常紧跟最后一条 user）。Kiro 只有 user / assistant，转换时以 `<system-reminder>` 包裹折叠进相邻 user 消息（不能当 prefill 丢弃）
14. **document / thinking**: Anthropic `document` 块（PDF 等）转为所在消息的 `documents[]`，当前消息与历史消息都附加（与官方客户端一致；2026-11-11 实测 cli / cli3 历史文档可读），每条最多 5 个、名称字符受限且整个请求内去重，超额转为占位文本；`content` 来源的文档与 `search_result` 块内联为文本，URL / Files API 来源的图片与文档留占位说明（不再静默丢弃）。上游无法执行的服务端工具（web_fetch / code_execution / tool_search / mcp_toolset / advisor）从 tools 中剔除。effort 独立于 thinking 下发（显式关闭 thinking 时 xhigh / max 降为 high）。schema 含 `thinking` 的新模型只用结构化字段 `additionalModelRequestFields` 控制思考（thinking.type / display、output_config.effort），不再注入 `<thinking_mode>` 标签；老模型仍靠标签。**该字段是请求顶层成员**（与 `conversationState` / `profileArn` 并列，见官方 Smithy 模型），旧版放在 `userInputMessage` 里会被上游静默忽略（2026-11-11 A/B 实测：effort / display 均不生效）。客户端未设置 thinking/effort 时不下发，交由上游默认。历史 assistant 的 thinking 块：结构化思考的新模型与官方 Kiro CLI 3.x（`@kiro/agent`）一致，放进 `assistantResponseMessage.reasoningContent`（`{reasoningText:{text,signature}}` 或 `{redactedContent}`，每条消息只取最后一段，无签名/空文本不回传），绝不拼进正文（内联 `<thinking>` 会被上游以 `REASONING_EXTRACTION` 拒答）；上游会验签，`THINKING_SIGNATURE_INVALID` 时去掉全部历史推理重试一次。响应侧按"签名封口"切分推理段，签名在块结束前一次性下发（`signature_delta`），`redactedContent` 映射为 `redacted_thinking`。老模型仍内联 `<thinking>`。上游拒答经 `metadataEvent.stopDetails.refusal` 下发，映射为 `stop_reason: "refusal"` + `stop_details`（不再是空回复）
15. **输入压缩**（`compression.enabled`，默认关闭）: 开启后对每个请求按阈值执行有损的多层压缩（空白压缩 → thinking 截断 → tool_result 截断 → tool_use input 截断 → 历史截断），并自动修复 tool_use/tool_result 配对以避免上游 400 错误
16. **上游 400 排障**: 若遇到 `Improperly formed request` 错误，参考 `docs/troubleshooting/400-improperly-formed-request.md` 和 `tools/test_400_improperly_formed.py` 进行诊断
