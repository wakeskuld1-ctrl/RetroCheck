// ### 修改记录 (2026-03-14)
// - 原因: 需要引入 flatc 生成的 Rust 类型
// - 目的: 让 edge_schema / edge_session_schema 使用安全 API
// - 备注: 生成文件位于 OUT_DIR，避免纳入版本控制
// ### 修改记录 (2026-03-15)
// - 原因: Rust 2024 对生成代码新增 warning 提示
// - 目的: 屏蔽 flatc 生成文件中的非业务警告噪音
#![allow(
    dead_code,
    unused_imports,
    non_camel_case_types,
    non_snake_case,
    unsafe_op_in_unsafe_fn,
    mismatched_lifetime_syntaxes,
)]

// ### 修改记录 (2026-03-14)
// - 原因: flatc 输出为 edge_generated.rs
// - 目的: 通过 include! 统一导入生成内容
include!(concat!(env!("OUT_DIR"), "/edge_generated.rs"));
