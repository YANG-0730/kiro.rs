//! 模型注册表：缓存每个凭据的可用模型列表

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use parking_lot::RwLock;

use super::model::available_models::AvailableModelEntry;

struct CredentialModelCache {
    models: Vec<AvailableModelEntry>,
    /// 原始上游 modelId 集合，用于精确匹配（[`ModelRegistry::credentials_supporting`]）
    model_ids: HashSet<String>,
    /// 客户端名查询索引：小写 modelId 与「点号转横杠」形式都映射回原始 modelId
    /// （写入时一次性预计算，查询时单次 HashMap lookup，避免热路径上反复 lowercase/replace）
    lookup: HashMap<String, String>,
    #[allow(dead_code)]
    fetched_at: Instant,
}

impl CredentialModelCache {
    fn new(models: Vec<AvailableModelEntry>) -> Self {
        let mut model_ids = HashSet::with_capacity(models.len());
        let mut lookup = HashMap::with_capacity(models.len() * 2);
        for m in &models {
            model_ids.insert(m.model_id.clone());
            let id_lower = m.model_id.to_lowercase();
            let dashed = id_lower.replace('.', "-");
            // 后写入的不覆盖：同一凭据内首次出现的 modelId 即为权威值
            lookup
                .entry(id_lower.clone())
                .or_insert_with(|| m.model_id.clone());
            if dashed != id_lower {
                lookup.entry(dashed).or_insert_with(|| m.model_id.clone());
            }
        }
        Self {
            models,
            model_ids,
            lookup,
            fetched_at: Instant::now(),
        }
    }
}

pub struct ModelRegistry {
    caches: RwLock<HashMap<u64, CredentialModelCache>>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            caches: RwLock::new(HashMap::new()),
        }
    }

    pub fn update_credential(&self, credential_id: u64, models: Vec<AvailableModelEntry>) {
        self.caches
            .write()
            .insert(credential_id, CredentialModelCache::new(models));
    }

    pub fn get_union_models(&self) -> Vec<AvailableModelEntry> {
        let caches = self.caches.read();
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for cache in caches.values() {
            for model in &cache.models {
                if seen.insert(model.model_id.clone()) {
                    result.push(model.clone());
                }
            }
        }
        result
    }

    pub fn credentials_supporting(&self, kiro_model_id: &str) -> Vec<u64> {
        let caches = self.caches.read();
        caches
            .iter()
            .filter(|(_, cache)| cache.model_ids.contains(kiro_model_id))
            .map(|(id, _)| *id)
            .collect()
    }

    /// 将客户端模型名解析为上游真实 modelId
    ///
    /// 权威来源是上游返回的 modelId 集合。客户端可能用横杠版本号
    /// （Anthropic 惯例 `claude-opus-4-8`）或点号（`deepseek-3.2`），
    /// 写入 registry 时已预计算好「小写原样」与「点号转横杠」两种索引键，
    /// 查询走单次 HashMap lookup，命中即返回上游原始 modelId。
    ///
    /// 若直接匹配不上，再剥掉末尾的日期后缀（Anthropic 惯例
    /// `claude-haiku-4-5-20251001` → `claude-haiku-4-5`）重试一次：
    /// 这类带日期名是客户端常发的合法名（尤其 haiku 用于后台轻任务），
    /// 对应上游的无日期点号 modelId（`claude-haiku-4.5`）。
    ///
    /// 仍匹配不上则返回 None（上游确实没有的模型，如更早的过时版本）。
    pub fn resolve_client_model(&self, client_model: &str) -> Option<String> {
        let needle = client_model.to_lowercase();
        let caches = self.caches.read();

        let lookup = |key: &str| -> Option<String> {
            for cache in caches.values() {
                if let Some(id) = cache.lookup.get(key) {
                    return Some(id.clone());
                }
            }
            None
        };

        if let Some(hit) = lookup(&needle) {
            return Some(hit);
        }
        // 剥掉末尾日期后缀后重试（仅在直接匹配失败时才计算）
        if let Some(stripped) = strip_date_suffix(&needle) {
            return lookup(stripped);
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.caches.read().is_empty()
    }

    /// 按客户端模型名查上游模型的 maxOutputTokens
    ///
    /// 先解析为上游 modelId，再在缓存中找到对应条目的 token_limits。
    /// 用于日志展示「上游模型支持的输出上限」（与客户端请求的 max_tokens 区分）。
    pub fn max_output_tokens_for(&self, client_model: &str) -> Option<i32> {
        let model_id = self.resolve_client_model(client_model)?;
        let caches = self.caches.read();
        for cache in caches.values() {
            for entry in &cache.models {
                if entry.model_id == model_id {
                    return entry.token_limits.as_ref().and_then(|t| t.max_output_tokens);
                }
            }
        }
        None
    }
}

