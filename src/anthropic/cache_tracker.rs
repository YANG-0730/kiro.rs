use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use crate::token::{
    TOKENS_PER_MESSAGE, count_message_content_tokens, count_system_message_tokens,
    count_tool_definition_tokens,
};

use super::types::{CacheControl, Message, MessagesRequest};

const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);
const ONE_HOUR_CACHE_TTL: Duration = Duration::from_secs(3600);
/// 官方 lookback 窗口：每个显式断点最多向前检查 20 个块位置（含断点自身）
/// https://platform.claude.com/docs/en/build-with-claude/prompt-caching
const PREFIX_LOOKBACK_BLOCKS: usize = 20;

#[derive(Debug, Clone, Copy, Default)]
pub struct CacheResult {
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}

#[derive(Debug, Clone)]
pub struct CacheProfile {
    total_input_tokens: i32,
    min_cacheable_tokens: i32,
    blocks: Vec<CacheBlock>,
    breakpoints: Vec<CacheBreakpoint>,
}

#[derive(Debug, Clone)]
struct CacheBlock {
    prefix_fingerprint: [u8; 32],
    cumulative_tokens: i32,
}

#[derive(Debug, Clone)]
struct CacheBreakpoint {
    block_index: usize,
    ttl: Duration,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    token_count: i32,
    ttl: Duration,
    expires_at: Instant,
}

struct CachedCheckpointStore {
    by_credential: HashMap<u64, HashMap<[u8; 32], CacheEntry>>,
}

pub struct CacheTracker {
    entries: Mutex<CachedCheckpointStore>,
    max_supported_ttl: Duration,
}

