//! Kiro CLI 端点实现

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::{KiroEndpoint, RequestContext, UsageRequestParts};
use crate::kiro::model::credentials::KiroCredentials;

pub const CLI_ENDPOINT_NAME: &str = "cli";
const CLI_ORIGIN: &str = "KIRO_CLI";
// 2026-11-10 起上游对旧 target `AmazonQDeveloperStreamingService.SendMessage` 一律返回
// 403 FEATURE_NOT_SUPPORTED（与版本号无关）；官方 CLI 3.x 已改用下面的 target，
// 该 target 要求请求体带 profileArn（IdC 账号经 ListAvailableProfiles 获取）。
const CLI_API_TARGET: &str = "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";
// MCP 同理：须带此 target，profileArn 走请求头 x-amzn-kiro-profile-arn（放请求体无效）
const CLI_MCP_TARGET: &str = "AmazonCodeWhispererStreamingService.InvokeMCP";
const CLI_STREAMING_API_VERSION: &str = "0.1.14474";
const CLI_RUNTIME_API_VERSION: &str = "0.1.14474";
const CLI_RUST_SDK_VERSION: &str = "1.3.14";
const CLI_RUST_VERSION: &str = "1.92.0";
// 2.x 最新版。appVersion-3.x 会被判为不支持（V3 客户端另有标识，未模拟），
// 2027-01-15 起上游将拒绝 ≤ 2.5.0。
const CLI_APP_VERSION: &str = "2.37.1";

pub struct CliEndpoint;

impl CliEndpoint {
    pub fn new() -> Self {
        Self
    }

    fn host(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "q.{}.amazonaws.com",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn api_origin(&self) -> &'static str {
        CLI_ORIGIN
    }

    fn streaming_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererstreaming/{api} os/linux lang/rust/{rust} md/appVersion-{app} app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_STREAMING_API_VERSION,
            rust = CLI_RUST_VERSION,
            app = CLI_APP_VERSION,
        )
    }

    fn streaming_x_amz_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererstreaming/{api} os/linux lang/rust/{rust} m/F app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_STREAMING_API_VERSION,
            rust = CLI_RUST_VERSION,
        )
    }

    fn runtime_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererruntime/{api} os/linux lang/rust/{rust} md/appVersion-{app} app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_RUNTIME_API_VERSION,
            rust = CLI_RUST_VERSION,
            app = CLI_APP_VERSION,
        )
    }

    fn runtime_x_amz_user_agent(&self) -> String {
        format!(
            "aws-sdk-rust/{sdk} ua/2.1 api/codewhispererruntime/{api} os/linux lang/rust/{rust} m/F app/AmazonQ-For-CLI",
            sdk = CLI_RUST_SDK_VERSION,
            api = CLI_RUNTIME_API_VERSION,
            rust = CLI_RUST_VERSION,
        )
    }

    /// 把消息来源改为 CLI：只改 `userInputMessage.origin`（当前消息与历史）
    ///
    /// 不能递归改所有名为 `origin` 的键：工具 schema / 工具调用参数里也可能有
    /// `origin` 字段（如地图类 MCP 工具的 origin/destination），会被误改坏。
    fn rewrite_origin(value: &mut serde_json::Value) {
        let Some(state) = value.get_mut("conversationState") else {
            return;
        };
        let cli_origin = || serde_json::Value::String(CLI_ORIGIN.to_string());
        if let Some(origin) = state.pointer_mut("/currentMessage/userInputMessage/origin") {
            *origin = cli_origin();
        }
        if let Some(history) = state.get_mut("history").and_then(|h| h.as_array_mut()) {
            for item in history {
                if let Some(origin) = item.pointer_mut("/userInputMessage/origin") {
                    *origin = cli_origin();
                }
            }
        }
    }

    fn transform_body(&self, body: &str) -> anyhow::Result<String> {
        let mut request: serde_json::Value = serde_json::from_str(body)?;
        Self::rewrite_origin(&mut request);
        Ok(serde_json::to_string(&request)?)
    }

    /// 写入当前凭据自己的 profileArn（先移除请求体里可能来自其他凭据的值）
    fn transform_api_request(
        &self,
        body: &str,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<String> {
        let mut request: serde_json::Value = serde_json::from_str(body)?;
        Self::rewrite_origin(&mut request);
        if let Some(obj) = request.as_object_mut() {
            obj.remove("profileArn");
            if let Some(arn) = credentials.profile_arn.as_deref() {
                obj.insert(
                    "profileArn".to_string(),
                    serde_json::Value::String(arn.to_string()),
                );
            }
        }
        Ok(serde_json::to_string(&request)?)
    }
}

