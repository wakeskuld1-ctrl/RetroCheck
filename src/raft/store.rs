//! ### 修改记录 (2026-02-17)
//! - 原因: 需要持久化 Raft 元数据
//! - 目的: 提供节点重启后的恢复能力

use anyhow::Result;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要封装 sled 的存储句柄
/// - 目的: 统一 RaftStore 对外接口
pub struct RaftStore {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要持久化 KV
    /// - 目的: 承载 Raft 元数据与日志索引
    db: sled::Db,
}

impl RaftStore {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要从目录初始化存储
    /// - 目的: 允许节点重启后复用同一目录
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            db: sled::open(path)?,
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要记录 last_applied
    /// - 目的: 防止状态机重复回放
    /// ### 修改记录 (2026-02-17)
    /// - 原因: sled::insert 需要 IVec 类型
    /// - 目的: 将固定字节数组转为 Vec<u8>
    pub fn set_last_applied(&self, value: u64) -> Result<()> {
        self.db
            .insert("last_applied", value.to_be_bytes().to_vec())?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取 last_applied
    /// - 目的: 支持恢复后正确定位
    pub fn get_last_applied(&self) -> Result<u64> {
        if let Some(bytes) = self.db.get("last_applied")? {
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
        self.db
            .insert("last_snapshot_index", value.to_be_bytes().to_vec())?;
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取快照索引
    /// - 目的: 支持重启后恢复快照边界
    pub fn get_last_snapshot_index(&self) -> Result<u64> {
        if let Some(bytes) = self.db.get("last_snapshot_index")? {
            let mut buffer = [0u8; 8];
            buffer.copy_from_slice(&bytes);
            Ok(u64::from_be_bytes(buffer))
        } else {
            Ok(0)
        }
    }
}
