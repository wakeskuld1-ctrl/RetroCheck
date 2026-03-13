// ### 修改记录 (2026-03-14)
// - 原因: 需要引入 flatc 生成的 Rust 类型
// - 目的: 让 edge_schema / edge_session_schema 使用安全 API
// - 备注: 生成文件位于 OUT_DIR，避免纳入版本控制
#![allow(dead_code, unused_imports, non_camel_case_types, non_snake_case)]

// ### 修改记录 (2026-03-14)
// - 原因: flatc 输出为 edge_generated.rs
// - 目的: 通过 include! 统一导入生成内容
include!(concat!(env!("OUT_DIR"), "/edge_generated.rs"));
