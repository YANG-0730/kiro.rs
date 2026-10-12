//! Token 计算模块
//!
//! 提供本地 token 数量估算与远程 count_tokens 回退逻辑。
//!
//! # 本地估算规则
//! - CJK 字符：约 1.5 字符/token
//! - 其他非空白字符：约 3.5 字符/token
//! - 忽略空白字符
//! - 最终四舍五入
//! - 图片按像素尺寸估算（Anthropic 视觉规则），不计 base64 文本
//! - 工具定义按名称 + 描述 + schema 实际内容计数

use crate::anthropic::types::{
    CountTokensRequest, CountTokensResponse, Message, SystemMessage, Tool,
};
use crate::http_client::{ProxyConfig, build_client};
use crate::model::config::TlsBackend;
use parking_lot::RwLock;
use std::sync::OnceLock;

/// 每个工具定义的固定包装开销（名称/描述/schema 之外的结构 token）
const TOKENS_PER_TOOL_OVERHEAD: u64 = 10;
const TOKENS_PER_MESSAGE: u64 = 4;

/// Count Tokens API 配置
#[derive(Clone, Default)]
pub struct CountTokensConfig {
    /// 外部 count_tokens API 地址
    pub api_url: Option<String>,
    /// count_tokens API 密钥
    pub api_key: Option<String>,
    /// count_tokens API 认证类型（"x-api-key" 或 "bearer"）
    pub auth_type: String,
    /// 代理配置
    pub proxy: Option<ProxyConfig>,

    pub tls_backend: TlsBackend,
}

/// 全局配置存储
static COUNT_TOKENS_CONFIG: OnceLock<CountTokensConfig> = OnceLock::new();

/// 代理配置的运行时可变存储（热更新时同步刷新）
static COUNT_TOKENS_PROXY: OnceLock<RwLock<Option<ProxyConfig>>> = OnceLock::new();

/// 初始化 count_tokens 配置
///
/// 应在应用启动时调用一次
pub fn init_config(config: CountTokensConfig) {
    let proxy = config.proxy.clone();
    let _ = COUNT_TOKENS_CONFIG.set(config);
    let _ = COUNT_TOKENS_PROXY.set(RwLock::new(proxy));
}

/// 热更新代理配置
pub fn update_proxy(proxy: Option<ProxyConfig>) {
    if let Some(lock) = COUNT_TOKENS_PROXY.get() {
        *lock.write() = proxy;
    }
}

/// 获取当前代理配置
fn get_current_proxy() -> Option<ProxyConfig> {
    COUNT_TOKENS_PROXY
        .get()
        .map(|lock| lock.read().clone())
        .unwrap_or(None)
}

/// 获取配置
fn get_config() -> Option<&'static CountTokensConfig> {
    COUNT_TOKENS_CONFIG.get()
}

fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{3040}'..='\u{309F}'
            | '\u{30A0}'..='\u{30FF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{1100}'..='\u{11FF}'
            | '\u{3130}'..='\u{318F}'
    )
}

/// 计算文本的 token 数量
pub fn count_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }

    let mut cjk_count = 0usize;
    let mut other_count = 0usize;

    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }

        if is_cjk(c) {
            cjk_count += 1;
        } else {
            other_count += 1;
        }
    }

    let tokens = (cjk_count as f64 / 1.5) + (other_count as f64 / 3.5);
    tokens.round() as u64
}

/// 估算请求的输入 tokens
///
/// 优先调用远程 API，失败时回退到本地计算
pub(crate) fn count_all_tokens(
    model: String,
    system: Option<Vec<SystemMessage>>,
    messages: Vec<Message>,
    tools: Option<Vec<Tool>>,
) -> u64 {
    // 检查是否配置了远程 API
    if let Some(config) = get_config()
        && config.api_url.is_some()
    {
        // 尝试调用远程 API
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(call_remote_count_tokens(
                config, model, &system, &messages, &tools,
            ))
        });

        match result {
            Ok(tokens) => {
                tracing::debug!("远程 count_tokens API 返回: {}", tokens);
                return tokens;
            }
            Err(e) => {
                tracing::warn!("远程 count_tokens API 调用失败，回退到本地计算: {}", e);
            }
        }
    }

    // 本地计算
    count_all_tokens_local(system, messages, tools)
}

