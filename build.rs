// ### 修改记录 (2026-03-15)
// - 原因: 修复 ANSI 乱码备注
// - 目的: 保证构建脚本注释可读
// ### 修改记录 (2026-03-14)
// - 原因: 需要在构建期生成 FlatBuffers 代码
// - 目的: 使用 flatc 产出可用的 Rust 类型
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ### 修改记录 (2026-03-14)
    // - 原因: 需要生成 gRPC protobuf 代码
    // - 目的: 确保 proto 变更可编译
    tonic_prost_build::compile_protos("proto/transaction.proto")?;

    // ### 修改记录 (2026-03-14)
    // - 原因: 监听 schema 变化触发重生成
    // - 目的: 确保 edge.fbs 变更生效
    println!("cargo:rerun-if-changed=proto/edge.fbs");

    // ### 修改记录 (2026-03-15)
    // - 原因: proto/transaction.proto 变更需要触发重生成
    // - 目的: 确保 AddEdge/RemoveHub 等变更同步到 pb
    println!("cargo:rerun-if-changed=proto/transaction.proto");

    // ### 修改记录 (2026-03-14)
    // - 原因: flatc 输出位置由 Cargo 指定
    // - 目的: 将生成文件写入 OUT_DIR
    let out_dir = std::env::var("OUT_DIR")?;

    // ### 修改记录 (2026-03-14)
    // - 原因: 允许运行时显式指定 flatc 路径
    // - 目的: 兼容 CI/本地环境缺少 PATH 的情况
    let flatc_path = std::env::var("FLATC").unwrap_or_else(|_| "flatc".to_string());

    // ### 修改记录 (2026-03-14)
    // - 原因: 需要调用 flatc 生成 Rust 代码
    // - 目的: 将 edge.fbs 转为 Rust 类型
    // - 备注: 允许通过 FLATC 覆盖默认路径
    let status = std::process::Command::new(&flatc_path)
        .args(["--rust", "-o", &out_dir, "proto/edge.fbs"])
        .status()
        .map_err(|e| format!("flatc 执行失败( path: {flatc_path} ): {e}"))?;
    if !status.success() {
        return Err("flatc 执行失败".into());
    }

    Ok(())
}
