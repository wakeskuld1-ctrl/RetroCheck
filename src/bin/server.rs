use check_program::config::EdgeGatewayConfig;
use check_program::engine::StorageEngine;
use check_program::hub::edge_gateway::EdgeGateway;
use check_program::management::cluster_admin_service::{
    ClusterAdminService, ClusterBaseline, ClusterNodeManager, DrainSignal,
};
use check_program::pb::cluster_admin_client::ClusterAdminClient;
use check_program::management::order_rules_service::{OrderRulesAdminService, OrderRulesStore};
use check_program::pb::cluster_admin_server::ClusterAdminServer;
/// ### 修改记录 (2026-03-15)
/// - 原因: 需要注册 Raft gRPC 服务端
/// - 目的: 将 RaftService 加入同端口服务列表
use check_program::pb::raft_service_server::RaftServiceServer;
use check_program::pb::database_service_server::{DatabaseService, DatabaseServiceServer};
use check_program::pb::order_rules_admin_server::OrderRulesAdminServer;
use check_program::pb::{
    AddEdgeRequest, AddHubRequest, CommitRequest, CommitResponse, Empty, ExecuteRequest,
    ExecuteResponse, GetVersionRequest, GetVersionResponse, NodeCompatibility, PrepareRequest,
    PrepareResponse, RemoveHubRequest, RollbackRequest, RollbackResponse,
};
/// ### 修改记录 (2026-03-15)
/// - 原因: 需要引入 Raft gRPC 服务实现
/// - 目的: 让 server 能构造 RaftServiceImpl
use check_program::raft::grpc_service::RaftServiceImpl;
use check_program::raft::router::Router;
use check_program::raft::{network::RaftRouter, raft_node::RaftNode};
use clap::Parser;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use tonic::{Request, Response, Status, transport::Server};
use async_trait::async_trait;

struct EdgeGatewayDrainSignal {
    gateway: Arc<EdgeGateway>,
}

impl DrainSignal for EdgeGatewayDrainSignal {
    fn set_reject_new(&self, reject_new: bool) {
        self.gateway.set_draining(reject_new);
    }

    fn inflight_requests(&self) -> u64 {
        self.gateway.inflight_requests()
    }
}

/// ### 修改记录 (2026-03-15)
/// - 原因: 需要携带 Raft gRPC 服务实例
/// - 目的: 统一在启动流程中注入到 tonic Server
type BootstrapComponents = (
    Arc<dyn StorageEngine>,
    Option<Arc<Router>>,
    Option<ClusterAdminService>,
    Option<Arc<ClusterNodeManager>>,
    Option<RaftServiceImpl>,
);

/// Database Server CLI Arguments
///
/// Defines the command-line interface for the database node server.
/// Allows specifying the listening port, database file path, and storage engine type.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The port to listen on for gRPC connections.
    /// Default is 50051 (Master), usually 50052/50053 for Slaves.
    #[arg(short, long, default_value_t = 50051)]
    port: u16,

    /// The path to the database file (e.g., "master.db", "slave1.db").
    /// If it doesn't exist, it will be created.
    #[arg(short, long)]
    db: Option<String>,

    /// The storage engine to use: "sqlite".
    /// - sqlite: General-purpose OLTP storage.
    #[arg(short, long, default_value = "sqlite")]
    engine: String,

    /// Path to edge gateway config file
    #[arg(long)]
    edge_config: Option<String>,

    /// Port for Edge Gateway (TCP)
    #[arg(long, default_value_t = 8080)]
    edge_port: u16,

    /// Advertised gRPC address for leader redirect (e.g. http://10.0.0.1:50051)
    #[arg(long)]
    advertise_grpc: Option<String>,

    #[arg(long)]
    addhub: Option<String>,

    #[arg(long)]
    addedge: Option<String>,

    #[arg(long)]
    edge_group: Option<String>,

    #[arg(long)]
    removehub: Option<String>,

    #[arg(long)]
    removeedge: Option<String>,

    #[arg(long, default_value = "http://127.0.0.1:50051")]
    admin: String,

    #[arg(long)]
    node_id: Option<u64>,

    #[arg(long)]
    raft_addr: Option<String>,

    #[arg(long, default_value_t = true)]
    auto_promote: bool,
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