impl CacheTracker {
    pub fn new(max_supported_ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(CachedCheckpointStore {
                by_credential: HashMap::new(),
            }),
            max_supported_ttl,
        }
    }

    pub fn build_profile(
        &self,
        payload: &MessagesRequest,
        total_input_tokens: i32,
    ) -> CacheProfile {
        let flattened = flatten_cacheable_blocks(payload);

        // 与 prompt 内容无关但会影响官方缓存可复用性的固定配置。
        let request_prelude = canonicalize_json(serde_json::json!({
            "model": payload.model,
            "tool_choice": payload.tool_choice,
        }));
        let prelude_bytes = serde_json::to_vec(&request_prelude).unwrap_or_default();
        let mut prefix_hasher = Sha256::new();
        prefix_hasher.update((prelude_bytes.len() as u64).to_be_bytes());
        prefix_hasher.update(&prelude_bytes);

        let mut blocks = Vec::with_capacity(flattened.len());
        let mut breakpoints = Vec::new();
        let mut cumulative_tokens = 0i32;

        for (index, block) in flattened.into_iter().enumerate() {
            cumulative_tokens = cumulative_tokens.saturating_add(block.tokens);

            let block_bytes = serde_json::to_vec(&block.value).unwrap_or_default();
            let block_hash: [u8; 32] = Sha256::digest(&block_bytes).into();

            let mut next_prefix_hasher = prefix_hasher.clone();
            next_prefix_hasher.update(block_hash);
            let prefix_fingerprint: [u8; 32] = next_prefix_hasher.finalize().into();
            prefix_hasher = Sha256::new();
            prefix_hasher.update(prefix_fingerprint);

            blocks.push(CacheBlock {
                prefix_fingerprint,
                cumulative_tokens,
            });

            // 只有客户端显式 cache_control 才是断点：官方语义下写入只发生在显式断点处，
            // 最后一个断点之后的内容既不读也不写缓存（计入 input_tokens）
            if let Some(ttl) = block.breakpoint_ttl {
                breakpoints.push(CacheBreakpoint {
                    block_index: index,
                    ttl: ttl.min(self.max_supported_ttl),
                });
            }
        }

        CacheProfile {
            total_input_tokens: total_input_tokens.max(0),
            min_cacheable_tokens: minimum_cacheable_tokens_for_model(&payload.model),
            blocks,
            breakpoints,
        }
    }

    pub fn compute(&self, credential_id: u64, profile: &CacheProfile) -> CacheResult {
        let Some(last_breakpoint) = profile.last_cacheable_breakpoint() else {
            return CacheResult::default();
        };
        let last_breakpoint_tokens = last_breakpoint
            .cumulative_tokens
            .min(profile.total_input_tokens);

        let now = Instant::now();
        let mut entries = self.entries.lock();
        prune_expired(&mut entries.by_credential, now);

        let Some(credential_entries) = entries.by_credential.get_mut(&credential_id) else {
            // 首次请求，需要创建缓存
            tracing::debug!(credential_id, "首次请求，无缓存条目");
            let (cache_5m, cache_1h) = compute_ttl_breakdown(profile, 0);
            return CacheResult {
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: last_breakpoint_tokens,
                cache_creation_5m_input_tokens: cache_5m,
                cache_creation_1h_input_tokens: cache_1h,
            };
        };

        tracing::debug!(
            credential_id,
            entry_count = credential_entries.len(),
            "查找缓存匹配"
        );

        // 官方规则：从每个显式断点向前逐块回看（最多 20 个位置），找之前请求写入过的前缀；
        // 取能命中的最长前缀。查找的是"之前的写入"，而不是"内容是否稳定"。
        let mut matched_tokens = 0;
        for breakpoint in profile.cacheable_breakpoints().iter().rev() {
            let lowest = breakpoint
                .block_index
                .saturating_sub(PREFIX_LOOKBACK_BLOCKS - 1);
            let hit = (lowest..=breakpoint.block_index).rev().find_map(|i| {
                let block = &profile.blocks[i];
                let entry = credential_entries.get_mut(&block.prefix_fingerprint)?;
                if entry.expires_at <= now {
                    return None;
                }
                entry.expires_at = now + entry.ttl;
                Some(block.cumulative_tokens)
            });
            if let Some(tokens) = hit {
                matched_tokens = matched_tokens.max(tokens.min(profile.total_input_tokens));
                break;
            }
        }

        let new_tokens = last_breakpoint_tokens.saturating_sub(matched_tokens).max(0);
        let (cache_5m, cache_1h) = compute_ttl_breakdown(profile, matched_tokens);

        tracing::debug!(
            credential_id,
            matched_tokens,
            new_tokens,
            cache_5m,
            cache_1h,
            "缓存计算结果"
        );

        CacheResult {
            cache_read_input_tokens: matched_tokens.max(0),
            cache_creation_input_tokens: new_tokens,
            cache_creation_5m_input_tokens: cache_5m,
            cache_creation_1h_input_tokens: cache_1h,
        }
    }

    pub fn update(&self, credential_id: u64, profile: &CacheProfile) {
        let now = Instant::now();
        let mut entries = self.entries.lock();
        prune_expired(&mut entries.by_credential, now);

        let credential_entries = entries.by_credential.entry(credential_id).or_default();

        for breakpoint in profile.cacheable_breakpoints() {
            let block = &profile.blocks[breakpoint.block_index];
            let next_expiry = now + breakpoint.ttl;

            match credential_entries.get_mut(&block.prefix_fingerprint) {
                Some(existing) => {
                    existing.token_count = existing.token_count.max(block.cumulative_tokens);
                    existing.ttl = existing.ttl.max(breakpoint.ttl);
                    existing.expires_at = existing.expires_at.max(next_expiry);
                }
                None => {
                    credential_entries.insert(
                        block.prefix_fingerprint,
                        CacheEntry {
                            token_count: block.cumulative_tokens,
                            ttl: breakpoint.ttl,
                            expires_at: next_expiry,
                        },
                    );
                }
            }
        }
    }
}

/// 计算不同 TTL 的缓存创建 token 数
fn compute_ttl_breakdown(profile: &CacheProfile, matched_tokens: i32) -> (i32, i32) {
    let Some(last_breakpoint) = profile.last_cacheable_breakpoint() else {
        return (0, 0);
    };

    let new_tokens = last_breakpoint
        .cumulative_tokens
        .min(profile.total_input_tokens)
        .saturating_sub(matched_tokens)
        .max(0);

    if new_tokens == 0 {
        return (0, 0);
    }

    if last_breakpoint.ttl == ONE_HOUR_CACHE_TTL {
        (0, new_tokens)
    } else {
        (new_tokens, 0)
    }
}

impl CacheProfile {
    #[cfg(test)]
    pub fn total_input_tokens(&self) -> i32 {
        self.total_input_tokens
    }

