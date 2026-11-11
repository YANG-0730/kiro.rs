//! Kiro 端点抽象
//!
//! 不同 Kiro 端点在 URL、请求头、请求体上存在差异，
//! 但共享凭据池、Token 刷新、重试和响应解码逻辑。

use reqwest::RequestBuilder;

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

pub mod cli;
pub mod cli3;
pub mod ide;

pub use cli::{CLI_ENDPOINT_NAME, CliEndpoint};
pub use cli3::{CLI3_ENDPOINT_NAME, Cli3Endpoint};
pub use ide::{IDE_ENDPOINT_NAME, IdeEndpoint};

pub struct UsageRequestParts {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
}

/// Kiro 控制面服务（KiroControlPlaneBearer，`management.{region}.kiro.dev`，REST-JSON）
///
/// 官方 CLI 3.x 的余额（`GET /Get-Usage-Limits`）与模型列表（`GET /List-Available-Models`）
/// 走这里，参数都在 query 中。
pub struct ControlPlaneParts {
    /// 形如 `https://management.us-east-1.kiro.dev`
    pub base_url: String,
    /// 请求的 origin（官方 CLI 为 KIRO_CLI）
    pub origin: &'static str,
    pub headers: Vec<(&'static str, String)>,
}

pub trait KiroEndpoint: Send + Sync {
    fn name(&self) -> &'static str;

    fn api_url(&self, ctx: &RequestContext<'_>) -> String;

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String;

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> anyhow::Result<String>;

    fn transform_mcp_body(&self, body: &str, _ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        Ok(body.to_string())
    }

    fn usage_request_parts(&self, ctx: &RequestContext<'_>) -> anyhow::Result<UsageRequestParts>;

    /// 控制面服务（余额 / 模型列表）；None 表示只走 q 端点的旧接口
    fn control_plane_parts(&self, _ctx: &RequestContext<'_>) -> Option<ControlPlaneParts> {
        None
    }

    fn is_monthly_request_limit(&self, body: &str) -> bool {
        default_is_monthly_request_limit(body)
    }

    fn is_bearer_token_invalid(&self, body: &str) -> bool {
        default_is_bearer_token_invalid(body)
    }
}

pub struct RequestContext<'a> {
    pub credentials: &'a KiroCredentials,
    pub token: &'a str,
    pub machine_id: &'a str,
    pub config: &'a Config,
}

pub fn default_is_monthly_request_limit(body: &str) -> bool {
    if body.contains("MONTHLY_REQUEST_COUNT") {
        return true;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };

    if value
        .get("reason")
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == "MONTHLY_REQUEST_COUNT")
    {
        return true;
    }

    value
        .pointer("/error/reason")
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == "MONTHLY_REQUEST_COUNT")
}

pub fn default_is_bearer_token_invalid(body: &str) -> bool {
    body.contains("The bearer token included in the request is invalid")
}