#[async_trait]
impl<R: WriteRouter + ?Sized> WriteRouter for Arc<R> {
    async fn write(&self, sql: String) -> anyhow::Result<usize> {
        (**self).write(sql).await
    }

    async fn get_version(&self, table: String) -> anyhow::Result<i32> {
        (**self).get_version(table).await
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
    if let Some(addhub_addr) = args.addhub.clone() {
        run_addhub_once(args, addhub_addr).await?;
        return Ok(());
    }
    if let Some(addedge_addr) = args.addedge.clone() {
        run_addedge_once(args, addedge_addr).await?;
        return Ok(());
    }
    if let Some(removehub_addr) = args.removehub.clone() {
        run_remove_once(args, removehub_addr, "REMOVE_HUB").await?;
        return Ok(());
    }
    if let Some(removeedge_addr) = args.removeedge.clone() {
        run_remove_once(args, removeedge_addr, "REMOVE_EDGE").await?;
        return Ok(());
    }
    let db_path = args
        .db
        .clone()
        .ok_or("--db is required when running server mode")?;
    let addr = format!("0.0.0.0:{}", args.port).parse()?;
    let local_grpc_addr = args
        .advertise_grpc
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", args.port));

    println!(
        "Starting server on {} using {} engine for db {}",
        addr, args.engine, db_path
    );

    // Factory pattern: Select engine based on CLI argument
    //
    // ### 修改记录 (2026-02-17)
    // - 原因: 需要将写路径切换到 Router
    // - 目的: 保持协议不变的同时完成内部替换
    // ### 修改记录 (2026-03-15)
    // - 原因: 需要把 Raft gRPC 服务实例带出初始化流程
    // - 目的: 与其他服务一起注册到 tonic Server
    let (engine, router, cluster_admin_service, cluster_manager, raft_service): BootstrapComponents =
        match args.engine.as_str() {
        "sqlite" => {
            let raft_router = RaftRouter::new();
            let raft_node = RaftNode::start(1, std::path::PathBuf::from(db_path), raft_router.clone())
                .await?;
            raft_router.register(1, Arc::new(raft_node.clone()));
            let router = Arc::new(Router::new_with_raft(raft_node.clone()));
            let baseline = ClusterBaseline {
                app_semver: env!("CARGO_PKG_VERSION").to_string(),
                sqlite_schema_version: 1,
                sled_format_version: 1,
                log_codec_version: 1,
            };
            // ### 修改记录 (2026-03-15)
            // - 原因: 需要构造 Raft gRPC 服务端实现
            // - 目的: 后续注册到同一 gRPC 端口
            let raft_service = RaftServiceImpl::new(raft_node.clone());
            let cluster_manager = Arc::new(ClusterNodeManager::new(
                1,
                local_grpc_addr.clone(),
                Arc::new(raft_node),
                raft_router,
                baseline,
            ));
            cluster_manager.bootstrap_local_if_needed().await?;
            let cluster_admin = ClusterAdminService::new(cluster_manager.clone());
            (
                Arc::new(RouterEngine::new(router.clone())),
                Some(router),
                Some(cluster_admin),
                Some(cluster_manager),
                Some(raft_service),
            )
        }
        _ => return Err(format!("Unknown engine: {}", args.engine).into()),
        };

    let service = DbServiceImpl::new(engine);
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要初始化顺序规则管理服务
    // - 目的: 支持规则热更新与版本查询
    let order_rules_store = Arc::new(OrderRulesStore::new());
    let order_rules_service = OrderRulesAdminService::new(order_rules_store.clone());

    // Start Edge Gateway if configured
    // ### 修改记录 (2026-03-01)
    // - 原因: clippy 提示可合并条件判断
    // - 目的: 保持网关启动逻辑不变
    if let Some(router) = router
        && let Some(config_path) = args.edge_config
    {
        let config = EdgeGatewayConfig::load_from_file(Path::new(&config_path))?;
        let edge_addr = format!("0.0.0.0:{}", args.edge_port);
        // ### 修改记录 (2026-03-01)
        // - 原因: 需要将规则存储注入网关侧
        // - 目的: 让网关读取热更新后的顺序规则
        let gateway = Arc::new(EdgeGateway::new(
            edge_addr,
            router,
            config,
            order_rules_store.clone(),
        )?);
        if let Some(manager) = cluster_manager.clone() {
            manager
                .register_drain_signal(
                    1,
                    Arc::new(EdgeGatewayDrainSignal {
                        gateway: gateway.clone(),
                    }),
                )
                .await;
        }
        tokio::spawn(async move {
            if let Err(e) = gateway.run().await {
                eprintln!("EdgeGateway error: {:?}", e);
            }
        });
    }

    // Start serving requests
    let mut builder = Server::builder()
        .add_service(OrderRulesAdminServer::new(order_rules_service))
        .add_service(DatabaseServiceServer::new(service));

    if let Some(cluster_admin_service) = cluster_admin_service {
        builder = builder.add_service(ClusterAdminServer::new(cluster_admin_service));
    }

    // ### 修改记录 (2026-03-15)
    // - 原因: 需要让 RaftService 与现有 gRPC 服务共用端口
    // - 目的: 复用已有 Server 监听并提供 Raft RPC
    if let Some(raft_service) = raft_service {
        builder = builder.add_service(RaftServiceServer::new(raft_service));
    }

    builder.serve(addr).await?;

    Ok(())
}

async fn run_addhub_once(args: Args, addhub_addr: String) -> Result<(), Box<dyn std::error::Error>> {
    let grpc_addr = normalize_http_addr(&addhub_addr);
    let raft_addr = args
        .raft_addr
        .unwrap_or_else(|| strip_http_prefix(&addhub_addr));
    let node_id = args.node_id.unwrap_or_else(|| derive_node_id(&grpc_addr));
    let request_id = format!(
        "cli-addhub-{}-{}",
        node_id,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    );
    let request = AddHubRequest {
        node_id,
        raft_addr,
        grpc_addr: grpc_addr.clone(),
        auto_promote: args.auto_promote,
        request_id,
        compatibility: Some(NodeCompatibility {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        }),
    };

    let mut admin_endpoint = normalize_http_addr(&args.admin);
    let mut client = ClusterAdminClient::connect(admin_endpoint.clone()).await?;
    let mut response = client.add_hub(Request::new(request.clone())).await?.into_inner();
    if response.reason_code == "NOT_LEADER" && response.leader_hint.starts_with("http") {
        admin_endpoint = response.leader_hint.clone();
        client = ClusterAdminClient::connect(admin_endpoint.clone()).await?;
        response = client.add_hub(Request::new(request)).await?.into_inner();
    }

    if response.reason_code.is_empty() {
        println!(
            "ADD_HUB_OK membership_version={} node_id={} grpc_addr={} admin={}",
            response.membership_version, node_id, grpc_addr, admin_endpoint
        );
        return Ok(());
    }

    let category = classify_addhub_reason_code(&response.reason_code);
    let detail = format_addhub_failure_detail(&response);
    println!(
        "ADD_HUB_FAIL reason_code={} category={} suggested_action={} leader_hint={} detail={}",
        response.reason_code, category, response.suggested_action, response.leader_hint, detail
    );
    Err(
        format!(
            "add_hub failed: code={}, category={}, detail={}",
            response.reason_code, category, detail
        )
        .into(),
    )
}

async fn run_addedge_once(
    args: Args,
    addedge_addr: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let edge_id = normalize_http_addr(&addedge_addr);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let request = AddEdgeRequest {
        edge_id: edge_id.clone(),
        group_id: args.edge_group.clone().unwrap_or_default(),
        request_id: format!("cli-addedge-{}-{}", derive_node_id(&edge_id), timestamp),
    };
    let mut admin_endpoint = normalize_http_addr(&args.admin);
    let mut client = ClusterAdminClient::connect(admin_endpoint.clone()).await?;
    let mut response = match client.add_edge(Request::new(request.clone())).await {
        Ok(resp) => resp.into_inner(),
        Err(status) if status.code() == tonic::Code::Unimplemented => {
            return run_addhub_once(args, addedge_addr).await;
        }
        Err(status) => return Err(status.into()),
    };

    if response.reason_code == "NOT_LEADER"
        && let Some(leader_hint) = extract_leader_hint_from_action(&response.suggested_action)
    {
        admin_endpoint = leader_hint;
        client = ClusterAdminClient::connect(admin_endpoint.clone()).await?;
        response = match client.add_edge(Request::new(request)).await {
            Ok(resp) => resp.into_inner(),
            Err(status) if status.code() == tonic::Code::Unimplemented => {
                return run_addhub_once(args, addedge_addr).await;
            }
            Err(status) => return Err(status.into()),
        };
    }

    if response.ready && response.reason_code.is_empty() {
        println!(
            "ADD_EDGE_OK edge_id={} group_id={} admin={}",
            edge_id,
            args.edge_group.as_deref().unwrap_or(""),
            admin_endpoint
        );
        return Ok(());
    }

    let category = classify_addedge_reason_code(&response.reason_code);
    let detail = format_addedge_failure_detail(&response);
    println!(
        "ADD_EDGE_FAIL reason_code={} category={} suggested_action={} detail={}",
        response.reason_code, category, response.suggested_action, detail
    );
    Err(
        format!(
            "add_edge failed: code={}, category={}, detail={}",
            response.reason_code, category, detail
        )
        .into(),
    )
}

async fn run_remove_once(
    args: Args,
    remove_addr: String,
    action: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let grpc_addr = normalize_http_addr(&remove_addr);
    let node_id = args.node_id.unwrap_or_else(|| derive_node_id(&grpc_addr));
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let mark_request = RemoveHubRequest {
        node_id,
        request_id: format!("cli-removehub-mark-{}-{}", node_id, timestamp),
        phase: "MARK_DRAINING".to_string(),
        force: false,
        drain_timeout_ms: 10_000,
    };
    let mut admin_endpoint = normalize_http_addr(&args.admin);
    let (mark_response, mark_endpoint) =
        remove_with_redirect(admin_endpoint.clone(), mark_request).await?;
    admin_endpoint = mark_endpoint;
    if !mark_response.reason_code.is_empty() && mark_response.reason_code != "DRAINING_MARKED" {
        let category = classify_removehub_reason_code(&mark_response.reason_code);
        let detail = format_removehub_failure_detail(&mark_response);
        println!(
            "{}_FAIL phase=MARK_DRAINING reason_code={} category={} suggested_action={} leader_hint={} detail={}",
            action,
            mark_response.reason_code,
            category,
            mark_response.suggested_action,
            mark_response.leader_hint,
            detail
        );
        return Err(
            format!(
                "remove_hub mark failed: code={}, category={}, detail={}",
                mark_response.reason_code, category, detail
            )
            .into(),
        );
    }

    let commit_request = RemoveHubRequest {
        node_id,
        request_id: format!("cli-removehub-commit-{}-{}", node_id, timestamp),
        phase: "COMMIT".to_string(),
        force: false,
        drain_timeout_ms: 10_000,
    };
    let (commit_response, commit_endpoint) = remove_with_redirect(admin_endpoint, commit_request).await?;
    if commit_response.reason_code.is_empty() {
        println!(
            "{}_OK membership_version={} node_id={} grpc_addr={} admin={}",
            action, commit_response.membership_version, node_id, grpc_addr, commit_endpoint
        );
        return Ok(());
    }
    let category = classify_removehub_reason_code(&commit_response.reason_code);
    let detail = format_removehub_failure_detail(&commit_response);
    println!(
        "{}_FAIL phase=COMMIT reason_code={} category={} suggested_action={} leader_hint={} detail={}",
        action,
        commit_response.reason_code,
        category,
        commit_response.suggested_action,
        commit_response.leader_hint,
        detail
    );
    Err(
        format!(
            "remove_hub commit failed: code={}, category={}, detail={}",
            commit_response.reason_code, category, detail
        )
        .into(),
    )
}

async fn remove_with_redirect(
    admin_endpoint: String,
    request: RemoveHubRequest,
) -> Result<(check_program::pb::RemoveHubResponse, String), Box<dyn std::error::Error>> {
    let mut endpoint = normalize_http_addr(&admin_endpoint);
    let mut client = ClusterAdminClient::connect(endpoint.clone()).await?;
    let mut response = client
        .remove_hub(Request::new(request.clone()))
        .await?
        .into_inner();
    if response.reason_code == "NOT_LEADER" && response.leader_hint.starts_with("http") {
        endpoint = response.leader_hint.clone();
        client = ClusterAdminClient::connect(endpoint.clone()).await?;
        response = client.remove_hub(Request::new(request)).await?.into_inner();
    }
    Ok((response, endpoint))
}

fn normalize_http_addr(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{}", addr)
    }
}

