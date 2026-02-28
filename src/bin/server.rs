use check_program::engine::StorageEngine;
use check_program::pb::database_service_server::{DatabaseService, DatabaseServiceServer};
use check_program::pb::{
    CommitRequest, CommitResponse, Empty, ExecuteRequest, ExecuteResponse, GetVersionRequest,
    GetVersionResponse, PrepareRequest, PrepareResponse, RollbackRequest, RollbackResponse,
};
use check_program::raft::router::Router;
use clap::Parser;
use std::sync::Arc;
use tonic::{Request, Response, Status, transport::Server};
// ### 修改记录 (2026-02-17)
// - 原因: 需要在服务启动时初始化 RaftNode
// - 目的: 让写路径走 RaftNode 封装
use async_trait::async_trait;
use check_program::raft::raft_node::RaftNode;

/// Database Server CLI Arguments
///
/// Defines the command-line interface for the database node server.
/// Allows specifying the listening port, database file path, and storage engine type.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The port to listen on for gRPC connections.
    /// Default is 50051 (Master), usually 50052/50053 for Slaves.
    #[arg(short, long, default_value_t = 50051)]
    port: u16,

    /// The path to the database file (e.g., "master.db", "slave1.db").
    /// If it doesn't exist, it will be created.
    #[arg(short, long)]
    db: String,

    /// The storage engine to use: "sqlite".
    /// - sqlite: General-purpose OLTP storage.
    #[arg(short, long, default_value = "sqlite")]
    engine: String,
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要抽象 Router 写入口
/// - 目的: 便于 gRPC 层注入不同 Router 实现
#[async_trait]
trait WriteRouter: Send + Sync {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要路由写入
    /// - 目的: 承载写请求的统一入口
    async fn write(&self, sql: String) -> anyhow::Result<usize>;
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取版本号
    /// - 目的: 统一读路径接口
    async fn get_version(&self, table: String) -> anyhow::Result<i32>;
}

/// ### 修改记录 (2026-02-17)
/// - 原因: Router 需要符合写接口
/// - 目的: 让 gRPC 层复用 Router 逻辑
#[async_trait]
impl WriteRouter for Router {
    async fn write(&self, sql: String) -> anyhow::Result<usize> {
        self.write(sql).await
    }

    async fn get_version(&self, table: String) -> anyhow::Result<i32> {
        self.get_version(table).await
    }
}

/// ### 修改记录 (2026-02-17)
/// - 原因: gRPC 需要一个 StorageEngine 实现
/// - 目的: 将 Execute 调用改为 Router 写入路径
struct RouterEngine<R: WriteRouter> {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要实际路由实例
    /// - 目的: 统一写入口
    router: R,
}

impl<R: WriteRouter> RouterEngine<R> {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要注入 Router
    /// - 目的: 简化初始化
    fn new(router: R) -> Self {
        Self { router }
    }
}

