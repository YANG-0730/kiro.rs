//! 使用额度查询数据模型
//!
//! 包含 getUsageLimits API 的响应类型定义

use serde::Deserialize;

/// 使用额度查询响应
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsResponse {
    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub next_date_reset: Option<f64>,

    /// 订阅信息
    #[serde(default)]
    pub subscription_info: Option<SubscriptionInfo>,

    /// 超额计费配置（账号级）
    #[serde(default)]
    pub overage_configuration: Option<OverageConfiguration>,

    /// 使用量明细列表
    #[serde(default)]
    pub usage_breakdown_list: Vec<UsageBreakdown>,
}

/// 超额计费配置
///
/// 当基础额度用尽后，若 `overageStatus == "ENABLED"`，账号仍可继续消费
/// （按 overage 计费），直到达到 `overageLimit`（null 表示无上限）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverageConfiguration {
    /// 超额上限（null 表示无上限）
    #[serde(default)]
    #[allow(dead_code)]
    pub overage_limit: Option<f64>,

    /// 超额状态 (ENABLED / DISABLED)
    #[serde(default)]
    pub overage_status: Option<String>,
}

impl OverageConfiguration {
    /// 超额计费是否已启用（大小写不敏感）
    pub fn is_enabled(&self) -> bool {
        self.overage_status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("ENABLED"))
            .unwrap_or(false)
    }
}

/// 订阅信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    /// 订阅标题 (KIRO PRO+ / KIRO FREE 等)
    #[serde(default)]
    pub subscription_title: Option<String>,
}

/// 使用量明细
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBreakdown {
    /// 当前使用量
    #[serde(default)]
    #[allow(dead_code)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 奖励额度列表（可能为 null）
    #[serde(default)]
    pub bonuses: Option<Vec<Bonus>>,

    /// 免费试用信息
    #[serde(default)]
    pub free_trial_info: Option<FreeTrialInfo>,

    /// 下次重置日期 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub next_date_reset: Option<f64>,

    /// 使用限额
    #[serde(default)]
    #[allow(dead_code)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,

    /// 当前超额使用量（精确值）
    #[serde(default)]
    pub current_overages_with_precision: f64,

    /// 超额上限（精确值，0 表示未提供/无上限）
    #[serde(default)]
    pub overage_cap_with_precision: f64,
}

/// 奖励额度
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bonus {
    /// 当前使用量
    #[serde(default)]
    pub current_usage: f64,

    /// 使用限额
    #[serde(default)]
    pub usage_limit: f64,

    /// 状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub status: Option<String>,
}

impl Bonus {
    /// 检查 bonus 是否处于激活状态（大小写不敏感）
    pub fn is_active(&self) -> bool {
        self.status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("ACTIVE"))
            .unwrap_or(false)
    }
}

/// 免费试用信息
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeTrialInfo {
    /// 当前使用量
    #[serde(default)]
    #[allow(dead_code)]
    pub current_usage: i64,

    /// 当前使用量（精确值）
    #[serde(default)]
    pub current_usage_with_precision: f64,

    /// 免费试用过期时间 (Unix 时间戳)
    #[serde(default)]
    #[allow(dead_code)]
    pub free_trial_expiry: Option<f64>,

    /// 免费试用状态 (ACTIVE / EXPIRED)
    #[serde(default)]
    pub free_trial_status: Option<String>,

    /// 使用限额
    #[serde(default)]
    #[allow(dead_code)]
    pub usage_limit: i64,

    /// 使用限额（精确值）
    #[serde(default)]
    pub usage_limit_with_precision: f64,
}

// ============ 便捷方法实现 ============

impl FreeTrialInfo {
    /// 检查免费试用是否处于激活状态
    pub fn is_active(&self) -> bool {
        self.free_trial_status
            .as_deref()
            .map(|s| s == "ACTIVE")
            .unwrap_or(false)
    }
}