fn strip_http_prefix(addr: &str) -> String {
    addr.trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string()
}

fn derive_node_id(addr: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    addr.hash(&mut hasher);
    let hashed = hasher.finish();
    if hashed == 0 {
        1
    } else {
        hashed
    }
}

fn extract_leader_hint_from_action(suggested_action: &str) -> Option<String> {
    let prefix = "retry request to ";
    if suggested_action.starts_with(prefix) {
        return Some(suggested_action[prefix.len()..].trim().to_string());
    }
    None
}

fn classify_addhub_reason_code(reason_code: &str) -> &'static str {
    match reason_code {
        "NOT_LEADER" => "LEADER_REDIRECT",
        "INVALID_ARGUMENT" => "REQUEST_INVALID",
        "NODE_ID_ALREADY_EXISTS" => "NODE_CONFLICT",
        "INCOMPATIBLE_APP_VERSION"
        | "INCOMPATIBLE_SQLITE_SCHEMA"
        | "INCOMPATIBLE_SLED_FORMAT"
        | "INCOMPATIBLE_LOG_CODEC" => "VERSION_INCOMPATIBLE",
        _ => "UNKNOWN",
    }
}

fn format_addhub_failure_detail(response: &check_program::pb::AddHubResponse) -> String {
    match response.reason_code.as_str() {
        "INCOMPATIBLE_APP_VERSION" => {
            "应用版本不一致，需统一 app_semver 后再重试".to_string()
        }
        "INCOMPATIBLE_SQLITE_SCHEMA" => {
            "SQLite schema 版本不一致，需先完成迁移再加点".to_string()
        }
        "INCOMPATIBLE_SLED_FORMAT" => {
            "sled 数据格式不一致，需升级或转换存储格式".to_string()
        }
        "INCOMPATIBLE_LOG_CODEC" => {
            "Raft 日志编码版本不一致，需切换到集群一致编码".to_string()
        }
        "NOT_LEADER" => {
            if response.leader_hint.is_empty() {
                "请求命中 follower，且暂未获取 leader 地址".to_string()
            } else {
                "请求命中 follower，需要转发到 leader_hint".to_string()
            }
        }
        "NODE_ID_ALREADY_EXISTS" => "node_id 已存在，请更换 node_id 或复用 request_id".to_string(),
        "INVALID_ARGUMENT" => "请求参数非法，请按 suggested_action 修正".to_string(),
        _ => "未知错误，请结合 reason_code 与 suggested_action 排查".to_string(),
    }
}

