//! ### 修改记录 (2026-02-25)
//! - 原因: 需要最小化 EdgeStore 定义以通过首个测试
//! - 目的: 提供可打开/关闭的 sled 句柄封装

// ### 修改记录 (2026-02-25)
// - 原因: 需要统一错误类型
// - 目的: 让上层测试使用 anyhow 处理错误
// ### 修改记录 (2026-02-26)
// - 原因: 需要在边缘层引入配置读取
// - 目的: 支撑 A/B 清理阈值配置
use anyhow::Result;
// ### 修改记录 (2026-02-26)
// - 原因: 需要引用 EdgeConfig
// - 目的: 将配置注入 EdgeStore
use crate::config::EdgeConfig;
// ### 修改记录 (2026-02-26)
// - 原因: 需要记录清理耗时
// - 目的: 支撑性能测试统计
use std::sync::atomic::{AtomicU64, Ordering};
// ### 修改记录 (2026-02-26)
// - 原因: 需要统计清理耗时
// - 目的: 记录清理执行时长
use std::time::Instant;

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要固定元数据键
/// - 目的: 保证活动指针读写一致
/// ### 修改记录 (2026-02-26)
/// - 原因: A/B 切换需要统一元数据入口
/// - 目的: 支撑活动槽位持久化
const ACTIVE_SLOT_KEY: &str = "edge_active_slot";
/// ### 修改记录 (2026-02-26)
/// - 原因: 需要区分 A/B 槽位前缀
/// - 目的: 让物理键空间隔离
const SLOT0_PREFIX: &[u8] = b"slot0:";
/// ### 修改记录 (2026-02-26)
/// - 原因: 需要区分 A/B 槽位前缀
/// - 目的: 让物理键空间隔离
const SLOT1_PREFIX: &[u8] = b"slot1:";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要统一日志前缀
/// - 目的: 支撑前缀扫描顺序
const LOG_PREFIX: &[u8] = b"log:";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要 TTL 空间前缀
/// - 目的: 区分 TTL 数据与其他键
const TTL_PREFIX: &[u8] = b"ttl:";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要记录物理状态序号
/// - 目的: 作为 HWM 推进前置条件
const STATE_SEQ_KEY: &str = "edge_state_seq";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要记录高水位线
/// - 目的: 支撑重放幂等判断
const HWM_KEY: &str = "edge_hwm";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要 cmd 命名空间前缀
/// - 目的: 前缀扫描覆盖指令数据
const CMD_PREFIX: &[u8] = b"cmd:";
/// ### 修改记录 (2026-02-28)
/// - 原因: 需要 ACK 命名空间前缀
/// - 目的: 指令确认结果可落盘
const ACK_PREFIX: &[u8] = b"ack:";
/// ### 修改记录 (2026-02-26)
/// - 原因: 需要持久化槽位计数
/// - 目的: 支撑清理阈值判断
const SLOT0_COUNT_KEY: &str = "edge_slot0_count";
/// ### 修改记录 (2026-02-26)
/// - 原因: 需要持久化槽位计数
/// - 目的: 支撑清理阈值判断
const SLOT1_COUNT_KEY: &str = "edge_slot1_count";
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要持久化 cmd 断点
/// - 目的: 断电恢复后续扫
const CMD_LAST_KEY: &str = "edge_cmd_last_key";

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要封装 sled::Db
/// - 目的: 统一 EdgeStore 对外接口
pub struct EdgeStore {
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化 KV
    /// - 目的: 支撑断电恢复与前缀扫描测试
    db: sled::Db,
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要保存 A/B 配置
    /// - 目的: 在写入时判断清理阈值
    config: EdgeConfig,
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要记录最近一次清理耗时
    /// - 目的: 供性能测试断言
    last_cleanup_ms: AtomicU64,
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要暴露 cmd 信封信息
/// - 目的: 支撑过期策略判断
pub struct CmdEnvelope {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要记录过期时间
    /// - 目的: 供策略判定是否过期
    pub expire_at_ms: u64,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要记录 payload
    /// - 目的: 供执行器处理
    pub payload: Vec<u8>,
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要 ACK 状态枚举
/// - 目的: 区分执行与丢弃结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStatus {
    /// ### 修改记录 (2026-02-28)
    /// - 原因: 需要执行状态
    /// - 目的: 标识已执行
    Executed,
    /// ### 修改记录 (2026-02-28)
    /// - 原因: 需要丢弃状态
    /// - 目的: 标识已丢弃
    Dropped,
}

/// ### 修改记录 (2026-02-28)
/// - 原因: 需要 ACK 记录结构
/// - 目的: 持久化确认结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckRecord {
    /// ### 修改记录 (2026-02-28)
    /// - 原因: 需要状态字段
    /// - 目的: 标识执行或丢弃
    pub status: AckStatus,
    /// ### 修改记录 (2026-02-28)
    /// - 原因: 需要执行序号
    /// - 目的: 支撑对账与回放
    pub exec_seq: u64,
}

