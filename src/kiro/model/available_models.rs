//! 上游 ListAvailableModels 响应类型

use serde::{Deserialize, Serialize};

/// GET /ListAvailableModels 响应
///
/// 上游还会返回 `defaultModel` / `nextToken` 等字段，但当前按 union 取
/// 全部 `models`，不依赖默认模型与分页（模型量小，单页返回），故只解析
/// `models`，其余字段由 serde 忽略。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAvailableModelsResponse {
    pub models: Vec<AvailableModelEntry>,
}

/// 单个可用模型条目
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableModelEntry {
    pub model_id: String,
    pub model_name: Option<String>,
    pub description: Option<String>,
    pub rate_multiplier: Option<f64>,
    pub rate_unit: Option<String>,
    pub token_limits: Option<TokenLimits>,
    pub supported_input_types: Option<Vec<String>>,
    pub prompt_caching: Option<PromptCachingInfo>,
    pub model_provider: Option<String>,
    pub status: Option<String>,
    pub available_origins: Option<Vec<String>>,
    /// 额外请求字段 schema（含 thinking/effort 配置），None 表示不支持扩展参数
    pub additional_model_request_fields_schema: Option<serde_json::Value>,
}

impl AvailableModelEntry {
    /// 是否支持 thinking 参数（schema 中包含 thinking 字段）
    pub fn supports_thinking(&self) -> bool {
        self.additional_model_request_fields_schema
            .as_ref()
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.get("thinking"))
            .is_some()
    }
}

/// Token 限制
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenLimits {
    pub max_output_tokens: Option<i32>,
    pub max_input_tokens: Option<i32>,
}

/// Prompt 缓存信息
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCachingInfo {
    pub supports_prompt_caching: Option<bool>,
    pub minimum_tokens_per_cache_checkpoint: Option<i32>,
    pub maximum_cache_checkpoints_per_request: Option<i32>,
}