    fn cacheable_breakpoints(&self) -> Vec<ResolvedBreakpoint> {
        self.breakpoints
            .iter()
            .filter_map(|breakpoint| {
                let block = self.blocks.get(breakpoint.block_index)?;
                if block.cumulative_tokens < self.min_cacheable_tokens {
                    return None;
                }

                Some(ResolvedBreakpoint {
                    block_index: breakpoint.block_index,
                    cumulative_tokens: block.cumulative_tokens,
                    ttl: breakpoint.ttl,
                })
            })
            .collect()
    }

    fn last_cacheable_breakpoint(&self) -> Option<ResolvedBreakpoint> {
        self.cacheable_breakpoints().into_iter().last()
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolvedBreakpoint {
    block_index: usize,
    cumulative_tokens: i32,
    ttl: Duration,
}

#[derive(Debug)]
struct PendingBlock {
    value: serde_json::Value,
    tokens: i32,
    breakpoint_ttl: Option<Duration>,
}

fn flatten_cacheable_blocks(payload: &MessagesRequest) -> Vec<PendingBlock> {
    let mut blocks = Vec::new();

    if let Some(tools) = &payload.tools {
        for (tool_index, tool) in tools.iter().enumerate() {
            let mut value = serde_json::to_value(tool).unwrap_or(serde_json::Value::Null);
            let breakpoint_ttl = extract_cache_ttl(&value);
            strip_cache_control(&mut value);

            blocks.push(PendingBlock {
                value: canonicalize_json(serde_json::json!({
                    "kind": "tool",
                    "tool_index": tool_index,
                    "tool": value,
                })),
                tokens: count_tool_definition_tokens(tool) as i32,
                breakpoint_ttl,
            });
        }
    }

    if let Some(system) = &payload.system {
        for (system_index, block) in system.iter().enumerate() {
            let mut value = serde_json::to_value(block).unwrap_or(serde_json::Value::Null);
            let breakpoint_ttl = extract_cache_ttl(&value);
            strip_cache_control(&mut value);
            canonicalize_system_block_for_cache(&mut value);

            blocks.push(PendingBlock {
                value: canonicalize_json(serde_json::json!({
                    "kind": "system",
                    "system_index": system_index,
                    "block": value,
                })),
                tokens: count_system_message_tokens(block) as i32,
                breakpoint_ttl,
            });
        }
    }

    for (message_index, message) in payload.messages.iter().enumerate() {
        blocks.extend(flatten_message_blocks(message_index, message));
    }

    blocks
}

fn canonicalize_system_block_for_cache(value: &mut serde_json::Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };

    let is_text_block = obj
        .get("type")
        .and_then(|v| v.as_str())
        .map(|t| t == "text")
        .unwrap_or(true);
    if !is_text_block {
        return;
    }

    let Some(text) = obj.get("text").and_then(|v| v.as_str()) else {
        return;
    };
    if !text.starts_with("x-anthropic-billing-header:") {
        return;
    }

    obj.insert(
        "text".to_string(),
        serde_json::Value::String("__anthropic_billing_header__".to_string()),
    );
}

fn flatten_message_blocks(message_index: usize, message: &Message) -> Vec<PendingBlock> {
    match &message.content {
        serde_json::Value::String(text) => vec![build_message_block(
            message_index,
            &message.role,
            0,
            serde_json::json!({
                "type": "text",
                "text": text,
            }),
            None,
            true,
        )],
        serde_json::Value::Array(blocks) => {
            let last_block_index = blocks.len().saturating_sub(1);
            blocks
                .iter()
                .enumerate()
                .map(|(block_index, block)| {
                    let breakpoint_ttl = extract_cache_ttl(block);
                    let mut normalized = block.clone();
                    strip_cache_control(&mut normalized);
                    build_message_block(
                        message_index,
                        &message.role,
                        block_index,
                        normalized,
                        breakpoint_ttl,
                        block_index == last_block_index,
                    )
                })
                .collect()
        }
        other => vec![build_message_block(
            message_index,
            &message.role,
            0,
            other.clone(),
            None,
            true,
        )],
    }
}

fn build_message_block(
    message_index: usize,
    role: &str,
    block_index: usize,
    block: serde_json::Value,
    breakpoint_ttl: Option<Duration>,
    is_message_end: bool,
) -> PendingBlock {
    // 消息结构开销计入该消息的最后一个块，使各块累计 token 与 count_all_tokens 的总量对齐；
    // 否则长对话（上千条消息）每条差 4 个 token，全部前缀命中时仍会剩下数千"未缓存"输入，
    // 换算成上游口径后更被放大约 2 倍。
    let structure = if is_message_end {
        TOKENS_PER_MESSAGE
    } else {
        0
    };
    PendingBlock {
        tokens: (count_message_content_tokens(&block) + structure) as i32,
        value: canonicalize_json(serde_json::json!({
            "kind": "message",
            "message_index": message_index,
            "role": role,
            "block_index": block_index,
            "block": block,
        })),
        breakpoint_ttl,
    }
}

fn extract_cache_ttl(value: &serde_json::Value) -> Option<Duration> {
    let cache_control = value.get("cache_control")?;
    let cache_control: CacheControl = serde_json::from_value(cache_control.clone()).ok()?;
    if cache_control.cache_type != "ephemeral" {
        return None;
    }

    Some(match cache_control.ttl.as_deref() {
        Some("1h") => ONE_HOUR_CACHE_TTL,
        _ => DEFAULT_CACHE_TTL,
    })
}

fn strip_cache_control(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(arr) => {
            for item in arr {
                strip_cache_control(item);
            }
        }
        serde_json::Value::Object(map) => {
            map.remove("cache_control");
            for item in map.values_mut() {
                strip_cache_control(item);
            }
        }
        _ => {}
    }
}