fn classify_addedge_reason_code(reason_code: &str) -> &'static str {
    match reason_code {
        "NOT_LEADER" => "LEADER_REDIRECT",
        "TARGET_METADATA_MISSING" => "TARGET_METADATA_MISSING",
        "TARGET_NOT_FOUND" => "TARGET_NOT_FOUND",
        "INVALID_ARGUMENT" => "REQUEST_INVALID",
        _ => "UNKNOWN",
    }
}

fn format_addedge_failure_detail(response: &check_program::pb::AddEdgeResponse) -> String {
    match response.reason_code.as_str() {
        "NOT_LEADER" => "请求命中 follower，请按 suggested_action 转发到 leader".to_string(),
        "TARGET_METADATA_MISSING" => "目标分组元数据缺失，请先同步元数据后重试".to_string(),
        "TARGET_NOT_FOUND" => "目标分组不存在，请确认 group_id".to_string(),
        "INVALID_ARGUMENT" => "请求参数非法，请按 suggested_action 修正".to_string(),
        _ => "未知错误，请结合 reason_code 与 suggested_action 排查".to_string(),
    }
}

fn classify_removehub_reason_code(reason_code: &str) -> &'static str {
    match reason_code {
        "NOT_LEADER" => "LEADER_REDIRECT",
        "INVALID_ARGUMENT" | "INVALID_PHASE" | "BOOTSTRAP_FAILED" => "REQUEST_INVALID",
        "NODE_NOT_FOUND" => "NODE_MISSING",
        "NOT_DRAINING" => "PHASE_VIOLATION",
        "LEADER_TRANSFER_TIMEOUT" => "LEADER_TRANSFER_TIMEOUT",
        "DRAIN_TIMEOUT" => "DRAIN_TIMEOUT",
        "LAST_VOTER_FORBIDDEN" => "MEMBERSHIP_GUARD",
        _ => "UNKNOWN",
    }
}