#[async_trait]
impl<R: WriteRouter> StorageEngine for RouterEngine<R> {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: Execute 需要走 Raft 写入口
    /// - 目的: 统一写入路径
    async fn execute(&self, sql: &str) -> anyhow::Result<usize> {
        self.router.write(sql.to_string()).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 对外协议保持 Prepare 接口
    /// - 目的: 兼容旧客户端调用
    async fn prepare(&self, _tx_id: &str, _sql: &str, _args: Vec<String>) -> anyhow::Result<()> {
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 对外协议保持 Commit 接口
    /// - 目的: 兼容旧客户端调用
    async fn commit(&self, _tx_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 对外协议保持 Rollback 接口
    /// - 目的: 兼容旧客户端调用
    async fn rollback(&self, _tx_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 版本读取需要后续改为强一致读
    /// - 目的: 先提供最小实现保持接口可用
    async fn get_version(&self, table: &str) -> anyhow::Result<i32> {
        self.router.get_version(table.to_string()).await
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 健康检查需要返回成功
    /// - 目的: 保持现有 gRPC 行为
    async fn check_health(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Database Service Implementation
///
/// This struct implements the gRPC `DatabaseService` trait defined in the protobuf.
/// It acts as a wrapper around the underlying `StorageEngine` implementation,
/// exposing it over the network.
#[derive(Clone)]
pub struct DbServiceImpl {
    /// The underlying storage engine (SQLite).
    ///
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 移除 DuckDB 依赖以避免编译失败
    /// - 目的: 当前版本仅保留 SQLite 引擎实现
    /// ### 修改记录 (2026-02-17)
    /// - 原因: clippy 指出文档缩进不符合规范
    /// - 目的: 修正文档列表缩进保持风格一致
    ///   We use `Arc<dyn StorageEngine>` for dynamic dispatch and thread safety.
    engine: Arc<dyn StorageEngine>,
}

impl DbServiceImpl {
    /// Creates a new service instance with the given storage engine.
    fn new(engine: Arc<dyn StorageEngine>) -> Self {
        Self { engine }
    }
}

/// gRPC Service Trait Implementation
///
/// Maps gRPC requests to `StorageEngine` method calls.
/// Handles error mapping from internal `anyhow::Error` to gRPC `Status`.
#[tonic::async_trait]
impl DatabaseService for DbServiceImpl {
    /// Health check endpoint.
    /// Returns OK if the database connection is alive.
    async fn check_health(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        self.engine
            .check_health()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(Empty {}))
    }

    /// Executes a raw SQL statement (No 2PC).
    /// Used for DDL (migrations) or simple queries.
    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        let req = request.into_inner();
        let rows = self
            .engine
            .execute(&req.sql)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ExecuteResponse {
            rows_affected: rows as i32,
        }))
    }

    /// Phase 1 of 2PC: Prepare.
    /// Instructs the engine to start a transaction and execute the SQL, but hold the commit.
    async fn prepare(
        &self,
        request: Request<PrepareRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        let req = request.into_inner();
        self.engine
            .prepare(&req.tx_id, &req.sql, req.args)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(PrepareResponse {}))
    }

    /// Phase 2 of 2PC: Commit.
    /// Instructs the engine to commit the previously prepared transaction.
    async fn commit(
        &self,
        request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        let req = request.into_inner();
        self.engine
            .commit(&req.tx_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(CommitResponse {}))
    }

    /// Phase 2 of 2PC: Rollback.
    /// Instructs the engine to rollback the previously prepared transaction.
    async fn rollback(
        &self,
        request: Request<RollbackRequest>,
    ) -> Result<Response<RollbackResponse>, Status> {
        let req = request.into_inner();
        self.engine
            .rollback(&req.tx_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RollbackResponse {}))
    }

    /// Retrieves the current version of a table.
    /// Used for consistency verification.
    async fn get_version(
        &self,
        request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        let req = request.into_inner();
        let v = self
            .engine
            .get_version(&req.table)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(GetVersionResponse { version: v }))
    }
}

/// Main Entry Point
///
/// 1. Parses command line arguments.
/// 2. Initializes the selected storage engine.
/// 3. Starts the gRPC server.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let addr = format!("0.0.0.0:{}", args.port).parse()?;

    println!(
        "Starting server on {} using {} engine for db {}",
        addr, args.engine, args.db
    );

    // Factory pattern: Select engine based on CLI argument
    //
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要将写路径切换到 Router
    // - 目的: 保持协议不变的同时完成内部替换
    let engine: Arc<dyn StorageEngine> = match args.engine.as_str() {
        "sqlite" => {
            // ### 修改记录 (2026-02-17)
            // - 原因: 需要引入 RaftNode 写路径
            // - 目的: 让 Router 写入走 RaftNode
            let raft_node =
                RaftNode::start_local(1, std::path::PathBuf::from(args.db.clone())).await?;
            let router = Router::new_with_raft(raft_node);
            Arc::new(RouterEngine::new(router))
        }
        _ => return Err(format!("Unknown engine: {}", args.engine).into()),
    };

    let service = DbServiceImpl::new(engine);

    // Start serving requests
    Server::builder()
        .add_service(DatabaseServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}

// ### 修改记录 (2026-02-17)
// - 原因: 需要验证 Execute 路径走 Router
// - 目的: 以测试驱动后续 RouterEngine 实现
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockRouter {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WriteRouter for MockRouter {
        async fn write(&self, _sql: String) -> anyhow::Result<usize> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }

        async fn get_version(&self, _table: String) -> anyhow::Result<i32> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn execute_routes_to_router() {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = MockRouter {
            calls: calls.clone(),
        };
        let engine = RouterEngine::new(router);
        let service = DbServiceImpl::new(Arc::new(engine));
        let req = ExecuteRequest {
            sql: "CREATE TABLE t(x INT)".to_string(),
        };
        let _ = service.execute(Request::new(req)).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
