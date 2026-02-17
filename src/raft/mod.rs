//! ### 修改记录 (2026-02-17)
//! - 原因: 需要集中放置 Raft 相关模块
//! - 目的: 统一对外暴露 Raft 类型与组件入口

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要预留 Raft 网络层
/// - 目的: 后续接入节点间通信
pub mod network;
/// ### 修改记录 (2026-02-17)
/// - 原因: 需要暴露 RaftNode 封装
/// - 目的: 提供 Raft 核心启动入口
pub mod raft_node;
/// ### 修改记录 (2026-02-17)
/// - 原因: 需要对外暴露路由入口
/// - 目的: 统一写入转发与 Leader 判断逻辑
pub mod router;
/// ### 修改记录 (2026-02-17)
/// - 原因: 需要对外暴露状态机实现
/// - 目的: 支持 Raft apply 的执行入口
pub mod state_machine;
/// ### 修改记录 (2026-02-17)
/// - 原因: 需要对外暴露 RaftStore
/// - 目的: 为测试与状态恢复提供持久化入口
pub mod store;
/// ### 修改记录 (2026-02-17)
/// - 原因: 需要对外暴露请求与响应类型
/// - 目的: 让上层调用使用稳定的 Request/Response
pub mod types;

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要提供快照调度入口
/// - 目的: 支撑 VACUUM INTO 快照生成
pub mod snapshot {
    use anyhow::{Result, anyhow};
    use std::path::Path;

    use crate::raft::router::Router;

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要快照调度器
    /// - 目的: 定期检查并触发 VACUUM INTO
    pub struct SnapshotScheduler {
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要访问写入与元数据
        /// - 目的: 复用 Router 路由能力
        router: Router,
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要输出快照目录
        /// - 目的: 统一快照文件落盘位置
        snapshot_dir: String,
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要时间窗口判断
        /// - 目的: 以秒为单位控制触发频率
        window_secs: u64,
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要记录快照索引
        /// - 目的: 持久化快照边界
        last_index: u64,
    }

    impl SnapshotScheduler {
        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要初始化调度器
        /// - 目的: 统一参数注入
        pub fn new(
            router: Router,
            snapshot_dir: String,
            window_secs: u64,
            last_index: u64,
        ) -> Self {
            Self {
                router,
                snapshot_dir,
                window_secs,
                last_index,
            }
        }

        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要单次触发入口
        /// - 目的: 便于测试与调度循环复用
        pub async fn tick_once(&self) -> Result<()> {
            // ### 修改记录 (2026-02-17)
            // - 原因: 需要确保快照目录存在
            // - 目的: 避免 VACUUM INTO 失败
            std::fs::create_dir_all(&self.snapshot_dir)?;

            // ### 修改记录 (2026-02-17)
            // - 原因: 需要读取 SQLite 当前时间
            // - 目的: 以数据库时间为权威来源
            let now = self.router.current_unix_timestamp().await?;
            let last_snapshot_at = self.router.get_meta_u64("last_snapshot_at").await?;
            let last_write_at = self.router.get_meta_u64("last_write_at").await?;

            // ### 修改记录 (2026-02-17)
            // - 原因: 需要避免无写入也生成快照
            // - 目的: 减少无效 VACUUM
            if last_write_at == 0 {
                return Ok(());
            }

            // ### 修改记录 (2026-02-17)
            // - 原因: 需要控制快照频率
            // - 目的: 按时间窗口触发
            if now.saturating_sub(last_snapshot_at) < self.window_secs {
                return Ok(());
            }

            // ### 修改记录 (2026-02-17)
            // - 原因: 需要保证有新的写入
            // - 目的: 避免重复快照
            if last_write_at <= last_snapshot_at {
                return Ok(());
            }

            let file_name = format!("snapshot_{}.db", now);
            let path = Path::new(&self.snapshot_dir).join(file_name);
            let snapshot_path = path
                .to_str()
                .ok_or_else(|| anyhow!("Invalid snapshot path"))?
                .to_string();

            // ### 修改记录 (2026-02-17)
            // - 原因: 需要执行 VACUUM INTO
            // - 目的: 生成快照文件
            self.router.vacuum_into(snapshot_path).await?;

            // ### 修改记录 (2026-02-17)
            // - 原因: 需要记录快照时间
            // - 目的: 更新调度基准
            self.router
                .update_meta("last_snapshot_at".to_string(), now.to_string())
                .await?;

            // ### 修改记录 (2026-02-17)
            // - 原因: 需要记录快照索引
            // - 目的: 保持索引持久化一致
            self.router
                .update_meta(
                    "last_snapshot_index".to_string(),
                    self.last_index.to_string(),
                )
                .await?;

            // ### 修改记录 (2026-02-17)
            // - 原因: 快照完成后需要切换 WAL
            // - 目的: 控制日志膨胀
            self.router.rotate_wal().await?;

            Ok(())
        }

        /// ### 修改记录 (2026-02-17)
        /// - 原因: 需要在测试中读取元数据
        /// - 目的: 验证快照生成后的状态
        pub async fn get_meta_u64(&self, key: &str) -> Result<u64> {
            self.router.get_meta_u64(key).await
        }
    }
}