// ### 修改记录 (2026-02-26)
// - 原因: 需要暴露调试统计
// - 目的: 供测试验证计数与耗时
#[cfg(any(test, debug_assertions))]
pub struct EdgeDebugStats {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取槽位计数
    // - 目的: 验证清理是否重置计数
    pub slot0_count: u64,
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取槽位计数
    // - 目的: 验证清理是否重置计数
    pub slot1_count: u64,
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取清理耗时
    // - 目的: 覆盖性能冒烟断言
    pub last_cleanup_ms: u64,
}

impl EdgeStore {
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要从目录初始化存储
    /// - 目的: 允许断电重启复用同一目录
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要默认配置启动
    /// - 目的: 在无配置文件时保持行为稳定
    pub fn open(path: &std::path::Path) -> Result<Self> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要默认配置
        // - 目的: 避免调用方必须提供配置
        Self::open_with_config(path, EdgeConfig::default())
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要通过配置构造 EdgeStore
    /// - 目的: 支撑 A/B 清理阈值由外部控制
    pub fn open_with_config(path: &std::path::Path, config: EdgeConfig) -> Result<Self> {
        // ### 修改记录 (2026-02-26)
        // - 原因: sled 打开可能失败
        // - 目的: 将错误交由上层处理
        let db = sled::open(path)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要保存配置
        // - 目的: 让清理逻辑可读到阈值
        Ok(Self {
            db,
            config,
            // ### 修改记录 (2026-02-26)
            // - 原因: 需要清理耗时默认值
            // - 目的: 让测试可判断是否已记录
            last_cleanup_ms: AtomicU64::new(0),
        })
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要读取配置
    /// - 目的: 供运行器获取默认超时
    pub fn config(&self) -> &EdgeConfig {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要暴露配置引用
        // - 目的: 避免复制
        &self.config
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要从配置文件构造 EdgeStore
    /// - 目的: 满足键计数配置文件管理要求
    pub fn open_with_config_file(
        path: &std::path::Path,
        config_path: &std::path::Path,
    ) -> Result<Self> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要加载配置文件
        // - 目的: 获得清理阈值
        let config = EdgeConfig::load_from_file(config_path)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要复用统一构造逻辑
        // - 目的: 减少配置分支重复
        Self::open_with_config(path, config)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化活动槽位
    /// - 目的: 支撑 A/B 切换断电恢复
    pub fn set_active_slot(&self, slot: u8) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 将 u8 转为单字节 Vec
        self.db.insert(ACTIVE_SLOT_KEY, vec![slot])?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要确保落盘
        // - 目的: 降低断电丢失风险
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取活动槽位
    /// - 目的: 重启后可确定活动实例
    pub fn get_active_slot(&self) -> Result<u8> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取持久化值
        // - 目的: 断电恢复时可取回
        let value = self.db.get(ACTIVE_SLOT_KEY)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 未设置时需默认值
        // - 目的: 保持调用方逻辑简单
        Ok(value.map(|v| v[0]).unwrap_or(0))
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要测试读取调试统计
    /// - 目的: 支撑清理计数与耗时断言
    #[cfg(any(test, debug_assertions))]
    pub fn debug_stats(&self) -> Result<EdgeDebugStats> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取槽位计数
        // - 目的: 验证清理后的计数状态
        let slot0_count = self.get_slot_count(0)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取槽位计数
        // - 目的: 验证清理后的计数状态
        let slot1_count = self.get_slot_count(1)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取清理耗时
        // - 目的: 覆盖性能冒烟断言
        let last_cleanup_ms = self.last_cleanup_ms.load(Ordering::Relaxed);
        Ok(EdgeDebugStats {
            slot0_count,
            slot1_count,
            last_cleanup_ms,
        })
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要在不可恢复时提供默认槽位
    /// - 目的: 让键构造接口保持非 Result
    fn active_slot_or_zero(&self) -> u8 {
        // ### 修改记录 (2026-02-26)
        // - 原因: 读取活动槽位可能失败
        // - 目的: 保持调用方接口稳定
        self.get_active_slot().unwrap_or(0)
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要将槽位映射为前缀
    /// - 目的: 支撑 A/B 槽隔离
    fn slot_prefix_for(slot: u8) -> &'static [u8] {
        // ### 修改记录 (2026-02-26)
        // - 原因: 仅支持双槽位
        // - 目的: 简化槽位映射逻辑
        if slot == 0 {
            SLOT0_PREFIX
        } else {
            SLOT1_PREFIX
        }
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要拼接槽位前缀与命名空间
    /// - 目的: 统一键构造逻辑
    fn prefixed_namespace(&self, slot: u8, namespace: &[u8]) -> Vec<u8> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要组合 slot 与 namespace
        // - 目的: 让 scan_prefix 只遍历当前槽位
        let slot_prefix = Self::slot_prefix_for(slot);
        let mut buf = Vec::with_capacity(slot_prefix.len() + namespace.len());
        buf.extend_from_slice(slot_prefix);
        buf.extend_from_slice(namespace);
        buf
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要拼接槽位前缀、命名空间与后缀
    /// - 目的: 构造完整业务键
    fn prefixed_key(&self, slot: u8, namespace: &[u8], tail: &[u8]) -> Vec<u8> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要组合所有片段
        // - 目的: 得到最终存储键
        let slot_prefix = Self::slot_prefix_for(slot);
        let mut buf = Vec::with_capacity(slot_prefix.len() + namespace.len() + tail.len());
        buf.extend_from_slice(slot_prefix);
        buf.extend_from_slice(namespace);
        buf.extend_from_slice(tail);
        buf
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要持久化槽位计数
    /// - 目的: 让清理阈值判断可落盘
    fn slot_count_key(slot: u8) -> &'static str {
        // ### 修改记录 (2026-02-26)
        // - 原因: 仅支持双槽位
        // - 目的: 简化映射逻辑
        if slot == 0 {
            SLOT0_COUNT_KEY
        } else {
            SLOT1_COUNT_KEY
        }
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要读取槽位计数
    /// - 目的: 判断是否达到清理阈值
    fn get_slot_count(&self, slot: u8) -> Result<u64> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取持久化计数
        // - 目的: 断电后仍保持计数
        let value = self.db.get(Self::slot_count_key(slot))?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要默认 0
        // - 目的: 未写入时避免 panic
        Ok(value
            .map(|v| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&v[0..8]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0))
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要写入槽位计数
    /// - 目的: 让计数可持久化
    fn set_slot_count(&self, slot: u8, value: u64) -> Result<()> {
        // ### 修改记录 (2026-02-26)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化槽位计数
        self.db
            .insert(Self::slot_count_key(slot), value.to_be_bytes().to_vec())?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要尽快落盘
        // - 目的: 断电恢复可继续计数
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要自增槽位计数
    /// - 目的: 在写入时更新计数
    fn increment_slot_count(&self, slot: u8) -> Result<u64> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取旧值
        // - 目的: 计算新计数
        let current = self.get_slot_count(slot)?;
        let next = current.saturating_add(1);
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要写回新值
        // - 目的: 保持计数连续
        self.set_slot_count(slot, next)?;
        Ok(next)
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要清理非活动槽位数据
    /// - 目的: 达到阈值后释放空间
    fn cleanup_slot(&self, slot: u8) -> Result<()> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要统计清理耗时
        // - 目的: 记录清理执行时长
        let started = Instant::now();
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要收集待删除键
        // - 目的: 避免迭代中删除
        let mut keys = Vec::new();
        let slot_prefix = Self::slot_prefix_for(slot);
        for item in self.db.scan_prefix(slot_prefix) {
            let (key, _value) = item?;
            keys.push(key.to_vec());
        }
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要逐个删除
        // - 目的: 清理槽位内全部数据
        for key in keys {
            self.db.remove(key)?;
        }
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要重置计数
        // - 目的: 让阈值判定回到初始
        self.set_slot_count(slot, 0)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要确保删除落盘
        // - 目的: 避免断电后残留
        self.db.flush()?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要记录清理耗时
        // - 目的: 供性能测试读取
        let elapsed_ms = started.elapsed().as_millis() as u64;
        self.last_cleanup_ms.store(elapsed_ms, Ordering::Relaxed);
        Ok(())
    }

    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要在达到阈值时触发清理
    /// - 目的: 实现延迟清理语义
    fn maybe_cleanup_other_slot(&self, active_slot: u8, active_count: u64) -> Result<()> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要获得非活动槽位
        // - 目的: 清理旧槽位数据
        let other_slot = if active_slot == 0 { 1 } else { 0 };
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取旧槽位计数
        // - 目的: 计算比例阈值
        let other_count = self.get_slot_count(other_slot)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 没有旧数据无需清理
        // - 目的: 减少无意义删除
        if other_count == 0 {
            return Ok(());
        }
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要比例阈值计算
        // - 目的: 达到阈值时触发清理
        let threshold = (other_count as f64) * self.config.ab_cleanup_ratio;
        if (active_count as f64) >= threshold {
            return self.cleanup_slot(other_slot);
        }
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要构造可排序的日志键
    /// - 目的: 保障字典序等于时间序
    pub fn log_key(&self, ts: u64, seq: u64) -> Vec<u8> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取活动槽位
        // - 目的: 在键中带上槽位前缀
        let slot = self.active_slot_or_zero();
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要组合后缀
        // - 目的: 保持大端排序
        let mut tail = Vec::with_capacity(16);
        tail.extend_from_slice(&ts.to_be_bytes());
        tail.extend_from_slice(&seq.to_be_bytes());
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要添加槽位前缀
        // - 目的: A/B 数据空间隔离
        self.prefixed_key(slot, LOG_PREFIX, &tail)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入日志记录
    /// - 目的: 为前缀扫描测试提供数据
    pub fn put_log(&self, ts: u64, seq: u64, value: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造日志键
        // - 目的: 保证排序语义
        let key = self.log_key(ts, seq);
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化日志内容
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电恢复可重放
        self.db.flush()?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要更新槽位写入计数
        // - 目的: 判断是否达到清理阈值
        let active_slot = self.get_active_slot()?;
        let active_count = self.increment_slot_count(active_slot)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要可能触发清理
        // - 目的: 达到阈值时清理旧槽
        self.maybe_cleanup_other_slot(active_slot, active_count)?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要前缀扫描日志键
    /// - 目的: 驱动待上传流水遍历
    pub fn scan_log_keys(&self, last_key: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要遍历前缀
        // - 目的: 限定扫描范围
        let mut result = Vec::new();
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要获取当前槽位
        // - 目的: 只扫描活动槽位
        let slot = self.get_active_slot()?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要组合槽位前缀
        // - 目的: 限定扫描范围
        let prefix = self.prefixed_namespace(slot, LOG_PREFIX);
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要容错非法断点
        // - 目的: 非本槽位前缀视为无断点
        let last_key = match last_key {
            Some(key) if key.starts_with(prefix.as_slice()) => Some(key),
            Some(_) => None,
            None => None,
        };
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要断点过滤
        // - 目的: 断电恢复从断点继续
        for item in self.db.scan_prefix(prefix.as_slice()) {
            let (key, _value) = item?;
            // ### 修改记录 (2026-02-25)
            // - 原因: 需要跳过已扫描键
            // - 目的: 避免重复上传
            // ### 修改记录 (2026-02-26)
            // - 原因: 需要满足 clippy collapsible_if
            // - 目的: 简化条件分支并保持原语义
            if let Some(ref last) = last_key
                && key.as_ref() <= last.as_slice()
            {
                continue;
            }
            result.push(key.to_vec());
        }
        Ok(result)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要构造 cmd 键
    /// - 目的: 支撑前缀扫描与断点续扫
    pub fn cmd_key(&self, ts: u64, seq: u64) -> Vec<u8> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取活动槽位
        // - 目的: 在键中带上槽位前缀
        let slot = self.active_slot_or_zero();
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要组合后缀
        // - 目的: 保持大端排序
        let mut tail = Vec::with_capacity(16);
        tail.extend_from_slice(&ts.to_be_bytes());
        tail.extend_from_slice(&seq.to_be_bytes());
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要添加槽位前缀
        // - 目的: A/B 数据空间隔离
        self.prefixed_key(slot, CMD_PREFIX, &tail)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入 cmd 记录
    /// - 目的: 支撑断点重放与过期丢弃
    pub fn put_cmd(&self, ts: u64, seq: u64, expire_at_ms: u64, payload: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造 cmd 键
        // - 目的: 保证排序语义
        let key = self.cmd_key(ts, seq);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造二进制信封
        // - 目的: 前 8 字节存过期时间戳
        let mut value = Vec::with_capacity(8 + payload.len());
        value.extend_from_slice(&expire_at_ms.to_be_bytes());
        value.extend_from_slice(&payload);
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化 cmd 内容
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电恢复可重放
        self.db.flush()?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要更新槽位写入计数
        // - 目的: 判断是否达到清理阈值
        let active_slot = self.get_active_slot()?;
        let active_count = self.increment_slot_count(active_slot)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要可能触发清理
        // - 目的: 达到阈值时清理旧槽
        self.maybe_cleanup_other_slot(active_slot, active_count)?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入原始 cmd 数据
    /// - 目的: 为坏包测试提供注入入口
    pub fn put_cmd_raw(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 原样持久化测试数据
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电后仍可读取
        self.db.flush()?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要更新槽位写入计数
        // - 目的: 判断是否达到清理阈值
        let active_slot = self.get_active_slot()?;
        let active_count = self.increment_slot_count(active_slot)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要可能触发清理
        // - 目的: 达到阈值时清理旧槽
        self.maybe_cleanup_other_slot(active_slot, active_count)?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要前缀扫描 cmd 键
    /// - 目的: 驱动断电重放指令
    pub fn scan_cmd_keys(&self, last_key: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要遍历前缀
        // - 目的: 限定扫描范围
        let mut result = Vec::new();
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要获取当前槽位
        // - 目的: 只扫描活动槽位
        let slot = self.get_active_slot()?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要组合槽位前缀
        // - 目的: 限定扫描范围
        let prefix = self.prefixed_namespace(slot, CMD_PREFIX);
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要容错非法断点
        // - 目的: 非本槽位前缀视为无断点
        let last_key = match last_key {
            Some(key) if key.starts_with(prefix.as_slice()) => Some(key),
            Some(_) => None,
            None => None,
        };
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要断点过滤
        // - 目的: 断电恢复从断点继续
        for item in self.db.scan_prefix(prefix.as_slice()) {
            let (key, _value) = item?;
            // ### 修改记录 (2026-02-25)
            // - 原因: 需要跳过已扫描键
            // - 目的: 避免重复执行
            // ### 修改记录 (2026-02-26)
            // - 原因: 需要满足 clippy collapsible_if
            // - 目的: 简化条件分支并保持原语义
            if let Some(ref last) = last_key
                && key.as_ref() <= last.as_slice()
            {
                continue;
            }
            result.push(key.to_vec());
        }
        Ok(result)
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要读取 cmd 信封
    /// - 目的: 由策略判断是否过期
    pub fn get_cmd_envelope(&self, key: &[u8]) -> Result<Option<CmdEnvelope>> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 可能不存在
        // - 目的: 简化调用方逻辑
        let raw = match self.db.get(key)? {
            Some(value) => value,
            None => return Ok(None),
        };
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要校验长度
        // - 目的: 保护坏包
        if raw.len() < 8 {
            self.db.remove(key)?;
            self.db.flush()?;
            return Ok(None);
        }
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要解析过期时间
        // - 目的: 提供给策略判断
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&raw[0..8]);
        let expire_at_ms = u64::from_be_bytes(ts_bytes);
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要返回 payload
        // - 目的: 供执行器使用
        Ok(Some(CmdEnvelope {
            expire_at_ms,
            payload: raw[8..].to_vec(),
        }))
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取 cmd payload
    /// - 目的: 支撑过期/坏包丢弃
    pub fn get_cmd_payload(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要复用信封读取
        // - 目的: 避免重复解析逻辑
        let envelope = match self.get_cmd_envelope(key)? {
            Some(envelope) => envelope,
            None => return Ok(None),
        };
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要判断过期
        // - 目的: 过期直接丢弃
        if envelope.expire_at_ms <= Self::now_ms() {
            self.remove_cmd(key)?;
            return Ok(None);
        }
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要返回 payload
        // - 目的: 跳过 TTL 头部
        Ok(Some(envelope.payload))
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要删除过期 cmd
    /// - 目的: 避免重复处理与积压
    pub fn remove_cmd(&self, key: &[u8]) -> Result<()> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要删除 cmd 键
        // - 目的: 释放过期数据
        self.db.remove(key)?;
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要尽快落盘
        // - 目的: 避免断电复活
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化 cmd 断点
    /// - 目的: 断电重放从断点继续
    pub fn set_cmd_last_key(&self, key: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化断点
        self.db.insert(CMD_LAST_KEY, key)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电后可恢复
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取 cmd 断点
    /// - 目的: 断电恢复可续扫
    pub fn get_cmd_last_key(&self) -> Result<Option<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 读取持久化值
        // - 目的: 断电恢复可取回
        Ok(self.db.get(CMD_LAST_KEY)?.map(|v| v.to_vec()))
    }

    /// ### 修改记录 (2026-02-28)
    /// - 原因: 需要从 cmd 键派生 ACK 键
    /// - 目的: 保持 A/B 槽位隔离一致
    fn ack_key_from_cmd_key(&self, cmd_key: &[u8]) -> Option<(u8, Vec<u8>)> {
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要匹配 slot0 前缀
        // - 目的: 识别槽位并复用尾部
        let slot0_prefix = self.prefixed_namespace(0, CMD_PREFIX);
        if cmd_key.starts_with(slot0_prefix.as_slice()) {
            // ### 修改记录 (2026-02-28)
            // - 原因: 需要截取尾部
            // - 目的: 复用 ts/seq 排序
            let tail = &cmd_key[slot0_prefix.len()..];
            // ### 修改记录 (2026-02-28)
            // - 原因: 需要拼接 ACK 前缀
            // - 目的: 生成目标 ACK 键
            let ack_key = self.prefixed_key(0, ACK_PREFIX, tail);
            return Some((0, ack_key));
        }
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要匹配 slot1 前缀
        // - 目的: 识别槽位并复用尾部
        let slot1_prefix = self.prefixed_namespace(1, CMD_PREFIX);
        if cmd_key.starts_with(slot1_prefix.as_slice()) {
            // ### 修改记录 (2026-02-28)
            // - 原因: 需要截取尾部
            // - 目的: 复用 ts/seq 排序
            let tail = &cmd_key[slot1_prefix.len()..];
            // ### 修改记录 (2026-02-28)
            // - 原因: 需要拼接 ACK 前缀
            // - 目的: 生成目标 ACK 键
            let ack_key = self.prefixed_key(1, ACK_PREFIX, tail);
            return Some((1, ack_key));
        }
        None
    }

    /// ### 修改记录 (2026-02-28)
    /// - 原因: 需要持久化 ACK 结果
    /// - 目的: 指令闭环可对账
    pub fn put_cmd_ack(&self, cmd_key: &[u8], record: AckRecord) -> Result<()> {
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要从 cmd 键派生 ACK 键
        // - 目的: 与 cmd 同槽位持久化
        let (slot, ack_key) = match self.ack_key_from_cmd_key(cmd_key) {
            Some(value) => value,
            None => return Err(anyhow::anyhow!("invalid cmd key for ack")),
        };
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要编码状态
        // - 目的: 便于反序列化
        let status_byte = match record.status {
            AckStatus::Executed => 1,
            AckStatus::Dropped => 2,
        };
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要写入 ACK 数据
        // - 目的: 固化执行结果
        let mut value = Vec::with_capacity(9);
        value.push(status_byte);
        value.extend_from_slice(&record.exec_seq.to_be_bytes());
        // ### 修改记录 (2026-02-28)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化 ACK 内容
        self.db.insert(ack_key, value)?;
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要尽快落盘
        // - 目的: 断电后仍可读取 ACK
        self.db.flush()?;
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要遵循活跃槽位计数
        // - 目的: 避免误触发清理
        let active_slot = self.get_active_slot()?;
        if slot == active_slot {
            // ### 修改记录 (2026-02-28)
            // - 原因: 需要更新槽位写入计数
            // - 目的: 判断是否达到清理阈值
            let active_count = self.increment_slot_count(active_slot)?;
            // ### 修改记录 (2026-02-28)
            // - 原因: 需要可能触发清理
            // - 目的: 达到阈值时清理旧槽
            self.maybe_cleanup_other_slot(active_slot, active_count)?;
        }
        Ok(())
    }

    /// ### 修改记录 (2026-02-28)
    /// - 原因: 需要读取 ACK 结果
    /// - 目的: 提供执行对账依据
    pub fn get_cmd_ack(&self, cmd_key: &[u8]) -> Result<Option<AckRecord>> {
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要从 cmd 键派生 ACK 键
        // - 目的: 复用命名空间规则
        let (_slot, ack_key) = match self.ack_key_from_cmd_key(cmd_key) {
            Some(value) => value,
            None => return Ok(None),
        };
        // ### 修改记录 (2026-02-28)
        // - 原因: 可能不存在
        // - 目的: 简化调用方逻辑
        let raw = match self.db.get(ack_key.as_slice())? {
            Some(value) => value,
            None => return Ok(None),
        };
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要校验长度
        // - 目的: 保护坏包
        if raw.len() < 9 {
            self.db.remove(ack_key.as_slice())?;
            self.db.flush()?;
            return Ok(None);
        }
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要解析状态
        // - 目的: 还原执行结果
        let status = match raw[0] {
            1 => AckStatus::Executed,
            2 => AckStatus::Dropped,
            _ => {
                self.db.remove(ack_key.as_slice())?;
                self.db.flush()?;
                return Ok(None);
            }
        };
        // ### 修改记录 (2026-02-28)
        // - 原因: 需要解析执行序号
        // - 目的: 供对账/回放使用
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&raw[1..9]);
        let exec_seq = u64::from_be_bytes(buf);
        Ok(Some(AckRecord { status, exec_seq }))
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要构造 TTL 键
    /// - 目的: 支撑 TTL 过期丢弃测试
    fn ttl_key(&self, key: u64) -> Vec<u8> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取活动槽位
        // - 目的: 在键中带上槽位前缀
        let slot = self.active_slot_or_zero();
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要组合后缀
        // - 目的: 保持大端排序
        let mut tail = Vec::with_capacity(8);
        tail.extend_from_slice(&key.to_be_bytes());
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要添加槽位前缀
        // - 目的: A/B 数据空间隔离
        self.prefixed_key(slot, TTL_PREFIX, &tail)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要封装当前时间
    /// - 目的: 统一 TTL 过期判断
    fn now_ms() -> u64 {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要系统时间戳
        // - 目的: 提供毫秒级过期判断
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        since_epoch.as_millis() as u64
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要写入带 TTL 的值
    /// - 目的: 允许过期直接丢弃
    pub fn put_with_ttl(&self, expire_at_ms: u64, payload: Vec<u8>) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要以过期时间作为 key
        // - 目的: 与测试调用保持一致
        let key = self.ttl_key(expire_at_ms);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造二进制信封
        // - 目的: 前 8 字节存过期时间戳
        let mut value = Vec::with_capacity(8 + payload.len());
        value.extend_from_slice(&expire_at_ms.to_be_bytes());
        value.extend_from_slice(&payload);
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化 TTL 值
        self.db.insert(key, value)?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电后仍可判断过期
        self.db.flush()?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要更新槽位写入计数
        // - 目的: 判断是否达到清理阈值
        let active_slot = self.get_active_slot()?;
        let active_count = self.increment_slot_count(active_slot)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要可能触发清理
        // - 目的: 达到阈值时清理旧槽
        self.maybe_cleanup_other_slot(active_slot, active_count)?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取带 TTL 的值
    /// - 目的: 过期时直接丢弃
    pub fn get_with_ttl(&self, key: u64) -> Result<Option<Vec<u8>>> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要构造 TTL 键
        // - 目的: 获取对应记录
        let ttl_key = self.ttl_key(key);
        // ### 修改记录 (2026-02-25)
        // - 原因: 可能不存在
        // - 目的: 简化调用方逻辑
        let raw = match self.db.get(ttl_key.as_slice())? {
            Some(value) => value,
            None => return Ok(None),
        };
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要解析过期时间
        // - 目的: 判断是否过期
        if raw.len() < 8 {
            // ### 修改记录 (2026-02-25)
            // - 原因: 数据异常
            // - 目的: 直接视为过期并清理
            self.db.remove(ttl_key)?;
            self.db.flush()?;
            return Ok(None);
        }
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&raw[0..8]);
        let expire_at_ms = u64::from_be_bytes(ts_bytes);
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要判断过期
        // - 目的: 过期直接丢弃
        if expire_at_ms <= Self::now_ms() {
            self.db.remove(ttl_key)?;
            self.db.flush()?;
            return Ok(None);
        }
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要返回 payload
        // - 目的: 跳过 TTL 头部
        Ok(Some(raw[8..].to_vec()))
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要持久化物理状态序号
    /// - 目的: 确保 HWM 推进有依据
    pub fn persist_state(&self, seq: u64) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: sled 需要 IVec
        // - 目的: 持久化状态序号
        self.db.insert(STATE_SEQ_KEY, seq.to_be_bytes().to_vec())?;
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要尽快落盘
        // - 目的: 断电恢复可校验
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要推进高水位线
    /// - 目的: 标记已安全执行的最大序号
    pub fn set_hwm(&self, seq: u64) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取物理状态序号
        // - 目的: 避免先推进 HWM
        let state_seq = self
            .db
            .get(STATE_SEQ_KEY)?
            .map(|v| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&v[0..8]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0);
        // ### 修改记录 (2026-02-25)
        // - 原因: HWM 不可超过物理状态
        // - 目的: 防止断电半联动
        if seq > state_seq {
            return Err(anyhow::anyhow!("HWM exceeds persisted state"));
        }
        self.db.insert(HWM_KEY, seq.to_be_bytes().to_vec())?;
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要 ACK 推进高水位线
    /// - 目的: 保证幂等且单调递增
    pub fn ack_hwm(&self, seq: u64) -> Result<()> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取物理状态序号
        // - 目的: 避免 ACK 超过落盘状态
        let state_seq = self
            .db
            .get(STATE_SEQ_KEY)?
            .map(|v| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&v[0..8]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0);
        // ### 修改记录 (2026-02-25)
        // - 原因: 不能超过物理状态
        // - 目的: 防止断电半联动
        if seq > state_seq {
            return Err(anyhow::anyhow!("HWM exceeds persisted state"));
        }
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要读取当前 HWM
        // - 目的: 保证单调递增
        let current = self.get_hwm()?;
        if seq <= current {
            return Ok(());
        }
        self.db.insert(HWM_KEY, seq.to_be_bytes().to_vec())?;
        self.db.flush()?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要读取高水位线
    /// - 目的: 供重放幂等判断使用
    pub fn get_hwm(&self) -> Result<u64> {
        // ### 修改记录 (2026-02-25)
        // - 原因: 读取持久化值
        // - 目的: 断电恢复可取回
        let value = self.db.get(HWM_KEY)?;
        Ok(value
            .map(|v| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&v[0..8]);
                u64::from_be_bytes(buf)
            })
            .unwrap_or(0))
    }
}
