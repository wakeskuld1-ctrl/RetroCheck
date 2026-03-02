use anyhow::{Result, anyhow};
use serde::Deserialize;

// ### 修改记录 (2026-03-01)
// - 原因: 需要在网关侧承载顺序规则的轻量模型
// - 目的: 让规则既支持 device_prefix 也支持可选 msg_type
// - 备注: 规则解析采用 serde_json，避免引入额外解析成本
// - 备注: priority 字段用于解决多条规则冲突的选择顺序
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OrderRule {
    pub device_prefix: String,
    pub msg_type: Option<u8>,
    pub order_group: String,
    pub stage: u32,
    // ### 修改记录 (2026-03-01)
    // - 原因: 规则配置可能不包含 priority
    // - 目的: 兼容旧配置并默认优先级为 0
    #[serde(default)]
    pub priority: i32,
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要在内存中维护一组可热更新的规则
// - 目的: 让匹配逻辑保持 O(n) 但足够轻量
// - 备注: 规则数量预期较小，线性扫描可接受
// - 备注: 默认空规则使用 derive(Default) 生成
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OrderingRules {
    pub rules: Vec<OrderRule>,
}

impl OrderingRules {
    // ### 修改记录 (2026-03-01)
    // - 原因: 管理接口下发规则采用 JSON
    // - 目的: 让规则热更新可直接落地到内存模型
    // - 备注: 失败时返回可观测的错误信息
    pub fn from_json(raw: &str) -> Result<Self> {
        let rules = serde_json::from_str::<OrderingRules>(raw)
            .map_err(|e| anyhow!("Invalid ordering rules: {e}"))?;
        Ok(rules)
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要根据 device_id + msg_type 精确选择规则
    // - 目的: 优先匹配 msg_type 规则，并按 priority 解决冲突
    // - 备注: 如果规则未声明 msg_type，则视为通配
    // - 备注: 修复冲突选择逻辑，避免较弱规则覆盖较强规则
    pub fn match_device(&self, device_id: &str, msg_type: u8) -> Option<OrderRule> {
        let mut best: Option<(usize, bool, i32)> = None;
        for (idx, rule) in self.rules.iter().enumerate() {
            if !device_id.starts_with(&rule.device_prefix) {
                continue;
            }
            let specific = match rule.msg_type {
                Some(rule_msg_type) => {
                    if rule_msg_type != msg_type {
                        continue;
                    }
                    true
                }
                None => false,
            };

            best = match best {
                None => Some((idx, specific, rule.priority)),
                Some((best_idx, best_specific, best_priority)) => {
                    if specific != best_specific {
                        if specific {
                            Some((idx, specific, rule.priority))
                        } else {
                            Some((best_idx, best_specific, best_priority))
                        }
                    } else if rule.priority > best_priority
                        || (rule.priority == best_priority && idx < best_idx)
                    {
                        Some((idx, specific, rule.priority))
                    } else {
                        Some((best_idx, best_specific, best_priority))
                    }
                }
            };
        }
        best.map(|(idx, _, _)| self.rules[idx].clone())
    }
}
