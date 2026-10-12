//! Anthropic API 类型定义

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// === 缓存控制 ===

/// 缓存控制配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

// === 错误响应 ===

/// API 错误响应
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

/// 错误详情
#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl ErrorResponse {
    /// 创建新的错误响应
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }

    /// 创建认证错误响应
    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid API key")
    }
}

// === Models 端点类型 ===

/// 模型信息
#[derive(Debug, Serialize)]
pub struct Model {
    pub id: String,
    pub object: String,
    pub created: i64,
    /// 官方 Models API 字段（RFC 3339）
    pub created_at: String,
    pub owned_by: String,
    pub display_name: String,
    /// 官方值为 "model"（旧值 "chat" 非官方）
    #[serde(rename = "type")]
    pub model_type: String,
    pub max_tokens: i32,
    /// 官方 Models API 字段：最大输入 tokens（上下文窗口）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
}

/// 模型列表响应
#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<Model>,
    /// 官方分页字段（本代理一次返回全部，has_more 恒为 false）
    pub has_more: bool,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
}

// === Messages 端点类型 ===

/// 最大思考预算 tokens
const MAX_BUDGET_TOKENS: i32 = 128_000;

/// Thinking 配置
#[derive(Debug, Deserialize, Clone)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(
        default = "default_budget_tokens",
        deserialize_with = "deserialize_budget_tokens"
    )]
    pub budget_tokens: i32,
    /// 思考内容展示方式（"summarized" / "omitted"），未设置时由上游默认
    #[serde(default)]
    pub display: Option<String>,
}

impl Thinking {
    /// 是否启用了 thinking（enabled 或 adaptive）
    pub fn is_enabled(&self) -> bool {
        self.thinking_type == "enabled" || self.thinking_type == "adaptive"
    }
}

fn default_budget_tokens() -> i32 {
    20000
}
fn deserialize_budget_tokens<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = i32::deserialize(deserializer)?;
    Ok(value.min(MAX_BUDGET_TOKENS))
}

/// OutputConfig 配置
#[derive(Debug, Deserialize, Clone)]
pub struct OutputConfig {
    /// 推理强度；未设置时为 None，交由上游模型默认（官方 Opus 5.5 默认 medium）。
    /// 旧实现缺省填 "high"，会让只带 `output_config.format` 等字段的请求被抬高一档。
    #[serde(default)]
    pub effort: Option<String>,
}

/// Claude Code 请求中的 metadata
#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    /// 用户 ID，格式如: user_xxx_account__session_0b4445e1-f5be-49e1-87ce-62bbc28ad705
    pub user_id: Option<String>,
}

/// Messages 请求体
#[derive(Debug, Clone, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    /// 为 Anthropic API 兼容保留，实际不透传给 Kiro 上游
    pub max_tokens: i32,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, deserialize_with = "deserialize_system")]
    pub system: Option<Vec<SystemMessage>>,
    pub tools: Option<Vec<Tool>>,
    #[allow(dead_code)]
    pub tool_choice: Option<serde_json::Value>,
    pub thinking: Option<Thinking>,
    pub output_config: Option<OutputConfig>,
    /// Claude Code 请求中的 metadata，包含 session 信息
    pub metadata: Option<Metadata>,
}

/// 反序列化 system 字段，支持字符串或数组格式
fn deserialize_system<'de, D>(deserializer: D) -> Result<Option<Vec<SystemMessage>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // 创建一个 visitor 来处理 string 或 array
    struct SystemVisitor;

    impl<'de> serde::de::Visitor<'de> for SystemVisitor {
        type Value = Option<Vec<SystemMessage>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or an array of system messages")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(Some(vec![SystemMessage {
                text: value.to_string(),
                block_type: None,
                cache_control: None,
            }]))
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut messages = Vec::new();
            while let Some(msg) = seq.next_element()? {
                messages.push(msg);
            }
            Ok(if messages.is_empty() {
                None
            } else {
                Some(messages)
            })
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            serde::de::Deserialize::deserialize(deserializer)
        }
    }

    deserializer.deserialize_any(SystemVisitor)
}

/// 消息
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    /// 可以是 string 或 ContentBlock 数组
    pub content: serde_json::Value,
}

/// 系统消息
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemMessage {
    pub text: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub block_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// 工具定义
///
/// 支持两种格式：
/// 1. 普通工具：{ name, description, input_schema }
/// 2. WebSearch 工具：{ type: "web_search_20250305", name: "web_search", max_uses: 8 }
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tool {
    /// 工具类型，如 "web_search_20250305"（可选，仅 WebSearch 工具）
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
    /// 工具名称
    #[serde(default)]
    pub name: String,
    /// 工具描述（普通工具必需，WebSearch 工具可选）
    #[serde(default)]
    pub description: String,
    /// 输入参数 schema（普通工具必需，WebSearch 工具无此字段）
    #[serde(default)]
    pub input_schema: HashMap<String, serde_json::Value>,
    /// 最大使用次数（仅 WebSearch 工具）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i32>,
    /// 缓存控制
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl Tool {
    /// 检查是否为 WebSearch 工具
    #[allow(dead_code)]
    pub fn is_web_search(&self) -> bool {
        self.tool_type
            .as_ref()
            .is_some_and(|t| t.starts_with("web_search"))
    }
}

/// 内容块
#[derive(Debug, Deserialize, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ImageSource>,
}

/// 图片数据源
#[derive(Debug, Deserialize, Serialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

// === Count Tokens 端点类型 ===

/// Token 计数请求
#[derive(Debug, Serialize, Deserialize)]
pub struct CountTokensRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_system"
    )]
    pub system: Option<Vec<SystemMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// Token 计数响应
#[derive(Debug, Serialize, Deserialize)]
pub struct CountTokensResponse {
    pub input_tokens: i32,
}

/// 根据模型名称获取上下文窗口大小
///
/// 注意：这是**兜底**逻辑，权威来源是上游 `ListAvailableModels` 的
/// `maxInputTokens`（见 `ModelRegistry::max_input_tokens_for`）。仅当 registry
/// 未命中（拉取失败 / 测试）时才依据模型名粗略判断。
///
/// - 1M 上下文: Claude Opus/Sonnet 4.6 及以上（含 opus-5 / sonnet-5 / opus-4.7 / opus-4.8）
/// - 其他模型: 200,000 tokens
pub fn get_context_window_size(model: &str) -> i32 {
    let model_lower = model.to_lowercase();
    let is_claude_opus_or_sonnet = model_lower.contains("opus") || model_lower.contains("sonnet");
    // 1M 上下文的 Claude 版本：4.6 / 4.7 / 4.8 / 5（含横杠与点号两种写法）
    let is_1m_version = ["4-6", "4.6", "4-7", "4.7", "4-8", "4.8"]
        .iter()
        .any(|v| model_lower.contains(v))
        || model_lower.contains("opus-5")
        || model_lower.contains("sonnet-5")
        || model_lower.contains("opus5")
        || model_lower.contains("sonnet5");
    if is_claude_opus_or_sonnet && is_1m_version {
        1_000_000
    } else {
        200_000
    }
}
