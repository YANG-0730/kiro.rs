//! HTTP Client 构建模块
//!
//! 提供统一的 HTTP Client 构建功能，支持代理配置

use reqwest::{Client, Proxy};
use std::time::Duration;

use crate::model::config::TlsBackend;

/// 代理配置
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ProxyConfig {
    /// 代理地址，支持 http/https/socks5
    pub url: String,
    /// 代理认证用户名
    pub username: Option<String>,
    /// 代理认证密码
    pub password: Option<String>,
}

impl ProxyConfig {
    /// 从 url 创建代理配置
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            username: None,
            password: None,
        }
    }

    /// 设置认证信息
    pub fn with_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }
}

/// 构建 HTTP Client
///
/// # Arguments
/// * `proxy` - 可选的代理配置
/// * `timeout_secs` - 超时时间（秒）
///
/// # Returns
/// 配置好的 reqwest::Client
pub fn build_client(
    proxy: Option<&ProxyConfig>,
    timeout_secs: u64,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    let builder = Client::builder().timeout(Duration::from_secs(timeout_secs));
    finish_client(builder, proxy, tls_backend)
}

/// 流式 API 请求的连接超时（与官方 Kiro CLI 3.x 的 connectionTimeout 一致）
const API_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 流式 API 请求的读空闲超时（与官方 Kiro CLI 3.x 的 requestTimeout 一致，为 socket 空闲超时）
const API_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// 构建用于对话 API（流式响应）的 HTTP Client
///
/// 不设整体超时：reqwest 的 `timeout` 是含响应体读取的总时限，长回复（大段 Write、
/// 高 effort 推理）超过时限会被从中间截断。改为连接超时 + 读空闲超时，只要上游
/// 持续有数据就不会断。
pub fn build_api_client(
    proxy: Option<&ProxyConfig>,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    let builder = Client::builder()
        .connect_timeout(API_CONNECT_TIMEOUT)
        .read_timeout(API_READ_IDLE_TIMEOUT);
    finish_client(builder, proxy, tls_backend)
}

fn finish_client(
    mut builder: reqwest::ClientBuilder,
    proxy: Option<&ProxyConfig>,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    if tls_backend == TlsBackend::Rustls {
        builder = builder.use_rustls_tls();
    }

    if let Some(proxy_config) = proxy {
        let mut proxy = Proxy::all(&proxy_config.url)?;

        // 设置代理认证
        if let (Some(username), Some(password)) = (&proxy_config.username, &proxy_config.password) {
            proxy = proxy.basic_auth(username, password);
        }

        builder = builder.proxy(proxy);
        tracing::debug!("HTTP Client 使用代理: {}", proxy_config.url);
    }

    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_config_new() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        assert_eq!(config.url, "http://127.0.0.1:7890");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }

    #[test]
    fn test_proxy_config_with_auth() {
        let config = ProxyConfig::new("socks5://127.0.0.1:1080").with_auth("user", "pass");
        assert_eq!(config.url, "socks5://127.0.0.1:1080");
        assert_eq!(config.username, Some("user".to_string()));
        assert_eq!(config.password, Some("pass".to_string()));
    }

    #[test]
    fn test_build_client_without_proxy() {
        let client = build_client(None, 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn test_build_api_client() {
        assert!(build_api_client(None, TlsBackend::Rustls).is_ok());
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        assert!(build_api_client(Some(&config), TlsBackend::Rustls).is_ok());
    }

    #[test]
    fn test_build_client_with_proxy() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        let client = build_client(Some(&config), 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }
}
