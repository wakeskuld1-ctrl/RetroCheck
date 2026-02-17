use anyhow::Result;
use check_program::config::{ClusterConfig, ConsistencyMode};
use check_program::coordinator::MultiDbCoordinator;
use std::env;
use std::fs;
use std::path::Path;

/// Distributed Transaction Client
///
/// This application serves as the entry point for testing and demonstrating
/// the distributed transaction capabilities of the system.
///
/// It performs the following steps:
/// 1. Initializes the client environment.
/// 2. Configures the cluster topology (Master + Slaves).
/// 3. Establishes connections to the cluster via the Coordinator.
/// 4. Executes a series of distributed operations:
///    - Schema Migration (DDL)
///    - Atomic Insert (2PC)
///    - Atomic Update (2PC with versioning)
///    - Consistency Verification
#[tokio::main]
async fn main() -> Result<()> {
    // 1. Prepare environment
    // Ensure the data directory exists for local artifacts (if any).
    let data_dir = Path::new("data");
    if !data_dir.exists() {
        fs::create_dir(data_dir)?;
    }

    // Cleanup local WAL if it exists from a previous run.
    // Note: This is for the Coordinator's local log, not the storage engines.
    let _ = fs::remove_file("data/coordinator_wal.db");

    // ### 修改记录 (2026-02-17)
    // - 原因: 需要通过 CLI 参数控制场景与一致性模式
    // - 目的: 支持脚本驱动的故障注入与验证流程
    let mut scenario = "full".to_string();
    // ### 修改记录 (2026-02-17)
    // - 原因: 默认模式需要可配置
    // - 目的: 允许切换 Quorum/Strong
    let mut mode_text = "quorum".to_string();
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要在 Prepare/Commit 间隙插入暂停
    // - 目的: 允许故障注入窗口可控
    let mut pause_before_commit_ms: Option<u64> = None;
    // ### 修改记录 (2026-02-17)
    // - 原因: 验证脚本需要可变地址输入
    // - 目的: 允许覆盖默认 Master/Slave 地址
    let mut master_addr = "http://127.0.0.1:50051".to_string();
    let mut slave_addrs_raw = "http://127.0.0.1:50052,http://127.0.0.1:50053".to_string();
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要解析 CLI 参数
    // - 目的: 避免引入新依赖以保持工程简洁
    let mut args_iter = env::args().skip(1);
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--scenario" => {
                if let Some(value) = args_iter.next() {
                    scenario = value;
                }
            }
            "--mode" => {
                if let Some(value) = args_iter.next() {
                    mode_text = value;
                }
            }
            "--pause-before-commit-ms" => {
                if let Some(value) = args_iter.next() {
                    pause_before_commit_ms = value.parse::<u64>().ok();
                }
            }
            "--master-addr" => {
                if let Some(value) = args_iter.next() {
                    master_addr = value;
                }
            }
            "--slave-addrs" => {
                if let Some(value) = args_iter.next() {
                    slave_addrs_raw = value;
                }
            }
            _ => {}
        }
    }

    // ### 修改记录 (2026-02-17)
    // - 原因: 一致性模式需要文本解析
    // - 目的: 将 CLI 输入转换为枚举
    let mode = match mode_text.as_str() {
        "strong" => ConsistencyMode::Strong,
        "quorum" => ConsistencyMode::Quorum,
        _ => {
            println!("Unknown mode '{}', fallback to quorum.", mode_text);
            ConsistencyMode::Quorum
        }
    };
    println!(">>> Starting Client in {:?} Mode (gRPC) <<<", mode);

    // 2. Configure Cluster
    // We define the static topology here. In a real system, this might come from
    // a service discovery mechanism (e.g., etcd, Consul).
    // Master: 50051 (SQLite)
    // Slaves: 50052 (SQLite), 50053 (SQLite)
    // ### 修改记录 (2026-02-17)
    // - 原因: slave 地址来自 CLI 字符串
    // - 目的: 支持脚本通过逗号分隔传参
    let slave_addrs_vec: Vec<String> = slave_addrs_raw
        .split(',')
        .map(|addr| addr.trim().to_string())
        .filter(|addr| !addr.is_empty())
        .collect();
    let slave_addrs_ref: Vec<&str> = slave_addrs_vec.iter().map(|addr| addr.as_str()).collect();
    let config = ClusterConfig::new(&master_addr, slave_addrs_ref, mode);
    // ### 修改记录 (2026-02-17)
    // - 原因: ClusterConfig 无 cluster_id 字段导致编译失败
    // - 目的: 使用 master_addr 作为连接标识输出
    println!("Initializing Cluster Connection: {}", config.master_addr);

    // 3. Initialize Coordinator
    // This establishes gRPC connections to all nodes.
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要透传提交前暂停参数
    // - 目的: 支持故障注入脚本控制
    let mut coordinator = MultiDbCoordinator::new(config, pause_before_commit_ms).await?;
    println!("Coordinator connected successfully.");

    // 4. Execute Operations
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要区分 full 与 verify-only
    // - 目的: 支持脚本在重启后只做一致性校验
    if scenario == "verify-only" {
        println!("Verifying Consistency...");
        coordinator.verify_consistency("accounts").await?;
        return Ok(());
    }

    // Step A: Schema Migration
    // Create the 'accounts' table on all nodes.
    println!("Creating business tables...");
    coordinator
        .execute_schema_migration(
            "CREATE TABLE IF NOT EXISTS accounts (
            id TEXT PRIMARY KEY,
            balance INTEGER,
            version INTEGER DEFAULT 1
        )",
        )
        .await?;

    // Step B: Atomic Insert Transaction
    // Insert a new user record. This uses 2PC to ensure it exists on at least Quorum nodes.
    println!("Executing Atomic Transaction 1 (Insert)...");
    match coordinator
        .atomic_write(
            "INSERT INTO accounts (id, balance, version) VALUES (?1, ?2, ?3)",
            vec!["user_001".to_string(), "1000".to_string(), "1".to_string()],
        )
        .await
    {
        Ok(_) => println!("Transaction 1 COMMITTED."),
        Err(e) => println!("Transaction 1 FAILED: {}", e),
    }

    // Step C: Atomic Update Transaction
    // Deduct balance and increment version.
    // Version check ensures Optimistic Concurrency Control (OCC).
    println!("Executing Atomic Transaction 2 (Update with Version)...");
    let current_version = 1;
    match coordinator.atomic_write(
        "UPDATE accounts SET balance = balance - 100, version = version + 1 WHERE id = ?1 AND version = ?2",
        vec!["user_001".to_string(), current_version.to_string()]
    ).await {
        Ok(_) => println!("Transaction 2 COMMITTED."),
        Err(e) => println!("Transaction 2 FAILED: {}", e),
    }

    // Step D: Verify Consistency
    // Check that all reachable nodes have the same data version.
    println!("Verifying Consistency...");
    coordinator.verify_consistency("accounts").await?;

    Ok(())
}