fn format_removehub_failure_detail(response: &check_program::pb::RemoveHubResponse) -> String {
    match response.reason_code.as_str() {
        "NOT_LEADER" => {
            if response.leader_hint.is_empty() {
                "请求命中 follower，且暂未获取 leader 地址".to_string()
            } else {
                "请求命中 follower，需要转发到 leader_hint".to_string()
            }
        }
        "INVALID_ARGUMENT" => "请求参数非法，请按 suggested_action 修正".to_string(),
        "INVALID_PHASE" => "phase 非法，必须是 MARK_DRAINING 或 COMMIT".to_string(),
        "NODE_NOT_FOUND" => "目标节点不存在，请确认 node_id 或地址映射".to_string(),
        "NOT_DRAINING" => "节点尚未标记排空，请先执行 MARK_DRAINING".to_string(),
        "LEADER_TRANSFER_TIMEOUT" => "主节点转移超时，请稍后重试 COMMIT".to_string(),
        "DRAIN_TIMEOUT" => "在途请求未在超时内排空，请稍后重试 COMMIT".to_string(),
        "LAST_VOTER_FORBIDDEN" => "禁止移除最后一个 voter，除非 force=true".to_string(),
        _ => "未知错误，请结合 reason_code 与 suggested_action 排查".to_string(),
    }
}