/// 剥掉 Anthropic 模型名末尾的日期后缀
///
/// `claude-haiku-4-5-20251001` → `claude-haiku-4-5`。
/// 仅当末尾是 `-` + 6~8 位纯数字时才剥离（日期格式如 20251001），
/// 避免误伤版本号末段（如 `claude-opus-4-8` 的 `-8`）。
/// 不含日期后缀时返回 None。
fn strip_date_suffix(model: &str) -> Option<&str> {
    let last_dash = model.rfind('-')?;
    let suffix = &model[last_dash + 1..];
    if (6..=8).contains(&suffix.len()) && suffix.bytes().all(|b| b.is_ascii_digit()) {
        Some(&model[..last_dash])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_date_suffix() {
        // 8 位日期被剥离
        assert_eq!(
            strip_date_suffix("claude-haiku-4-5-20251001"),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            strip_date_suffix("claude-sonnet-4-5-20250929"),
            Some("claude-sonnet-4-5")
        );
        // 版本号末段（1 位）不被误剥
        assert_eq!(strip_date_suffix("claude-opus-4-8"), None);
        assert_eq!(strip_date_suffix("claude-opus-4.8"), None);
        // 非数字后缀不剥
        assert_eq!(strip_date_suffix("deepseek-3.2"), None);
        assert_eq!(strip_date_suffix("glm-5"), None);
    }

    #[test]
    fn test_resolve_with_date_suffix() {
        let reg = ModelRegistry::new();
        reg.update_credential(
            1,
            vec![AvailableModelEntry {
                model_id: "claude-haiku-4.5".to_string(),
                model_name: None,
                description: None,
                rate_multiplier: None,
                rate_unit: None,
                token_limits: None,
                supported_input_types: None,
                prompt_caching: None,
                model_provider: None,
                status: None,
                available_origins: None,
                additional_model_request_fields_schema: None,
            }],
        );
        // 带日期 → 剥日期 → 点号转横杠反查命中
        assert_eq!(
            reg.resolve_client_model("claude-haiku-4-5-20251001"),
            Some("claude-haiku-4.5".to_string())
        );
        // 无日期的横杠名也命中
        assert_eq!(
            reg.resolve_client_model("claude-haiku-4-5"),
            Some("claude-haiku-4.5".to_string())
        );
        // 剥日期后版本不存在 → None
        assert_eq!(reg.resolve_client_model("claude-opus-4-9-20260101"), None);
    }

    /// 写入 registry 时索引应该包含「小写原样」与「点号转横杠」两种键
    /// 形式（白盒测试，防止未来重构 CredentialModelCache::new 时丢掉某条索引）
    #[test]
    fn test_lookup_index_prebuilt_on_insert() {
        fn entry(id: &str) -> AvailableModelEntry {
            AvailableModelEntry {
                model_id: id.to_string(),
                model_name: None,
                description: None,
                rate_multiplier: None,
                rate_unit: None,
                token_limits: None,
                supported_input_types: None,
                prompt_caching: None,
                model_provider: None,
                status: None,
                available_origins: None,
                additional_model_request_fields_schema: None,
            }
        }

        let cache = CredentialModelCache::new(vec![
            entry("claude-opus-4.8"),  // 含点号：插入两条索引（点号、横杠）
            entry("Claude-Haiku-4.5"), // 大写：键归一化为小写
            entry("glm-5"),            // 无点号：单条索引
        ]);

        // 含点号的 id：lookup 同时支持点号原样与横杠形
        assert_eq!(
            cache.lookup.get("claude-opus-4.8").map(String::as_str),
            Some("claude-opus-4.8")
        );
        assert_eq!(
            cache.lookup.get("claude-opus-4-8").map(String::as_str),
            Some("claude-opus-4.8")
        );

        // 大写 id：键全部小写化，但 value 保留原始大小写
        assert_eq!(
            cache.lookup.get("claude-haiku-4.5").map(String::as_str),
            Some("Claude-Haiku-4.5")
        );
        assert_eq!(
            cache.lookup.get("claude-haiku-4-5").map(String::as_str),
            Some("Claude-Haiku-4.5")
        );

        // 不含点号的 id：仅一条索引，不重复 insert
        assert_eq!(cache.lookup.get("glm-5").map(String::as_str), Some("glm-5"));

        // 精确匹配集合保留原始大小写
        assert!(cache.model_ids.contains("claude-opus-4.8"));
        assert!(cache.model_ids.contains("Claude-Haiku-4.5"));
    }
}
