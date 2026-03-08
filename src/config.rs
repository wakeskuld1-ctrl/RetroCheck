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

// ### 修改记录 (2026-03-01)
// - 原因: 需要 EdgeGateway 统一配置
// - 目的: 支持 TTL/Nonce/持久化开关配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeGatewayConfig {
    pub session_ttl_ms: u64,
    pub nonce_cache_limit: usize,
    pub nonce_persist_enabled: bool,
    pub nonce_persist_path: String,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要支持密钥配置
    // - 目的: 避免硬编码密钥，提升安全性
    #[serde(default = "default_secret_key")]
    pub secret_key: String,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要并发连接数限制
    // - 目的: 防止 DoS 攻击
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在入口聚合写请求
    // - 目的: 控制 Smart Batcher 单批最大 SQL 数
    #[serde(default = "default_edge_batch_max_size")]
    pub batch_max_size: usize,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在延迟与吞吐间取得平衡
    // - 目的: 设置 Smart Batcher 最大等待时间
    #[serde(default = "default_edge_batch_max_delay_ms")]
    pub batch_max_delay_ms: u64,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要有明确背压策略
    // - 目的: 控制 Smart Batcher 队列上限
    #[serde(default = "default_edge_batch_max_queue_size")]
    pub batch_max_queue_size: usize,
    // ### 修改记录 (2026-03-01)
    // - 原因: 防止请求无限等待批处理结果
    // - 目的: 设置单请求最大等待时间
    #[serde(default = "default_edge_batch_wait_timeout_ms")]
    pub batch_wait_timeout_ms: u64,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要控制顺序域阶段并行度
    // - 目的: 防止顺序调度器过度并发
    #[serde(default = "default_edge_ordering_stage_parallelism")]
    pub ordering_stage_parallelism: usize,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要限制顺序调度队列长度
    // - 目的: 避免热更新场景下排队过深
    #[serde(default = "default_edge_ordering_queue_limit")]
    pub ordering_queue_limit: usize,
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要支持 SmartBatcher 分片
    // - 目的: 提升高并发场景下的吞吐量
    #[serde(default = "default_edge_batch_shards")]
    pub batch_shards: usize,
    #[serde(default = "default_edge_pre_agg_enabled")]
    pub pre_agg_enabled: bool,
    #[serde(default = "default_edge_pre_agg_queue_size")]
    pub pre_agg_queue_size: usize,
    // ### 修改记录 (2026-03-04)
    // - 原因: 需要将端到端重试策略配置化
    // - 目的: 统一控制重试间隔、重试次数与指数退避开关
    #[serde(default = "default_edge_client_retry_delay_ms")]
    pub edge_client_retry_delay_ms: u64,
    #[serde(default = "default_edge_client_retry_max_attempts")]
    pub edge_client_retry_max_attempts: u32,
    #[serde(default = "default_edge_client_retry_exponential_backoff")]
    pub edge_client_retry_exponential_backoff: bool,
}

fn default_secret_key() -> String {
    "device_secret".to_string()
}

fn default_max_connections() -> usize {
    1000
}

fn default_edge_batch_shards() -> usize {
    4
}

fn default_edge_pre_agg_enabled() -> bool {
    false
}

fn default_edge_pre_agg_queue_size() -> usize {
    50_000
}

fn default_edge_client_retry_delay_ms() -> u64 {
    100
}

fn default_edge_client_retry_max_attempts() -> u32 {
    3
}

