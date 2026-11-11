//! Kiro CLI 3.x 端点（KAS 引擎）
//!
//! 官方 CLI 3.x 的对话由内嵌的 Node 引擎 KAS（`@kiro/agent`）发出，走新服务
//! KiroRuntimeService：`https://runtime.{region}.kiro.dev/`，awsJson 1.0
//! （`X-Amz-Target: KiroRuntimeService.*`，Content-Type 必须是 `application/x-amz-json-1.0`）。
//! 响应仍是 AWS event stream（开头的 `initial-response` 被忽略；`metadataEvent` 携带停止原因 / 拒答详情）。
//! API 与 MCP 都要求请求体带 profileArn。余额 / 模型列表仍走 `q.{region}` 的旧接口。

use reqwest::RequestBuilder;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use uuid::Uuid;

use super::{CliEndpoint, KiroEndpoint, RequestContext, UsageRequestParts};
use crate::kiro::model::credentials::KiroCredentials;

pub const CLI3_ENDPOINT_NAME: &str = "cli3";
const CLI3_ORIGIN: &str = "KIRO_CLI";
const KRS_API_TARGET: &str = "KiroRuntimeService.GenerateAssistantResponse";
const KRS_MCP_TARGET: &str = "KiroRuntimeService.InvokeMCP";
const AWS_JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.0";
// 取自官方 kiro-cli 3.2.0（BUILD 2026-11-10，内嵌 @kiro/agent 0.66.95）
const CLI3_APP_VERSION: &str = "3.2.0";
const KAS_VERSION: &str = "0.66.95";
/// KiroRuntimeService 客户端 `@amzn/kiro-runtime-service-typescript-client` 的版本：
/// smithy 生成的 JS 客户端在 UA 里用它填 `aws-sdk-js/{v}` 与 `api/kiroruntime#{v}`
const KRS_CLIENT_VERSION: &str = "1.0.0";
/// 余额 / 模型列表仍走 q 端点的旧接口，沿用原 UA 的 SDK 版本
const Q_JS_SDK_VERSION: &str = "3.1031.0";
/// UA 的 SDK 部分的 OS 段：KAS 运行在 Linux 上（与客户端部分的 `os/linux` 一致，
/// 不再用 config.system_version，避免出现 win32 / linux 自相矛盾的 UA）
const SDK_OS: &str = "linux#6.8.0";
/// KRS 请求的 agentMode（Smithy 模型绑定到 `x-amzn-kiro-agent-mode` 头），官方 CLI 为 vibe
const KRS_AGENT_MODE: &str = "vibe";

pub struct Cli3Endpoint;

impl Cli3Endpoint {
    pub fn new() -> Self {
        Self
    }

