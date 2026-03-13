pub mod edge_gateway;
// ### 修改记录 (2026-03-14)
// - 原因: 需要暴露 FlatBuffers 生成模块
// - 目的: 让 edge_schema / edge_session_schema 复用安全解析
pub mod edge_fbs;
pub mod edge_schema;
pub mod edge_session_schema;
pub mod protocol;
pub mod security;
// ### 修改记录 (2026-03-01)
// - 原因: 需要暴露顺序规则模块给网关与管理接口
// - 目的: 统一规则模型与匹配逻辑的入口
pub mod order_rules;
// ### 修改记录 (2026-03-01)
// - 原因: 需要暴露顺序调度器模块
// - 目的: 让网关侧可直接使用阶段屏障调度
pub mod order_scheduler;