fn default_edge_client_retry_exponential_backoff() -> bool {
    true
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要提供 Smart Batcher 的默认参数
// - 目的: 让无配置文件场景仍可控
fn default_edge_batch_max_size() -> usize {
    256
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要默认延迟窗口
// - 目的: 在吞吐与延迟间取得折中
fn default_edge_batch_max_delay_ms() -> u64 {
    10
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要默认队列容量
// - 目的: 防止过高并发下内存暴涨
fn default_edge_batch_max_queue_size() -> usize {
    50_000
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要默认等待超时
// - 目的: 防止请求卡死
fn default_edge_batch_wait_timeout_ms() -> u64 {
    200
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要顺序调度的默认并行度
// - 目的: 在性能与顺序约束间取得折中
fn default_edge_ordering_stage_parallelism() -> usize {
    4
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要顺序调度队列默认上限
// - 目的: 控制高峰期内存占用
fn default_edge_ordering_queue_limit() -> usize {
    50_000
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要默认配置
// - 目的: 保证无配置文件时可用
impl Default for EdgeGatewayConfig {
    fn default() -> Self {
        Self {
            session_ttl_ms: 60_000,
            nonce_cache_limit: 1000,
            nonce_persist_enabled: false,
            nonce_persist_path: "edge_nonce_cache".to_string(),
            secret_key: default_secret_key(),
            max_connections: default_max_connections(),
            batch_max_size: default_edge_batch_max_size(),
            batch_max_delay_ms: default_edge_batch_max_delay_ms(),
            batch_max_queue_size: default_edge_batch_max_queue_size(),
            batch_wait_timeout_ms: default_edge_batch_wait_timeout_ms(),
            ordering_stage_parallelism: default_edge_ordering_stage_parallelism(),
            ordering_queue_limit: default_edge_ordering_queue_limit(),
            batch_shards: default_edge_batch_shards(),
            pre_agg_enabled: default_edge_pre_agg_enabled(),
            pre_agg_queue_size: default_edge_pre_agg_queue_size(),
            edge_client_retry_delay_ms: default_edge_client_retry_delay_ms(),
            edge_client_retry_max_attempts: default_edge_client_retry_max_attempts(),
            edge_client_retry_exponential_backoff: default_edge_client_retry_exponential_backoff(),
        }
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要加载 EdgeGateway 配置
// - 目的: 从 JSON 文件初始化网关
impl EdgeGatewayConfig {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let config: EdgeGatewayConfig = serde_json::from_reader(reader)?;
        Ok(config.sanitize())
    }

    fn sanitize(self) -> Self {
        let default = Self::default();
        Self {
            session_ttl_ms: if self.session_ttl_ms == 0 {
                default.session_ttl_ms
            } else {
                self.session_ttl_ms
            },
            nonce_cache_limit: if self.nonce_cache_limit == 0 {
                default.nonce_cache_limit
            } else {
                self.nonce_cache_limit
            },
            nonce_persist_enabled: self.nonce_persist_enabled,
            nonce_persist_path: if self.nonce_persist_path.is_empty() {
                default.nonce_persist_path
            } else {
                self.nonce_persist_path
            },
            secret_key: if self.secret_key.is_empty() {
                default.secret_key
            } else {
                self.secret_key
            },
            max_connections: if self.max_connections == 0 {
                default.max_connections
            } else {
                self.max_connections
            },
            batch_max_size: if self.batch_max_size == 0 {
                default.batch_max_size
            } else {
                self.batch_max_size
            },
            batch_max_delay_ms: self.batch_max_delay_ms,
            batch_max_queue_size: if self.batch_max_queue_size == 0 {
                default.batch_max_queue_size
            } else {
                self.batch_max_queue_size
            },
            batch_wait_timeout_ms: self.batch_wait_timeout_ms,
            ordering_stage_parallelism: if self.ordering_stage_parallelism == 0 {
                default.ordering_stage_parallelism
            } else {
                self.ordering_stage_parallelism
            },
            ordering_queue_limit: if self.ordering_queue_limit == 0 {
                default.ordering_queue_limit
            } else {
                self.ordering_queue_limit
            },
            batch_shards: if self.batch_shards == 0 {
                default.batch_shards
            } else {
                self.batch_shards
            },
            pre_agg_enabled: self.pre_agg_enabled,
            pre_agg_queue_size: if self.pre_agg_queue_size == 0 {
                default.pre_agg_queue_size
            } else {
                self.pre_agg_queue_size
            },
            edge_client_retry_delay_ms: self.edge_client_retry_delay_ms,
            edge_client_retry_max_attempts: if self.edge_client_retry_max_attempts == 0 {
                default.edge_client_retry_max_attempts
            } else {
                self.edge_client_retry_max_attempts
            },
            edge_client_retry_exponential_backoff: self.edge_client_retry_exponential_backoff,
        }
    }
}
