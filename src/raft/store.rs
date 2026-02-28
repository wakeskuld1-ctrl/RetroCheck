//! ### 修改记录 (2026-02-17)
//! - 原因: 需要持久化 Raft 元数据
//! - 目的: 提供节点重启后的恢复能力

// ### 修改记录 (2026-02-27)
// - 原因: 需要测试辅助写入
// - 目的: 提供简化入口
use anyhow::Result;
// ### 修改记录 (2026-02-27)
// - 原因: 需要构造测试日志
// - 目的: 为 append_for_test 提供类型
use openraft::Entry;
// ### 修改记录 (2026-02-26)
// - 原因: 需要接入 OpenRaft 日志存储接口
// - 目的: 实现 RaftLogStorage 与 RaftLogReader
use openraft::storage::{LogFlushed, LogState, RaftLogReader, RaftLogStorage};
// ### 修改记录 (2026-02-26)
// - 原因: 需要使用 Raft 核心类型
// - 目的: 让存储与类型配置对齐
use crate::raft::types::{NodeId, TypeConfig};
use openraft::{AnyError, LogId, StorageError, StorageIOError, Vote};
use std::ops::{Bound, RangeBounds};

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要封装 sled 的存储句柄
/// - 目的: 统一 RaftStore 对外接口
#[derive(Clone)]
pub struct RaftStore {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要持久化 KV
    /// - 目的: 承载 Raft 元数据与日志索引
    #[allow(dead_code)]
    db: sled::Db,
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要分离日志树
    /// - 目的: 避免日志与元数据混存
    log_tree: sled::Tree,
    /// ### 修改记录 (2026-02-26)
    /// - 原因: 需要保存元数据
    /// - 目的: 存放 last_applied/vote 等信息
    meta_tree: sled::Tree,
}

impl RaftStore {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要从目录初始化存储
    /// - 目的: 允许节点重启后复用同一目录
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let db = sled::open(path)?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要独立日志树
        // - 目的: 提供有序日志存储
        let log_tree = db.open_tree("raft_log")?;
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要独立元数据树
        // - 目的: 保存 vote/last_applied 等元数据
        let meta_tree = db.open_tree("raft_meta")?;
        Ok(Self {
            db,
            log_tree,
            meta_tree,
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要记录 last_applied
    /// - 目的: 防止状态机重复回放
    /// ### 修改记录 (2026-02-17)
    /// - 原因: sled::insert 需要 IVec 类型
    /// - 目的: 将固定字节数组转为 Vec<u8>
    pub fn set_last_applied(&self, value: u64) -> Result<()> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要统一写入元数据树
        // - 目的: 避免与日志混存
        self.meta_tree
            .insert("last_applied", value.to_be_bytes().to_vec())?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取 last_applied
    /// - 目的: 支持恢复后正确定位
    pub fn get_last_applied(&self) -> Result<u64> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要从元数据树读取
        // - 目的: 保持数据隔离
        if let Some(bytes) = self.meta_tree.get("last_applied")? {
            let mut buffer = [0u8; 8];
            buffer.copy_from_slice(&bytes);
            Ok(u64::from_be_bytes(buffer))
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要记录快照索引
    /// - 目的: 支持快照后截断日志
    pub fn set_last_snapshot_index(&self, value: u64) -> Result<()> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要从元数据树写入
        // - 目的: 与日志数据隔离
        self.meta_tree
            .insert("last_snapshot_index", value.to_be_bytes().to_vec())?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取快照索引
    /// - 目的: 支持重启后恢复快照边界
    pub fn get_last_snapshot_index(&self) -> Result<u64> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要从元数据树读取
        // - 目的: 避免与日志混存
        if let Some(bytes) = self.meta_tree.get("last_snapshot_index")? {
            let mut buffer = [0u8; 8];
            buffer.copy_from_slice(&bytes);
            Ok(u64::from_be_bytes(buffer))
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-27)
    /// - 原因: 测试需要最小追加入口
    /// - 目的: 避免依赖 LogFlushed 构造
    pub async fn append_for_test(&mut self, entries: Vec<Entry<TypeConfig>>) -> Result<()> {
        for entry in entries {
            let key = Self::log_key(entry.log_id.index).to_vec();
            let value = bincode::serialize(&entry)?;
            self.log_tree.insert(key, value)?;
        }
        Ok(())
    }
}

impl RaftStore {
    // ### 修改记录 (2026-02-26)
    // - 原因: 需要稳定的日志键格式
    // - 目的: 保证 sled 中日志有序
    fn log_key(index: u64) -> [u8; 8] {
        index.to_be_bytes()
    }

    // ### 修改记录 (2026-02-26)
    // - 原因: 需要统一元数据键名
    // - 目的: 避免魔法字符串散落
    fn meta_key_vote() -> &'static str {
        "vote"
    }

    // ### 修改记录 (2026-02-26)
    // - 原因: 需要记录 last_purged
    // - 目的: 支持日志回收后的状态恢复
    fn meta_key_last_purged() -> &'static str {
        "last_purged"
    }

    fn to_read_error<E: std::fmt::Display>(e: E) -> StorageError<NodeId> {
        StorageError::IO {
            source: StorageIOError::read(AnyError::error(e.to_string())),
        }
    }

    fn to_write_error<E: std::fmt::Display>(e: E) -> StorageError<NodeId> {
        StorageError::IO {
            source: StorageIOError::write(AnyError::error(e.to_string())),
        }
    }

    // ### 修改记录 (2026-02-26)
    // - 原因: 需要把 range bound 转换为 sled bound
    // - 目的: 便于日志范围读取
    fn bound_to_vec(bound: Bound<&u64>) -> Bound<Vec<u8>> {
        match bound {
            Bound::Included(value) => Bound::Included(Self::log_key(*value).to_vec()),
            Bound::Excluded(value) => Bound::Excluded(Self::log_key(*value).to_vec()),
            Bound::Unbounded => Bound::Unbounded,
        }
    }

    // ### 修改记录 (2026-02-26)
    // - 原因: 需要读取 last_purged
    // - 目的: 提供日志状态返回
    fn read_last_purged(&self) -> Result<Option<LogId<NodeId>>> {
        if let Some(bytes) = self.meta_tree.get(Self::meta_key_last_purged())? {
            let log_id: LogId<NodeId> = bincode::deserialize(&bytes)?;
            Ok(Some(log_id))
        } else {
            Ok(None)
        }
    }

    // ### 修改记录 (2026-02-26)
    // - 原因: 需要持久化 last_purged
    // - 目的: 支持日志回收后恢复
    fn write_last_purged(&self, log_id: LogId<NodeId>) -> Result<()> {
        let bytes = bincode::serialize(&log_id)?;
        self.meta_tree.insert(Self::meta_key_last_purged(), bytes)?;
        Ok(())
    }
}

impl RaftLogReader<TypeConfig> for RaftStore {
    async fn try_get_log_entries<
        RB: RangeBounds<u64> + Clone + std::fmt::Debug + openraft::OptionalSend,
    >(
        &mut self,
        range: RB,
    ) -> Result<Vec<openraft::Entry<TypeConfig>>, StorageError<NodeId>> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要将 u64 range 转换为 sled range
        // - 目的: 支持范围读取日志
        let start = Self::bound_to_vec(range.start_bound());
        let end = Self::bound_to_vec(range.end_bound());
        let mut entries = Vec::new();
        for item in self.log_tree.range((start, end)) {
            let (_, value) = item.map_err(Self::to_read_error)?;
            let entry: openraft::Entry<TypeConfig> =
                bincode::deserialize(&value).map_err(Self::to_read_error)?;
            entries.push(entry);
        }
        Ok(entries)
    }
}