// ### 修改记录 (2026-02-17)
// - 原因: 需要验证 Execute 路径走 Router
// - 目的: 以测试驱动后续 RouterEngine 实现
#[cfg(test)]
mod tests {
    use super::*;
    use check_program::pb::{AddEdgeResponse, AddHubResponse, RemoveHubResponse};
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

    #[test]
    fn classify_reason_code_version_incompatible() {
        assert_eq!(
            classify_addhub_reason_code("INCOMPATIBLE_SQLITE_SCHEMA"),
            "VERSION_INCOMPATIBLE"
        );
    }

    #[test]
    fn format_reason_detail_for_version_error() {
        let response = AddHubResponse {
            membership_version: 0,
            leader_hint: String::new(),
            reason_code: "INCOMPATIBLE_APP_VERSION".to_string(),
            suggested_action: "upgrade or downgrade".to_string(),
        };
        assert!(format_addhub_failure_detail(&response).contains("应用版本不一致"));
    }

    #[test]
    fn classify_remove_reason_code_for_drain_timeout() {
        assert_eq!(
            classify_removehub_reason_code("DRAIN_TIMEOUT"),
            "DRAIN_TIMEOUT"
        );
    }

    #[test]
    fn format_remove_reason_detail_for_leader_transfer_timeout() {
        let response = RemoveHubResponse {
            membership_version: 0,
            leader_hint: String::new(),
            reason_code: "LEADER_TRANSFER_TIMEOUT".to_string(),
            suggested_action: "retry".to_string(),
            status: "DRAINING".to_string(),
        };
        assert!(format_removehub_failure_detail(&response).contains("主节点转移超时"));
    }

    #[test]
    fn classify_addedge_reason_code_for_metadata_missing() {
        assert_eq!(
            classify_addedge_reason_code("TARGET_METADATA_MISSING"),
            "TARGET_METADATA_MISSING"
        );
    }

    #[test]
    fn format_addedge_reason_detail_for_target_not_found() {
        let response = AddEdgeResponse {
            ready: false,
            reason_code: "TARGET_NOT_FOUND".to_string(),
            suggested_action: "verify group".to_string(),
        };
        assert!(format_addedge_failure_detail(&response).contains("目标分组不存在"));
    }

    #[test]
    fn extract_leader_hint_from_action_works() {
        let hint = extract_leader_hint_from_action("retry request to http://127.0.0.1:50051");
        assert_eq!(hint.as_deref(), Some("http://127.0.0.1:50051"));
    }
}
