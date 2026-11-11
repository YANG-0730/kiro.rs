//! 元数据事件
//!
//! 处理 metadataEvent：上游在流末尾给出的停止原因与停止详情。
//! 实测样例：
//! - 正常：`{"stopReason":"TOOL_USE"}`
//! - 拒答：`{"stopDetails":{"refusal":{"category":"REASONING_EXTRACTION","explanation":"..."}},"stopReason":"..."}`

use serde::Deserialize;

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

/// 元数据事件
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataEvent {
    /// 上游停止原因（如 `TOOL_USE`）
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// 停止详情（目前只见到 refusal）
    #[serde(default)]
    pub stop_details: Option<StopDetails>,
}

/// 停止详情
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopDetails {
    /// 安全分类器 / 模型拒答详情
    #[serde(default)]
    pub refusal: Option<RefusalDetails>,
}

/// 拒答详情
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusalDetails {
    /// 拒答类别（上游为大写下划线，如 `REASONING_EXTRACTION`）
    #[serde(default)]
    pub category: Option<String>,
    /// 给用户看的说明
    #[serde(default)]
    pub explanation: Option<String>,
}

impl EventPayload for MetadataEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_json()
    }
}

impl MetadataEvent {
    /// 拒答详情（存在即表示本次回复被拒）
    pub fn refusal(&self) -> Option<&RefusalDetails> {
        self.stop_details.as_ref()?.refusal.as_ref()
    }

    /// 转为 Anthropic `stop_details`（仅拒答时有值）：
    /// `{"type":"refusal","category":"reasoning_extraction","explanation":"..."}`，
    /// category 按官方取值转为小写。
    pub fn anthropic_stop_details(&self) -> Option<serde_json::Value> {
        let refusal = self.refusal()?;
        Some(serde_json::json!({
            "type": "refusal",
            "category": refusal.category.as_deref().map(str::to_ascii_lowercase),
            "explanation": refusal.explanation,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_stop_reason_is_not_refusal() {
        let e: MetadataEvent = serde_json::from_str(r#"{"stopReason":"TOOL_USE"}"#).unwrap();
        assert_eq!(e.stop_reason.as_deref(), Some("TOOL_USE"));
        assert!(e.refusal().is_none());
        assert!(e.anthropic_stop_details().is_none());
    }

    #[test]
    fn test_refusal_maps_to_anthropic_stop_details() {
        let e: MetadataEvent = serde_json::from_str(
            r#"{"stopDetails":{"refusal":{"category":"REASONING_EXTRACTION","explanation":"The selected model cannot continue this conversation."}},"stopReason":"CONTENT_FILTERED"}"#,
        )
        .unwrap();
        assert_eq!(
            e.anthropic_stop_details().unwrap(),
            serde_json::json!({
                "type": "refusal",
                "category": "reasoning_extraction",
                "explanation": "The selected model cannot continue this conversation."
            })
        );
    }

    #[test]
    fn test_refusal_without_category() {
        let e: MetadataEvent = serde_json::from_str(r#"{"stopDetails":{"refusal":{}}}"#).unwrap();
        assert_eq!(
            e.anthropic_stop_details().unwrap(),
            serde_json::json!({"type": "refusal", "category": null, "explanation": null})
        );
    }
}