/// 调用远程 count_tokens API
async fn call_remote_count_tokens(
    config: &CountTokensConfig,
    model: String,
    system: &Option<Vec<SystemMessage>>,
    messages: &[Message],
    tools: &Option<Vec<Tool>>,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let api_url = config.api_url.as_ref().unwrap();
    let current_proxy = get_current_proxy();
    let client = build_client(current_proxy.as_ref(), 300, config.tls_backend)?;

    // 构建请求体
    let request = CountTokensRequest {
        model,
        messages: messages.to_vec(),
        system: system.clone(),
        tools: tools.clone(),
    };

    // 构建请求
    let mut req_builder = client.post(api_url);

    // 设置认证头
    if let Some(api_key) = &config.api_key {
        if config.auth_type == "bearer" {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", api_key));
        } else {
            req_builder = req_builder.header("x-api-key", api_key);
        }
    }

    // 发送请求
    let response = req_builder
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("API 返回错误状态: {}", response.status()).into());
    }

    let result: CountTokensResponse = response.json().await?;
    Ok(result.input_tokens as u64)
}

/// 估算消息列表的 tokens
///
/// 按内容块计数（与 cache_tracker 的分块口径一致），而不是把整个 messages 序列化成
/// JSON 再计数——后者会把键名/引号/转义算进去，并把图片 base64 当作文本，导致含图
/// 请求的估算偏高几个数量级。
fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|m| count_message_content_tokens(&m.content) + TOKENS_PER_MESSAGE)
        .sum()
}

fn count_serialized_value_tokens(value: &serde_json::Value) -> u64 {
    let json = serde_json::to_string(value).unwrap_or_default();
    count_tokens(&json)
}

/// 无法解析尺寸的图片按此估算（约等于 Anthropic 缩放上限 1.15MP / 750）
const FALLBACK_IMAGE_TOKENS: u64 = 1_600;

/// 按 Anthropic 视觉计费规则估算图片 tokens（基于像素尺寸，而非 base64 长度）
fn estimate_image_block_tokens(obj: &serde_json::Map<String, serde_json::Value>) -> u64 {
    let Some(source) = obj.get("source") else {
        return FALLBACK_IMAGE_TOKENS;
    };
    match source.get("data").and_then(|v| v.as_str()) {
        Some(data) if source.get("type").and_then(|v| v.as_str()) == Some("base64") => {
            crate::image::estimate_image_tokens(data)
                .map(|(tokens, _, _)| tokens)
                .unwrap_or(FALLBACK_IMAGE_TOKENS)
        }
        _ => FALLBACK_IMAGE_TOKENS,
    }
}

fn estimate_content_block_tokens(obj: &serde_json::Map<String, serde_json::Value>) -> u64 {
    if obj.get("type").and_then(|v| v.as_str()) == Some("image") {
        return estimate_image_block_tokens(obj);
    }

    if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
        return count_tokens(text);
    }

    if let Some(thinking) = obj.get("thinking").and_then(|v| v.as_str()) {
        return count_tokens(thinking);
    }

    if let Some(input) = obj.get("input") {
        // tool_use：工具名 + 参数
        let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        return count_tokens(name) + count_serialized_value_tokens(input);
    }

    if let Some(content) = obj.get("content") {
        return count_message_content_tokens(content);
    }

    match obj.get("type").and_then(|v| v.as_str()) {
        // redacted_thinking：加密内容按序列化长度估算
        Some("redacted_thinking") => obj
            .get("data")
            .and_then(|v| v.as_str())
            .map(count_tokens)
            .unwrap_or(0),
        // document（PDF 等）：base64 来源无法本地解析页数，按解码字节粗估（约 1 token / 3 字节）；
        // 文本来源按文本计数
        Some("document") => match obj.get("source") {
            Some(src) if src.get("type").and_then(|v| v.as_str()) == Some("base64") => src
                .get("data")
                .and_then(|v| v.as_str())
                .map(|d| (d.len() as u64 * 3 / 4) / 3)
                .unwrap_or(0),
            Some(src) => src
                .get("data")
                .and_then(|v| v.as_str())
                .map(count_tokens)
                .unwrap_or(0),
            None => 0,
        },
        _ => 0,
    }
}

/// 本地计算请求的输入 tokens
fn count_all_tokens_local(
    system: Option<Vec<SystemMessage>>,
    messages: Vec<Message>,
    tools: Option<Vec<Tool>>,
) -> u64 {
    let system_tokens: u64 = system
        .unwrap_or_default()
        .iter()
        .map(count_system_message_tokens)
        .sum();
    let message_tokens = estimate_messages_tokens(&messages);
    let tool_tokens: u64 = tools
        .as_ref()
        .map(|items| items.iter().map(count_tool_definition_tokens).sum())
        .unwrap_or(0);

    (system_tokens + message_tokens + tool_tokens).max(1)
}

/// 估算输出 tokens
pub(crate) fn estimate_output_tokens(content: &[serde_json::Value]) -> i32 {
    let total: i32 = content
        .iter()
        .map(|block| count_message_content_tokens(block) as i32)
        .sum();

    total.max(1)
}

