// ### 修改记录 (2026-03-14)
// - 原因: 需要在构建时生成 FlatBuffers 代码
// - 目的: 通过 flatc 生成安全 API，替换手工解析
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ### 修改记录 (2026-03-14)
    // - 原因: 保持 gRPC protobuf 生成逻辑不变
    // - 目的: 避免影响现有 proto 编译流程
    tonic_prost_build::compile_protos("proto/transaction.proto")?;

    // ### 修改记录 (2026-03-14)
    // - 原因: 需要在 schema 变更时触发重建
    // - 目的: 确保 edge.fbs 更新后自动重新生成
    println!("cargo:rerun-if-changed=proto/edge.fbs");

    // ### 修改记录 (2026-03-14)
    // - 原因: flatc 生成文件应放在 OUT_DIR
    // - 目的: 避免将生成文件纳入版本控制
    let out_dir = std::env::var("OUT_DIR")?;

    // ### 修改记录 (2026-03-14)
    // - 原因: 需要支持从环境变量指定 flatc 路径
    // - 目的: 解决 CI/本机未在 PATH 中配置 flatc 的问题
    let flatc_path =
        std::env::var("FLATC").unwrap_or_else(|_| "flatc".to_string());

    // ### 修改记录 (2026-03-14)
    // - 原因: 需要调用 flatc 生成 Rust 类型
    // - 目的: 提供安全解析所需的类型定义
    // - 备注: 优先使用 FLATC 环境变量，其次回退到 PATH 中的 flatc
    let status = std::process::Command::new(&flatc_path)
        .args(["--rust", "-o", &out_dir, "proto/edge.fbs"])
        .status()
        .map_err(|e| format!("flatc 执行失败（path: {flatc_path}）：{e}"))?;
    if !status.success() {
        return Err("flatc 生成失败".into());
    }

    Ok(())
}
