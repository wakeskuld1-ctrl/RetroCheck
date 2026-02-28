// ### 修改记录 (2026-02-26)
// - 原因: 需要读取 Edge 配置文件
// - 目的: 支撑 A/B 清理阈值配置
use anyhow::Result;
// ### 修改记录 (2026-02-26)
// - 原因: 需要配置序列化/反序列化
// - 目的: 统一 JSON 配置解析
use serde::{Deserialize, Serialize};
// ### 修改记录 (2026-02-26)
// - 原因: 需要文件路径与读取
// - 目的: 从磁盘加载 Edge 配置
use std::{fs::File, io::BufReader, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsistencyMode {
    Strong, // All nodes must succeed
    Quorum, // N/2 + 1 nodes must succeed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRole {
    Master,
    Slave,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub master_addr: String,
    pub slave_addrs: Vec<String>,
    pub mode: ConsistencyMode,
    pub committed_history_limit: usize,
}

// ### 修改记录 (2026-02-26)
// - 原因: 需要 Edge 层 A/B 清理阈值配置
// - 目的: 让键计数策略由配置文件驱动
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeConfig {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要配置清理触发阈值
    // - 目的: 当活动槽位达到比例时清理旧槽
    pub ab_cleanup_ratio: f64,
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要配置指令过期宽限期
    // - 目的: 让过期确认窗口由配置控制
    #[serde(default)]
    pub cmd_timeout_ms: u64,
}

impl ClusterConfig {
    pub fn new(master_addr: &str, slave_addrs: Vec<&str>, mode: ConsistencyMode) -> Self {
        Self {
            master_addr: master_addr.to_string(),
            slave_addrs: slave_addrs.iter().map(|s| s.to_string()).collect(),
            mode,
            committed_history_limit: 32,
        }
    }
}

// ### 修改记录 (2026-02-26)
// - 原因: 需要为 EdgeConfig 提供默认值
// - 目的: 保证未配置时仍可运行
impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            // ### 修改记录 (2026-02-26)
            // - 原因: 默认阈值为 50%
            // - 目的: 与 A/B 设计约定一致
            ab_cleanup_ratio: 0.5,
            // ### 修改记录 (2026-02-27)
            // - 原因: 需要默认超时配置
            // - 目的: 保持过期策略默认严格
            cmd_timeout_ms: 0,
        }
    }
}

// ### 修改记录 (2026-02-26)
// - 原因: 需要从配置文件加载 EdgeConfig
// - 目的: 让键计数策略外部可配置
impl EdgeConfig {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取 JSON 配置
    // - 目的: 供 EdgeStore 初始化使用
    pub fn load_from_file(path: &Path) -> Result<Self> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要打开配置文件
        // - 目的: 将文件内容交给反序列化
        let file = File::open(path)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要缓冲读取
        // - 目的: 提升小文件读取稳定性
        let reader = BufReader::new(file);
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要反序列化 JSON
        // - 目的: 得到 EdgeConfig 实例
        let config: EdgeConfig = serde_json::from_reader(reader)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要防止非法阈值
        // - 目的: 保持清理逻辑可预期
        Ok(config.sanitize())
    }

    // ### 修改记录 (2026-02-26)
    // - 原因: 需要校正阈值范围
    // - 目的: 避免 0 或负数导致误触发
    fn sanitize(self) -> Self {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要阈值在 (0, 1]
        // - 目的: 保持比例语义稳定
        if self.ab_cleanup_ratio <= 0.0 || self.ab_cleanup_ratio > 1.0 {
            return Self::default();
        }
        self
    }
}
