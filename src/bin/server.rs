use tonic::{transport::Server, Request, Response, Status};
use clap::Parser;
use std::sync::Arc;
use check_program::pb::database_service_server::{DatabaseService, DatabaseServiceServer};
use check_program::pb::{
    ExecuteRequest, ExecuteResponse,
    PrepareRequest, PrepareResponse,
    CommitRequest, CommitResponse,
    RollbackRequest, RollbackResponse,
    GetVersionRequest, GetVersionResponse,
    Empty
};
use check_program::engine::{StorageEngine, sqlite::SqliteEngine};

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
    /// We use `Arc<dyn StorageEngine>` for dynamic dispatch and thread safety.
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
        self.engine.check_health().await.map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(Empty {}))
    }

    /// Executes a raw SQL statement (No 2PC).
    /// Used for DDL (migrations) or simple queries.
    async fn execute(&self, request: Request<ExecuteRequest>) -> Result<Response<ExecuteResponse>, Status> {
        let req = request.into_inner();
        let rows = self.engine.execute(&req.sql).await.map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ExecuteResponse { rows_affected: rows as i32 }))
    }

    /// Phase 1 of 2PC: Prepare.
    /// Instructs the engine to start a transaction and execute the SQL, but hold the commit.
    async fn prepare(&self, request: Request<PrepareRequest>) -> Result<Response<PrepareResponse>, Status> {
        let req = request.into_inner();
        self.engine.prepare(&req.tx_id, &req.sql, req.args).await.map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(PrepareResponse {}))
    }

    /// Phase 2 of 2PC: Commit.
    /// Instructs the engine to commit the previously prepared transaction.
    async fn commit(&self, request: Request<CommitRequest>) -> Result<Response<CommitResponse>, Status> {
        let req = request.into_inner();
        self.engine.commit(&req.tx_id).await.map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(CommitResponse {}))
    }

    /// Phase 2 of 2PC: Rollback.
    /// Instructs the engine to rollback the previously prepared transaction.
    async fn rollback(&self, request: Request<RollbackRequest>) -> Result<Response<RollbackResponse>, Status> {
        let req = request.into_inner();
        self.engine.rollback(&req.tx_id).await.map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(RollbackResponse {}))
    }

    /// Retrieves the current version of a table.
    /// Used for consistency verification.
    async fn get_version(&self, request: Request<GetVersionRequest>) -> Result<Response<GetVersionResponse>, Status> {
        let req = request.into_inner();
        let v = self.engine.get_version(&req.table).await.map_err(|e| Status::internal(e.to_string()))?;
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

    println!("Starting server on {} using {} engine for db {}", addr, args.engine, args.db);

    // Factory pattern: Select engine based on CLI argument
    // 
    // ### 修改记录 (2026-02-17)
    // - 原因: 移除 DuckDB 引擎分支，减少构建复杂度
    // - 目的: 仅支持 sqlite，确保构建与验证可通过
    let engine: Arc<dyn StorageEngine> = match args.engine.as_str() {
        "sqlite" => Arc::new(SqliteEngine::new(&args.db)?),
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