impl Default for CliEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for CliEndpoint {
    fn name(&self) -> &'static str {
        CLI_ENDPOINT_NAME
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "https://q.{}.amazonaws.com/generateAssistantResponse",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "https://q.{}.amazonaws.com/mcp",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("Accept", "*/*")
            .header("X-Amz-Target", CLI_API_TARGET)
            .header("x-amzn-codewhisperer-optout", "false")
            .header("x-amzn-kiro-agent-mode", "cli")
            .header("x-amz-user-agent", self.streaming_x_amz_user_agent())
            .header("user-agent", self.streaming_user_agent())
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        req
    }

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("X-Amz-Target", CLI_MCP_TARGET)
            .header("x-amz-user-agent", self.streaming_x_amz_user_agent())
            .header("user-agent", self.streaming_user_agent())
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if let Some(arn) = ctx.credentials.profile_arn.as_deref() {
            req = req.header("x-amzn-kiro-profile-arn", arn);
        }
        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        req
    }

    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        self.transform_api_request(body, ctx.credentials)
    }

    fn transform_mcp_body(&self, body: &str, _ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        self.transform_body(body)
    }

    fn usage_request_parts(&self, ctx: &RequestContext<'_>) -> anyhow::Result<UsageRequestParts> {
        let host = self.host(ctx);
        let url = format!(
            "https://{}/getUsageLimits?origin={}&resourceType=AGENTIC_REQUEST",
            host,
            self.api_origin()
        );

        let mut headers = vec![
            ("Accept", "application/json".to_string()),
            ("x-amz-user-agent", self.runtime_x_amz_user_agent()),
            ("user-agent", self.runtime_user_agent()),
            ("host", host),
            ("amz-sdk-invocation-id", Uuid::new_v4().to_string()),
            ("amz-sdk-request", "attempt=1; max=1".to_string()),
            ("Authorization", format!("Bearer {}", ctx.token)),
        ];

        if ctx.credentials.is_api_key_credential() {
            headers.push(("tokentype", "API_KEY".to_string()));
        }

        Ok(UsageRequestParts { url, headers })
    }
}

#[cfg(test)]
mod tests {
    use super::CliEndpoint;

    #[test]
    fn test_rewrite_origin_only_touches_message_origin() {
        let mut v = serde_json::json!({
            "conversationState": {
                "currentMessage": {"userInputMessage": {
                    "origin": "AI_EDITOR",
                    "userInputMessageContext": {"tools": [{"toolSpecification": {
                        "name": "directions",
                        "inputSchema": {"json": {"properties": {"origin": {"type": "string"}}}}
                    }}]}
                }},
                "history": [
                    {"userInputMessage": {"origin": "AI_EDITOR", "content": "x"}},
                    {"assistantResponseMessage": {"content": "",
                        "toolUses": [{"toolUseId": "t", "name": "directions", "input": {"origin": "Paris"}}]}}
                ]
            }
        });
        CliEndpoint::rewrite_origin(&mut v);
        let cs = &v["conversationState"];
        assert_eq!(
            cs["currentMessage"]["userInputMessage"]["origin"],
            "KIRO_CLI"
        );
        assert_eq!(cs["history"][0]["userInputMessage"]["origin"], "KIRO_CLI");
        // 工具 schema 与工具调用参数里的 origin 字段不能被改
        assert_eq!(
            cs["currentMessage"]["userInputMessage"]["userInputMessageContext"]["tools"][0]["toolSpecification"]
                ["inputSchema"]["json"]["properties"]["origin"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            cs["history"][1]["assistantResponseMessage"]["toolUses"][0]["input"]["origin"],
            "Paris"
        );
    }
}