fn minimum_cacheable_tokens_for_model(model: &str) -> i32 {
    let model_lower = model.to_lowercase();

    if model_lower.contains("opus") {
        4096
    } else if model_lower.contains("haiku-3") || model_lower.contains("haiku_3") {
        2048
    } else {
        1024
    }
}

fn prune_expired(entries: &mut HashMap<u64, HashMap<[u8; 32], CacheEntry>>, now: Instant) {
    entries.retain(|_, credential_entries| {
        credential_entries.retain(|_, entry| entry.expires_at > now);
        !credential_entries.is_empty()
    });
}

fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(canonicalize_json).collect())
        }
        serde_json::Value::Object(map) => {
            let ordered: BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect();

            let mut out = serde_json::Map::new();
            for (key, value) in ordered {
                out.insert(key, value);
            }
            serde_json::Value::Object(out)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::{SystemMessage, Tool};
    use crate::token;

    fn build_request(messages: Vec<Message>) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages,
            stream: false,
            system: Some(vec![SystemMessage {
                block_type: None,
                text: "system".to_string(),
                cache_control: None,
            }]),
            tools: Some(vec![Tool {
                tool_type: None,
                name: "echo".to_string(),
                description: "echo".to_string(),
                input_schema: Default::default(),
                max_uses: None,
                cache_control: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    fn build_request_with_system(
        messages: Vec<Message>,
        system: Vec<SystemMessage>,
    ) -> MessagesRequest {
        let mut request = build_request(messages);
        request.system = Some(system);
        request
    }

    fn msg(role: &str, content: serde_json::Value) -> Message {
        Message {
            role: role.to_string(),
            content,
        }
    }

    fn cache_text(text: &str) -> serde_json::Value {
        serde_json::json!([{
            "type": "text",
            "text": text,
            "cache_control": { "type": "ephemeral" }
        }])
    }

    fn long_cacheable_text() -> String {
        std::iter::repeat_n("cacheable prompt chunk", 256)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn medium_turn_text(label: &str) -> String {
        format!(
            "{} {}",
            label,
            std::iter::repeat_n("conversation growth chunk", 80)
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn estimate_input_tokens(request: &MessagesRequest) -> i32 {
        token::count_all_tokens(
            request.model.clone(),
            request.system.clone(),
            request.messages.clone(),
            request.tools.clone(),
        ) as i32
    }

    #[test]
    fn full_prefix_hit_leaves_only_new_turn_uncached_in_long_conversation() {
        // 复现：1000 条消息的长对话全部前缀命中时，旧实现因每条消息 4 token 的结构开销
        // 只计入总量、不计入缓存块，会剩下 ~4000 "未缓存"输入（换算上游口径后约 8000）。
        // 断点按 Claude Code 形态每轮移到最新一条 user 消息上。
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let history = || {
            let mut v = vec![msg("user", serde_json::json!(long_cacheable_text()))];
            v.push(msg("assistant", serde_json::json!("R0")));
            for i in 1..500 {
                v.push(msg("user", serde_json::json!(format!("question {i}"))));
                v.push(msg("assistant", serde_json::json!(format!("answer {i}"))));
            }
            v
        };
        let mut first = history();
        first.push(msg("user", cache_text("question 500")));
        let req1 = build_request(first);
        tracker.update(
            1,
            &tracker.build_profile(&req1, estimate_input_tokens(&req1)),
        );

        let mut next = history();
        next.push(msg(
            "user",
            serde_json::json!([{"type": "text", "text": "question 500"}]),
        ));
        next.push(msg("assistant", serde_json::json!("answer 500")));
        next.push(msg("user", cache_text("next question")));
        let req2 = build_request(next);
        let total = estimate_input_tokens(&req2);
        let result = tracker.compute(1, &tracker.build_profile(&req2, total));
        let uncached = total - result.cache_read_input_tokens - result.cache_creation_input_tokens;
        assert!(result.cache_read_input_tokens > 0, "应命中上一轮缓存前缀");
        assert!(
            result.cache_creation_input_tokens < 50,
            "只有新增一轮应写入缓存，实际 {}",
            result.cache_creation_input_tokens
        );
        assert!(
            uncached < 20,
            "断点在最后一条消息末尾时未缓存部分只应是零头，实际 {uncached}（total={total}, read={}, write={}）",
            result.cache_read_input_tokens,
            result.cache_creation_input_tokens
        );
    }

    #[test]
    fn attribution_header_drift_does_not_break_cache_hit() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let system1 = vec![
            SystemMessage {
                block_type: Some("text".to_string()),
                text:
                    "x-anthropic-billing-header: cc_version=2.1.87.1; cc_entrypoint=cli; cch=aaaaa;"
                        .to_string(),
                cache_control: None,
            },
            SystemMessage {
                block_type: Some("text".to_string()),
                text: long_cacheable_text(),
                cache_control: Some(CacheControl {
                    cache_type: "ephemeral".to_string(),
                    ttl: None,
                }),
            },
        ];
        let system2 = vec![
            SystemMessage {
                block_type: Some("text".to_string()),
                text: "x-anthropic-billing-header: cc_version=2.1.87.222222222222222222; cc_entrypoint=cli; cch=bbbbb; extra_padding=xyzxyzxyzxyz;".to_string(),
                cache_control: None,
            },
            SystemMessage {
                block_type: Some("text".to_string()),
                text: long_cacheable_text(),
                cache_control: Some(CacheControl {
                    cache_type: "ephemeral".to_string(),
                    ttl: None,
                }),
            },
        ];

        let req1 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system1);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system2);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);
        let expected_match = profile2
            .last_cacheable_breakpoint()
            .map(|bp| bp.cumulative_tokens.min(profile2.total_input_tokens()))
            .unwrap_or(0);

        assert!(total1 != total2);
        assert!(result.cache_read_input_tokens > 0);
        assert_eq!(result.cache_read_input_tokens, expected_match);
        assert_eq!(result.cache_creation_input_tokens, 0);
    }

    #[test]
    fn normal_system_text_change_still_misses() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let system1 = vec![SystemMessage {
            block_type: Some("text".to_string()),
            text: long_cacheable_text(),
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
                ttl: None,
            }),
        }];
        let system2 = vec![SystemMessage {
            block_type: Some("text".to_string()),
            text: format!("{} extra", long_cacheable_text()),
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
                ttl: None,
            }),
        }];

        let req1 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system1);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 =
            build_request_with_system(vec![msg("user", serde_json::json!("hello"))], system2);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        assert_eq!(result.cache_read_input_tokens, 0);
    }

    #[test]
    fn explicit_breakpoint_without_hit_creates_prefix_only() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        let total = estimate_input_tokens(&req);
        let profile = tracker.build_profile(&req, total);
        let result = tracker.compute(1, &profile);

        assert_eq!(result.cache_read_input_tokens, 0);
        assert_eq!(
            result.cache_creation_input_tokens,
            profile
                .last_cacheable_breakpoint()
                .map(|bp| bp.cumulative_tokens)
                .unwrap_or(0)
        );
    }

    #[test]
    fn same_content_with_shape_drift_does_not_false_hit() {
        // 字符串内容与单块数组等价；断点挪到后面的 assistant 上时，回看窗口内能找到上一轮
        // 在 user 消息处的写入 → 命中，新增 assistant 写入缓存
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req1 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        tracker.update(
            1,
            &tracker.build_profile(&req1, estimate_input_tokens(&req1)),
        );

        let req2 = build_request(vec![
            msg("user", serde_json::json!(long_cacheable_text())),
            msg("assistant", cache_text("ok")),
        ]);
        let result = tracker.compute(
            1,
            &tracker.build_profile(&req2, estimate_input_tokens(&req2)),
        );
        assert!(result.cache_read_input_tokens > 0);
        assert!(result.cache_creation_input_tokens > 0);
    }

    #[test]
    fn changed_earlier_content_does_not_false_hit() {
        // 真正的"误命中"防线：前面的内容变了，之后的前缀都不能命中
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req1 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        tracker.update(
            1,
            &tracker.build_profile(&req1, estimate_input_tokens(&req1)),
        );

        let req2 = build_request(vec![
            msg(
                "user",
                serde_json::json!(format!("{} changed", long_cacheable_text())),
            ),
            msg("assistant", cache_text("ok")),
        ]);
        let r = tracker.compute(
            1,
            &tracker.build_profile(&req2, estimate_input_tokens(&req2)),
        );
        assert_eq!(r.cache_read_input_tokens, 0);
        assert!(r.cache_creation_input_tokens > 0);
    }

    #[test]
    fn tail_after_last_breakpoint_counts_as_input_tokens() {
        // 用户场景：断点不在最后一个块（如 Claude Code 辅助请求、或 system-reminder 附在末尾），
        // 断点之后的尾部既不读也不写缓存 → input_tokens > 0（此前被错算进 cache_creation）
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req = build_request(vec![msg(
            "user",
            serde_json::json!([
                {"type": "text", "text": long_cacheable_text(), "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": medium_turn_text("uncached tail")}
            ]),
        )]);
        let total = estimate_input_tokens(&req);
        let r = tracker.compute(1, &tracker.build_profile(&req, total));
        let input = total - r.cache_read_input_tokens - r.cache_creation_input_tokens;
        assert!(
            input > 50,
            "断点之后的尾部应计入 input_tokens，实际 {input}"
        );
    }

    #[test]
    fn previous_write_outside_lookback_window_misses() {
        // 官方：每个断点最多回看 20 个块；上一轮的写入在窗口外则不命中
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let mut base = vec![msg("user", cache_text(&long_cacheable_text()))];
        let req1 = build_request(base.clone());
        tracker.update(
            1,
            &tracker.build_profile(&req1, estimate_input_tokens(&req1)),
        );

        base[0] = msg("user", serde_json::json!(long_cacheable_text()));
        for i in 0..12 {
            base.push(msg("assistant", serde_json::json!(format!("a{i}"))));
            base.push(msg("user", serde_json::json!(format!("u{i}"))));
        }
        base.push(msg("assistant", cache_text("final")));
        let r = tracker.compute(1, &tracker.build_profile(&build_request(base), 100_000));
        assert_eq!(r.cache_read_input_tokens, 0, "超过 20 块的回看窗口不应命中");
    }

    #[test]
    fn same_length_retry_with_same_breakpoint_is_hit() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req1 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let req2 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        assert_eq!(
            result.cache_read_input_tokens,
            profile1
                .last_cacheable_breakpoint()
                .map(|bp| bp.cumulative_tokens)
                .unwrap_or(0)
        );
        assert_eq!(result.cache_creation_input_tokens, 0);
    }

    #[test]
    fn prefix_match_with_appended_turn_reads_previous_prefix_cache() {
        // 断点只在第一条消息上：之后追加的轮次落在最后一个断点之后，计入 input_tokens
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let req1 = build_request(vec![
            msg("user", cache_text(&long_cacheable_text())),
            msg("assistant", serde_json::json!("R1")),
        ]);
        let profile1 = tracker.build_profile(&req1, estimate_input_tokens(&req1));
        tracker.update(1, &profile1);

        let req2 = build_request(vec![
            msg("user", cache_text(&long_cacheable_text())),
            msg("assistant", serde_json::json!("R1")),
            msg("user", serde_json::json!("New feedback")),
            msg("assistant", serde_json::json!("R3")),
        ]);
        let total2 = estimate_input_tokens(&req2);
        let result = tracker.compute(1, &tracker.build_profile(&req2, total2));

        let matched_tokens = profile1
            .last_cacheable_breakpoint()
            .map(|bp| bp.cumulative_tokens)
            .unwrap_or(0);
        assert!(matched_tokens > 0);
        assert_eq!(result.cache_read_input_tokens, matched_tokens);
        assert_eq!(result.cache_creation_input_tokens, 0);
        assert!(
            total2 - result.cache_read_input_tokens > 0,
            "断点之后的内容计入 input_tokens"
        );
    }

    #[test]
    fn every_explicit_cache_control_becomes_a_breakpoint() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let mut messages = Vec::new();
        for i in 0..12 {
            messages.push(msg(
                "user",
                cache_text(&format!("{}-{i}", long_cacheable_text())),
            ));
            messages.push(msg("assistant", serde_json::json!(format!("reply-{i}"))));
        }
        let req = build_request(messages);
        let total = estimate_input_tokens(&req);
        let profile = tracker.build_profile(&req, total);
        // 12 个显式 cache_control → 12 个断点；不再在每条消息末尾派生额外断点
        assert_eq!(profile.cacheable_breakpoints().len(), 12);
    }

    #[test]
    fn multi_turn_history_extends_cacheable_prefix() {
        // Claude Code 形态：断点每轮移到最新一条 user 消息；上一轮断点处的前缀
        // 在新断点的 20 块回看窗口内，应命中并逐轮增长
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let long = long_cacheable_text();
        let plain = |t: &str| serde_json::json!([{"type": "text", "text": t}]);

        let req1 = build_request(vec![msg("user", cache_text(&long))]);
        let profile1 = tracker.build_profile(&req1, estimate_input_tokens(&req1));
        let result1 = tracker.compute(1, &profile1);
        assert!(result1.cache_creation_input_tokens > 0);
        tracker.update(1, &profile1);

        let req2 = build_request(vec![
            msg("user", plain(&long)),
            msg("assistant", serde_json::json!(medium_turn_text("R1"))),
            msg("user", cache_text(&medium_turn_text("R2"))),
        ]);
        let profile2 = tracker.build_profile(&req2, estimate_input_tokens(&req2));
        let result2 = tracker.compute(1, &profile2);
        assert!(result2.cache_read_input_tokens >= result1.cache_creation_input_tokens);
        assert!(result2.cache_creation_input_tokens > 0);
        tracker.update(1, &profile2);

        let req3 = build_request(vec![
            msg("user", plain(&long)),
            msg("assistant", serde_json::json!(medium_turn_text("R1"))),
            msg("user", plain(&medium_turn_text("R2"))),
            msg("assistant", serde_json::json!(medium_turn_text("R2A"))),
            msg("user", cache_text(&medium_turn_text("R3"))),
        ]);
        let profile3 = tracker.build_profile(&req3, estimate_input_tokens(&req3));
        let result3 = tracker.compute(1, &profile3);
        assert!(result3.cache_read_input_tokens > result2.cache_read_input_tokens);
    }

    #[test]
    fn tool_changes_invalidate_downstream_prefix() {
        let tracker = CacheTracker::new(Duration::from_secs(3600));
        let mut req1 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        req1.tools.as_mut().unwrap().push(Tool {
            tool_type: None,
            name: "alpha".to_string(),
            description: "alpha".to_string(),
            input_schema: Default::default(),
            max_uses: None,
            cache_control: None,
        });
        let total1 = estimate_input_tokens(&req1);
        let profile1 = tracker.build_profile(&req1, total1);
        tracker.update(1, &profile1);

        let mut req2 = build_request(vec![msg("user", cache_text(&long_cacheable_text()))]);
        req2.tools.as_mut().unwrap().push(Tool {
            tool_type: None,
            name: "beta".to_string(),
            description: "beta".to_string(),
            input_schema: Default::default(),
            max_uses: None,
            cache_control: None,
        });
        let total2 = estimate_input_tokens(&req2);
        let profile2 = tracker.build_profile(&req2, total2);
        let result = tracker.compute(1, &profile2);

        assert_eq!(result.cache_read_input_tokens, 0);
        assert_eq!(
            result.cache_creation_input_tokens,
            profile2
                .last_cacheable_breakpoint()
                .map(|bp| bp.cumulative_tokens)
                .unwrap_or(0)
        );
    }
}