    fn runtime_host(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "runtime.{}.kiro.dev",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn q_host(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "q.{}.amazonaws.com",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    /// KAS 追加在 SDK UA 之后的客户端标识（与官方 buildClientUserAgent 输出一致）
    /// 宿主（Rust CLI，acp_engine.rs）传给 KAS 的客户端标识，3.2.0 起末尾带 `app/AmazonQ-For-CLI`
    fn client_user_agent() -> String {
        format!(
            "KiroCLI/{app} KAS/{kas} os/linux md/appVersion-{app} app/AmazonQ-For-CLI",
            app = CLI3_APP_VERSION,
            kas = KAS_VERSION,
        )
    }

    fn user_agent(&self, ctx: &RequestContext<'_>, api: &str, sdk_version: &str) -> String {
        format!(
            "aws-sdk-js/{sdk} ua/2.1 os/{os} lang/js md/nodejs#{node} api/{api}#{sdk} m/E,N {client}",
            sdk = sdk_version,
            os = SDK_OS,
            node = ctx.config.node_version,
            api = api,
            client = Self::client_user_agent(),
        )
    }

    fn x_amz_user_agent(&self, sdk_version: &str) -> String {
        format!("aws-sdk-js/{} {}", sdk_version, Self::client_user_agent())
    }

    /// KAS 的 TokenType 头：IdC → SSO_OIDC，API Key → API_KEY，Social 不带
    fn token_type(credentials: &KiroCredentials) -> Option<&'static str> {
        if credentials.is_api_key_credential() {
            return Some("API_KEY");
        }
        let oidc = matches!(
            credentials.auth_method.as_deref(),
            Some("builder-id") | Some("idc")
        ) || (credentials.client_id.is_some() && credentials.client_secret.is_some());
        oidc.then_some("SSO_OIDC")
    }

    fn decorate_runtime(
        &self,
        req: RequestBuilder,
        ctx: &RequestContext<'_>,
        target: &'static str,
    ) -> RequestBuilder {
        // provider 已设置 content-type: application/json；reqwest 的 header() 是追加，
        // 这里用 headers() 覆盖（KRS 只认 awsJson，application/json 会被当成未知操作）
        let mut content_type = HeaderMap::new();
        content_type.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(AWS_JSON_CONTENT_TYPE),
        );

        let mut req = req
            .headers(content_type)
            .header("Accept", "*/*")
            .header("X-Amz-Target", target)
            .header(
                "x-amz-user-agent",
                self.x_amz_user_agent(KRS_CLIENT_VERSION),
            )
            .header(
                "user-agent",
                self.user_agent(ctx, "kiroruntime", KRS_CLIENT_VERSION),
            )
            .header("x-amzn-kiro-agent-mode", KRS_AGENT_MODE)
            .header("host", self.runtime_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if let Some(token_type) = Self::token_type(ctx.credentials) {
            req = req.header("TokenType", token_type);
        }
        req
    }
}

impl Default for Cli3Endpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for Cli3Endpoint {
    fn name(&self) -> &'static str {
        CLI3_ENDPOINT_NAME
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("https://{}/", self.runtime_host(ctx))
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("https://{}/", self.runtime_host(ctx))
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        self.decorate_runtime(req, ctx, KRS_API_TARGET)
    }

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        self.decorate_runtime(req, ctx, KRS_MCP_TARGET)
    }

    /// 请求体与 cli 端点相同：origin 改写为 KIRO_CLI，并写入当前凭据的 profileArn
    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        CliEndpoint::new().transform_api_body(body, ctx)
    }

    /// KRS 的 InvokeMCP 只认请求体里的 profileArn（x-amzn-kiro-profile-arn 头无效）
    fn transform_mcp_body(&self, body: &str, ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        let mut request: serde_json::Value = serde_json::from_str(body)?;
        if let Some(obj) = request.as_object_mut() {
            obj.remove("profileArn");
            if let Some(arn) = ctx.credentials.profile_arn.as_deref() {
                obj.insert(
                    "profileArn".to_string(),
                    serde_json::Value::String(arn.to_string()),
                );
            }
        }
        Ok(serde_json::to_string(&request)?)
    }

    fn usage_request_parts(&self, ctx: &RequestContext<'_>) -> anyhow::Result<UsageRequestParts> {
        let host = self.q_host(ctx);
        let url = format!(
            "https://{}/getUsageLimits?origin={}&resourceType=AGENTIC_REQUEST",
            host, CLI3_ORIGIN
        );

        let mut headers = vec![
            ("Accept", "application/json".to_string()),
            ("x-amz-user-agent", self.x_amz_user_agent(Q_JS_SDK_VERSION)),
            (
                "user-agent",
                self.user_agent(ctx, "codewhispererruntime", Q_JS_SDK_VERSION),
            ),
            ("host", host),
            ("amz-sdk-invocation-id", Uuid::new_v4().to_string()),
            ("amz-sdk-request", "attempt=1; max=1".to_string()),
            ("Authorization", format!("Bearer {}", ctx.token)),
        ];
        if let Some(token_type) = Self::token_type(ctx.credentials) {
            headers.push(("TokenType", token_type.to_string()));
        }

        Ok(UsageRequestParts { url, headers })
    }
}