/// 计算系统消息的 tokens
pub(crate) fn count_system_message_tokens(message: &SystemMessage) -> u64 {
    count_tokens(&message.text)
}

/// 计算工具定义的 tokens
///
/// 按名称 + 描述 + input_schema 的实际内容计数，外加少量固定开销（工具定义包装）。
/// 此前固定 150/个，Claude Code 这类工具描述很长的客户端会被严重低估。
pub(crate) fn count_tool_definition_tokens(tool: &Tool) -> u64 {
    let schema_tokens = if tool.input_schema.is_empty() {
        0
    } else {
        serde_json::to_string(&tool.input_schema)
            .map(|s| count_tokens(&s))
            .unwrap_or(0)
    };
    TOKENS_PER_TOOL_OVERHEAD
        + count_tokens(&tool.name)
        + count_tokens(&tool.description)
        + schema_tokens
}

/// 计算消息内容的 tokens
pub(crate) fn count_message_content_tokens(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::Null => 0,
        serde_json::Value::String(s) => count_tokens(s),
        serde_json::Value::Array(arr) => arr.iter().map(count_message_content_tokens).sum(),
        serde_json::Value::Object(obj) => estimate_content_block_tokens(obj),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::collections::HashMap;

    fn png_base64(w: u32, h: u32) -> String {
        let mut png = Vec::new();
        image::DynamicImage::new_rgb8(w, h)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(&png)
    }

    #[test]
    fn image_tokens_follow_pixel_size_not_base64_length() {
        // 1000x1000 → 1_000_000 / 750 ≈ 1333 tokens，无论 base64 多长
        let msg = Message {
            role: "user".to_string(),
            content: serde_json::json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png_base64(1000, 1000)}}
            ]),
        };
        let tokens = estimate_messages_tokens(&[msg]);
        assert_eq!(tokens, 1333 + TOKENS_PER_MESSAGE);
    }

    #[test]
    fn image_with_unparseable_data_uses_fallback() {
        let msg = Message {
            role: "user".to_string(),
            content: serde_json::json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "not-an-image"}}
            ]),
        };
        assert_eq!(
            estimate_messages_tokens(&[msg]),
            FALLBACK_IMAGE_TOKENS + TOKENS_PER_MESSAGE
        );
    }

    #[test]
    fn tool_result_images_are_counted_by_pixels() {
        let msg = Message {
            role: "user".to_string(),
            content: serde_json::json!([
                {"type": "tool_result", "tool_use_id": "t", "content": [
                    {"type": "text", "text": "ok"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png_base64(750, 100)}}
                ]}
            ]),
        };
        // "ok" ≈ 1 token；750*100/750 = 100 tokens
        assert_eq!(
            estimate_messages_tokens(&[msg]),
            1 + 100 + TOKENS_PER_MESSAGE
        );
    }

    #[test]
    fn message_estimate_excludes_json_structure_overhead() {
        let text = "hello world ".repeat(100);
        let msg = Message {
            role: "user".to_string(),
            content: serde_json::json!([{"type": "text", "text": text}]),
        };
        assert_eq!(
            estimate_messages_tokens(&[msg]),
            count_tokens(&text) + TOKENS_PER_MESSAGE
        );
    }

    #[test]
    fn document_and_redacted_thinking_blocks_are_counted() {
        let pdf_b64 = "A".repeat(40_000); // 30_000 字节 → ~10_000 tokens
        let doc = serde_json::json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": pdf_b64}});
        assert_eq!(count_message_content_tokens(&doc), 10_000);
        let text_doc = serde_json::json!({"type": "document", "source": {"type": "text", "media_type": "text/plain", "data": "hello world"}});
        assert_eq!(
            count_message_content_tokens(&text_doc),
            count_tokens("hello world")
        );
        let redacted = serde_json::json!({"type": "redacted_thinking", "data": "abcdefghij"});
        assert!(count_message_content_tokens(&redacted) > 0);
    }

    #[test]
    fn tool_definition_tokens_scale_with_content() {
        let mut schema = HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        let small = Tool {
            tool_type: None,
            name: "a".to_string(),
            description: "short".to_string(),
            input_schema: schema.clone(),
            max_uses: None,
            cache_control: None,
        };
        let large = Tool {
            description: "do something carefully ".repeat(500),
            ..small.clone()
        };
        let small_tokens = count_tool_definition_tokens(&small);
        let large_tokens = count_tool_definition_tokens(&large);
        assert!(small_tokens < 30, "small={small_tokens}");
        assert!(large_tokens > 2_500, "large={large_tokens}");
    }
}
