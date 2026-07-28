//! 推理内容事件
//!
//! 处理 reasoningContentEvent 类型的事件。
//!
//! 新模型（如 claude-opus-5 / claude-sonnet-5 / claude-opus-4.8）通过独立的
//! `reasoningContentEvent` 流式返回思考内容，而非老模型在正文中内联的
//! `<thinking>` 标签。每帧要么携带 `text`（思考文本增量），要么携带
//! `signature`（思考结束时的签名，通常为最后一帧）。

use serde::{Deserialize, Serialize};

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

/// 推理内容事件
///
/// # 设计说明
///
/// `text` 与 `signature` 均为可选：上游会先发送若干带 `text` 的帧，
/// 最后发送一帧带 `signature` 的帧。其余未使用字段通过 `#[serde(flatten)]`
/// 捕获到 `extra`，确保反序列化兼容。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningContentEvent {
    /// 思考文本增量
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// 思考结束签名（通常为最后一帧）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// 捕获其他未使用的字段，确保反序列化兼容性
    #[serde(flatten)]
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    extra: serde_json::Value,
}

impl EventPayload for ReasoningContentEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_text_frame() {
        let json = r#"{"text":"I'm thinking..."}"#;
        let event: ReasoningContentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.text.as_deref(), Some("I'm thinking..."));
        assert!(event.signature.is_none());
    }

    #[test]
    fn test_deserialize_signature_frame() {
        let json = r#"{"signature":"CAIS+AEK..."}"#;
        let event: ReasoningContentEvent = serde_json::from_str(json).unwrap();
        assert!(event.text.is_none());
        assert_eq!(event.signature.as_deref(), Some("CAIS+AEK..."));
    }

    #[test]
    fn test_deserialize_with_extra_fields() {
        let json = r#"{"text":"x","conversationId":"c-1","foo":123}"#;
        let event: ReasoningContentEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.text.as_deref(), Some("x"));
    }
}