impl UsageLimitsResponse {
    /// 获取订阅标题
    pub fn subscription_title(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|info| info.subscription_title.as_deref())
    }

    /// 获取第一个使用量明细
    fn primary_breakdown(&self) -> Option<&UsageBreakdown> {
        self.usage_breakdown_list.first()
    }

    /// 账号是否有可用的超额（overage）额度
    ///
    /// KIRO PRO 等账号在基础额度用尽后，若开启超额计费（`overageStatus == ENABLED`），
    /// 仍可继续按超额计费消费，直到达到 `overageCap`。此时不应判定为"余额不足"而禁用。
    ///
    /// 判定条件：账号级 `overageConfiguration` 启用，且主 breakdown 的超额未达上限
    /// （`overageCap <= 0` 视为未提供上限，只要启用就认为可用）。
    pub fn has_available_overage(&self) -> bool {
        let overage_enabled = self
            .overage_configuration
            .as_ref()
            .map(|c| c.is_enabled())
            .unwrap_or(false);
        if !overage_enabled {
            return false;
        }

        // 启用超额后，检查是否还有超额余量
        match self.primary_breakdown() {
            Some(b) => {
                let cap = b.overage_cap_with_precision;
                // cap <= 0 视为无上限（或上游未提供），只要启用即认为可用
                cap <= 0.0 || b.current_overages_with_precision < cap
            }
            // 无 breakdown 但账号启用了超额：保守认为可用
            None => true,
        }
    }

    /// 判断凭据是否真正额度耗尽（应被禁用）
    ///
    /// 基础剩余额度低于 `threshold`，**且**没有可用的超额额度时，才视为耗尽。
    /// 开启超额且未达上限的账号即使基础余额为 0，也不算耗尽。
    pub fn is_exhausted(&self, threshold: f64) -> bool {
        let remaining = (self.usage_limit() - self.current_usage()).max(0.0);
        remaining < threshold && !self.has_available_overage()
    }

    /// 获取总使用限额（精确值）
    ///
    /// 累加基础额度、激活的免费试用额度和激活的奖励额度
    #[allow(clippy::collapsible_if)]
    pub fn usage_limit(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.usage_limit_with_precision;

        // 累加激活的 free trial 额度
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.usage_limit_with_precision;
            }
        }

        // 累加激活的 bonus 额度
        if let Some(bonuses) = &breakdown.bonuses {
            for bonus in bonuses {
                if bonus.is_active() {
                    total += bonus.usage_limit;
                }
            }
        }

        total
    }

    /// 获取总当前使用量（精确值）
    ///
    /// 累加基础使用量、激活的免费试用使用量和激活的奖励使用量
    #[allow(clippy::collapsible_if)]
    pub fn current_usage(&self) -> f64 {
        let Some(breakdown) = self.primary_breakdown() else {
            return 0.0;
        };

        let mut total = breakdown.current_usage_with_precision;

        // 累加激活的 free trial 使用量
        if let Some(trial) = &breakdown.free_trial_info {
            if trial.is_active() {
                total += trial.current_usage_with_precision;
            }
        }

        // 累加激活的 bonus 使用量
        if let Some(bonuses) = &breakdown.bonuses {
            for bonus in bonuses {
                if bonus.is_active() {
                    total += bonus.current_usage;
                }
            }
        }

        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个模拟 getUsageLimits 响应的 JSON
    fn resp(json: serde_json::Value) -> UsageLimitsResponse {
        serde_json::from_value(json).unwrap()
    }

    /// 基础额度用尽 + 超额启用且未达上限 → 不判定为耗尽（对应真实 KIRO PRO overage 账号）
    #[test]
    fn test_overage_enabled_not_exhausted() {
        let r = resp(serde_json::json!({
            "overageConfiguration": {"overageLimit": null, "overageStatus": "ENABLED"},
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 1789.93,
                "usageLimitWithPrecision": 1000.0,
                "currentOveragesWithPrecision": 789.93,
                "overageCapWithPrecision": 10000.0
            }]
        }));
        assert!(r.has_available_overage(), "超额启用且未达上限应可用");
        assert!(!r.is_exhausted(1.0), "有可用超额时不应判定为耗尽");
    }

    /// 基础额度用尽 + 超额未启用 → 判定为耗尽
    #[test]
    fn test_overage_disabled_exhausted() {
        let r = resp(serde_json::json!({
            "overageConfiguration": {"overageStatus": "DISABLED"},
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 1000.0,
                "usageLimitWithPrecision": 1000.0
            }]
        }));
        assert!(!r.has_available_overage());
        assert!(r.is_exhausted(1.0), "无超额且余额为 0 应判定为耗尽");
    }

    /// 超额启用但已达上限 → 判定为耗尽
    #[test]
    fn test_overage_reached_cap_exhausted() {
        let r = resp(serde_json::json!({
            "overageConfiguration": {"overageStatus": "ENABLED"},
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 11000.0,
                "usageLimitWithPrecision": 1000.0,
                "currentOveragesWithPrecision": 10000.0,
                "overageCapWithPrecision": 10000.0
            }]
        }));
        assert!(!r.has_available_overage(), "超额已达上限应不可用");
        assert!(r.is_exhausted(1.0));
    }

    /// 超额启用但无 cap 字段（视为无上限）→ 不判定为耗尽
    #[test]
    fn test_overage_enabled_no_cap_available() {
        let r = resp(serde_json::json!({
            "overageConfiguration": {"overageLimit": null, "overageStatus": "ENABLED"},
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 5000.0,
                "usageLimitWithPrecision": 1000.0
            }]
        }));
        assert!(
            r.has_available_overage(),
            "无 cap 视为无上限，超额启用即可用"
        );
        assert!(!r.is_exhausted(1.0));
    }

    /// overageStatus 大小写不敏感
    #[test]
    fn test_overage_status_case_insensitive() {
        let r = resp(serde_json::json!({
            "overageConfiguration": {"overageStatus": "enabled"},
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 1000.0,
                "usageLimitWithPrecision": 1000.0,
                "overageCapWithPrecision": 10000.0
            }]
        }));
        assert!(r.has_available_overage());
    }

    /// 余额充足的普通账号 → 不耗尽（与超额无关）
    #[test]
    fn test_sufficient_balance_not_exhausted() {
        let r = resp(serde_json::json!({
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 100.0,
                "usageLimitWithPrecision": 1000.0
            }]
        }));
        assert!(!r.has_available_overage(), "无超额配置");
        assert!(!r.is_exhausted(1.0), "余额充足不应耗尽");
    }

    /// 无 overageConfiguration 字段（老响应/普通账号）→ 超额不可用
    #[test]
    fn test_no_overage_config() {
        let r = resp(serde_json::json!({
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 1000.0,
                "usageLimitWithPrecision": 1000.0
            }]
        }));
        assert!(!r.has_available_overage());
        assert!(r.is_exhausted(1.0));
    }
}
