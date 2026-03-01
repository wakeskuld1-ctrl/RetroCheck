pub mod actor;
pub mod config;
pub mod coordinator;
/// ### 修改记录 (2026-02-25)
/// - 原因: 需要公开 edge 模块
/// - 目的: 支撑边缘底座测试与实现
pub mod edge;
pub mod engine;
/// ### 修改记录 (2026-03-01)
/// - 原因: 需要公开 Hub 模块
/// - 目的: 暴露 EdgeGateway 给 server 使用
pub mod hub;
/// ### 修改记录 (2026-02-17)
/// - 原因: 需要公开 Raft 模块供测试与上层引用
/// - 目的: 统一对外暴露 raft 子模块入口
pub mod raft;
pub mod wal;

pub mod pb {
    tonic::include_proto!("transaction");
}