impl RaftLogStorage<TypeConfig> for RaftStore {
    type LogReader = RaftStore;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let last_purged_log_id = self.read_last_purged().map_err(Self::to_read_error)?;

        let last_log_id = if let Some(item) = self.log_tree.iter().next_back() {
            let (_, value) = item.map_err(Self::to_read_error)?;
            let entry: openraft::Entry<TypeConfig> =
                bincode::deserialize(&value).map_err(Self::to_read_error)?;
            Some(entry.log_id)
        } else {
            last_purged_log_id
        };
        Ok(LogState {
            last_purged_log_id,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要提供独立读视图
        // - 目的: 避免写读互相阻塞
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要持久化投票
        // - 目的: 确保重启后投票不丢失
        let bytes = bincode::serialize(vote).map_err(Self::to_write_error)?;
        self.meta_tree
            .insert(Self::meta_key_vote(), bytes)
            .map_err(Self::to_write_error)?;
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要读取持久化投票
        // - 目的: 支持重启后恢复选举状态
        if let Some(bytes) = self
            .meta_tree
            .get(Self::meta_key_vote())
            .map_err(Self::to_read_error)?
        {
            let vote = bincode::deserialize(&bytes).map_err(Self::to_read_error)?;
            Ok(Some(vote))
        } else {
            Ok(None)
        }
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = openraft::Entry<TypeConfig>> + openraft::OptionalSend,
        I::IntoIter: openraft::OptionalSend,
    {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要批量写入日志
        // - 目的: 支持 AppendEntries 追加
        for entry in entries {
            let key = Self::log_key(entry.log_id.index).to_vec();
            let value = bincode::serialize(&entry).map_err(Self::to_write_error)?;
            self.log_tree
                .insert(key, value)
                .map_err(Self::to_write_error)?;
        }
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要通知日志已写入
        // - 目的: 满足 OpenRaft 回调要求
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要截断冲突日志
        // - 目的: 满足 Raft 日志回滚要求
        let start_key = Self::log_key(log_id.index).to_vec();
        let keys: Vec<Vec<u8>> = self
            .log_tree
            .range(start_key..)
            .map(|item| item.map(|(key, _)| key.to_vec()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Self::to_read_error)?;
        for key in keys {
            self.log_tree.remove(key).map_err(Self::to_write_error)?;
        }
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        // ### 修改记录 (2026-02-26)
        // - 原因: 需要清理已持久化快照之前的日志
        // - 目的: 降低存储占用
        let end_key = Self::log_key(log_id.index).to_vec();
        let keys: Vec<Vec<u8>> = self
            .log_tree
            .range(..=end_key)
            .map(|item| item.map(|(key, _)| key.to_vec()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Self::to_read_error)?;
        for key in keys {
            self.log_tree.remove(key).map_err(Self::to_write_error)?;
        }
        self.write_last_purged(log_id)
            .map_err(Self::to_write_error)?;
        Ok(())
    }
}
