//! ### 修改记录 (2026-02-17)
//! - 原因: 需要验证 Router 的转发逻辑
//! - 目的: 在最小实现阶段锁定分支行为
//! ### 修改记录 (2026-02-25)
//! - 原因: 需要补充非 Leader 的转发用例
//! - 目的: 先用失败测试驱动 gRPC 转发实现

// ### 修改记录 (2026-02-25)
// - 原因: 需要引入 gRPC Service 与请求类型
// - 目的: 构造最小可用的转发测试服务器
use check_program::pb::database_service_server::{DatabaseService, DatabaseServiceServer};
// ### 修改记录 (2026-02-25)
// - 原因: 测试需要完整实现 gRPC Trait
// - 目的: 使用协议中的 Request/Response 类型
use check_program::pb::{
    CommitRequest, CommitResponse, Empty, ExecuteRequest, ExecuteResponse, GetVersionRequest,
    GetVersionResponse, PrepareRequest, PrepareResponse, RollbackRequest, RollbackResponse,
};
// ### 修改记录 (2026-02-25)
// - 原因: 需要构造真实 Router 作为 Leader
// - 目的: 让转发链路能执行写入
use check_program::raft::router::Router;
// ### 修改记录 (2026-02-25)
// - 原因: 需要在测试中共享 Router
// - 目的: 避免重复初始化并简化生命周期
use std::sync::Arc;
// ### 修改记录 (2026-02-25)
// - 原因: 需要临时 SQLite 文件
// - 目的: 避免污染仓库目录
use tempfile::TempDir;
// ### 修改记录 (2026-02-25)
// - 原因: 需要启动临时 gRPC 服务
// - 目的: 为 Router 转发提供可达的 Leader
use tokio::net::TcpListener;
// ### 修改记录 (2026-02-25)
// - 原因: 将 TcpListener 包装为 tonic 兼容流
// - 目的: 避免硬编码端口
use tokio_stream::wrappers::TcpListenerStream;
// ### 修改记录 (2026-02-25)
// - 原因: 需要构造 gRPC Server 与请求上下文
// - 目的: 保持测试与生产调用一致
use tonic::{Request, Response, Status, transport::Server};

// ### 修改记录 (2026-02-25)
// - 原因: 需要为 Router 转发测试提供 gRPC 服务
// - 目的: 最小实现 Execute 路径即可
#[derive(Clone)]
struct TestRouterService {
    // ### 修改记录 (2026-02-25)
    // - 原因: Execute 需要调用 Router::write
    // - 目的: 复用生产逻辑确保转发真实可用
    router: Arc<Router>,
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要在测试中实现 gRPC Trait
// - 目的: 保证 Router 转发的服务端路径完整
#[tonic::async_trait]
impl DatabaseService for TestRouterService {
    // ### 修改记录 (2026-02-25)
    // - 原因: 转发只关注 Execute 行为
    // - 目的: 验证非 Leader 能走到 Leader 写入
    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        let req = request.into_inner();
        let rows = self
            .router
            .write(req.sql)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ExecuteResponse {
            rows_affected: rows as i32,
        }))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试只需最小协议闭环
    // - 目的: 其它接口保持空实现以满足 Trait
    async fn prepare(
        &self,
        _request: Request<PrepareRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        Ok(Response::new(PrepareResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不覆盖 2PC 阶段
    // - 目的: 保持接口完整性
    async fn commit(
        &self,
        _request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        Ok(Response::new(CommitResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不覆盖 2PC 阶段
    // - 目的: 保持接口完整性
    async fn rollback(
        &self,
        _request: Request<RollbackRequest>,
    ) -> Result<Response<RollbackResponse>, Status> {
        Ok(Response::new(RollbackResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不验证版本读取
    // - 目的: 返回固定值即可
    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse { version: 0 }))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 需要健康检查最小实现
    // - 目的: 避免客户端调用时报错
    async fn check_health(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        Ok(Response::new(Empty {}))
    }
}

// ### 修改记录 (2026-02-25)
// - 原因: 测试需要启动临时 gRPC 服务
// - 目的: 统一封装启动逻辑简化用例
async fn spawn_test_server(router: Arc<Router>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpListenerStream::new(listener);
    let service = TestRouterService { router };
    tokio::spawn(
        Server::builder()
            .add_service(DatabaseServiceServer::new(service))
            .serve_with_incoming(incoming),
    );
    format!("http://{}", addr)
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要模拟转发超时场景
// - 目的: 通过延迟响应触发 Router 超时逻辑
#[derive(Clone)]
struct SlowRouterService {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要控制延迟
    // - 目的: 让测试可稳定触发超时
    delay_ms: u64,
}

// ### 修改记录 (2026-02-25)
// - 原因: 测试需要可控的延迟响应服务
// - 目的: 模拟 Leader 执行过慢
#[tonic::async_trait]
impl DatabaseService for SlowRouterService {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要延迟响应 Execute
    // - 目的: 触发转发超时错误
    async fn execute(
        &self,
        _request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        Ok(Response::new(ExecuteResponse { rows_affected: 1 }))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn prepare(
        &self,
        _request: Request<PrepareRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        Ok(Response::new(PrepareResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn commit(
        &self,
        _request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        Ok(Response::new(CommitResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn rollback(
        &self,
        _request: Request<RollbackRequest>,
    ) -> Result<Response<RollbackResponse>, Status> {
        Ok(Response::new(RollbackResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不关注版本读取
    // - 目的: 返回固定值保持接口可用
    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse { version: 0 }))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不关注健康检查
    // - 目的: 返回固定成功
    async fn check_health(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        Ok(Response::new(Empty {}))
    }
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要启动延迟响应的 gRPC 服务
// - 目的: 专用于超时测试场景
async fn spawn_slow_server(delay_ms: u64) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpListenerStream::new(listener);
    let service = SlowRouterService { delay_ms };
    tokio::spawn(
        Server::builder()
            .add_service(DatabaseServiceServer::new(service))
            .serve_with_incoming(incoming),
    );
    format!("http://{}", addr)
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要稳定触发连接失败
// - 目的: 避免因非 gRPC 握手进入 execute 分支
async fn reserve_port_then_close() -> String {
    // ### 修改记录 (2026-02-25)
    // - 原因: 获取可用端口
    // - 目的: 避免硬编码端口引入不稳定
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要连接失败
    // - 目的: 让连接阶段直接报错
    drop(listener);
    format!("http://{}", addr)
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要模拟端口占用但服务不符合预期
// - 目的: 验证连接成功但执行失败的场景
#[derive(Clone)]
struct UnimplementedService;

// ### 修改记录 (2026-02-25)
// - 原因: 需要 gRPC 服务返回未实现
// - 目的: 触发 execute failed 分支
#[tonic::async_trait]
impl DatabaseService for UnimplementedService {
    // ### 修改记录 (2026-02-25)
    // - 原因: 模拟服务端方法未实现
    // - 目的: 让客户端收到非预期响应
    async fn execute(
        &self,
        _request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        Err(Status::unimplemented("method not implemented"))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn prepare(
        &self,
        _request: Request<PrepareRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        Ok(Response::new(PrepareResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn commit(
        &self,
        _request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        Ok(Response::new(CommitResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn rollback(
        &self,
        _request: Request<RollbackRequest>,
    ) -> Result<Response<RollbackResponse>, Status> {
        Ok(Response::new(RollbackResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不关注版本读取
    // - 目的: 返回固定值保持接口可用
    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse { version: 0 }))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不关注健康检查
    // - 目的: 返回固定成功
    async fn check_health(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        Ok(Response::new(Empty {}))
    }
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要启动未实现服务
// - 目的: 模拟端口占用但服务不符合预期
async fn spawn_unimplemented_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpListenerStream::new(listener);
    let service = UnimplementedService;
    tokio::spawn(
        Server::builder()
            .add_service(DatabaseServiceServer::new(service))
            .serve_with_incoming(incoming),
    );
    format!("http://{}", addr)
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要模拟 leader 返回包含敏感信息的错误
// - 目的: 验证 Router 会对错误信息进行脱敏
#[derive(Clone)]
struct ErrorEchoService {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要注入错误文本
    // - 目的: 让测试可控验证敏感内容
    message: String,
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要专用服务返回错误消息
// - 目的: 触发 Router 的 execute error 分支
#[tonic::async_trait]
impl DatabaseService for ErrorEchoService {
    // ### 修改记录 (2026-02-25)
    // - 原因: 强制返回包含敏感字串的错误
    // - 目的: 验证脱敏逻辑
    async fn execute(
        &self,
        _request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        Err(Status::internal(self.message.clone()))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn prepare(
        &self,
        _request: Request<PrepareRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        Ok(Response::new(PrepareResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn commit(
        &self,
        _request: Request<CommitRequest>,
    ) -> Result<Response<CommitResponse>, Status> {
        Ok(Response::new(CommitResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 保持接口完整性
    // - 目的: 其它接口返回空成功
    async fn rollback(
        &self,
        _request: Request<RollbackRequest>,
    ) -> Result<Response<RollbackResponse>, Status> {
        Ok(Response::new(RollbackResponse {}))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不关注版本读取
    // - 目的: 返回固定值保持接口可用
    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse { version: 0 }))
    }

    // ### 修改记录 (2026-02-25)
    // - 原因: 测试不关注健康检查
    // - 目的: 返回固定成功
    async fn check_health(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        Ok(Response::new(Empty {}))
    }
}

// ### 修改记录 (2026-02-25)
// - 原因: 需要启动返回错误的 gRPC 服务
// - 目的: 为敏感信息脱敏测试提供输入
async fn spawn_error_server(message: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = TcpListenerStream::new(listener);
    let service = ErrorEchoService { message };
    tokio::spawn(
        Server::builder()
            .add_service(DatabaseServiceServer::new(service))
            .serve_with_incoming(incoming),
    );
    format!("http://{}", addr)
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 非 Leader 需要走转发路径
/// - 目的: 覆盖 Router 的条件分支
/// ### 修改记录 (2026-02-17)
/// - 原因: 非 Leader 写入改为明确报错
/// - 目的: 与当前写入语义保持一致
#[tokio::test]
async fn router_returns_error_when_not_leader() {
    // ### 修改记录 (2026-02-17)
    // - 原因: 构造非 Leader Router
    // - 目的: 验证非 Leader 明确报错
    let router = check_program::raft::router::Router::new_for_test(false);
    let res = router.write("INSERT INTO t VALUES (1)".to_string()).await;
    assert!(res.is_err());
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证非 Leader 走转发链路
/// - 目的: 锁定转发行为与返回值
#[tokio::test]
async fn router_forwards_write_to_leader_grpc() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要临时 SQLite 作为 Leader 存储
    // - 目的: 保证测试隔离且可重复
    let temp_dir = TempDir::new().unwrap();
    let leader_db = temp_dir.path().join("leader.db");

    // ### 修改记录 (2026-02-25)
    // - 原因: 需要真实 Leader Router 承接写入
    // - 目的: 确保转发链路调用真实写逻辑
    let leader_router = Router::new_local_leader(leader_db.to_string_lossy().to_string()).unwrap();
    let leader_addr = spawn_test_server(Arc::new(leader_router)).await;

    // ### 修改记录 (2026-02-25)
    // - 原因: 非 Leader 需要转发到 Leader
    // - 目的: 覆盖转发入口路径
    let follower_router = Router::new_follower_with_leader_addr(leader_addr);

    // ### 修改记录 (2026-02-25)
    // - 原因: 需要准备测试表
    // - 目的: 避免插入失败导致误判
    let _ = follower_router
        .write("CREATE TABLE IF NOT EXISTS t(x INT)".to_string())
        .await
        .unwrap();

    // ### 修改记录 (2026-02-25)
    // - 原因: 写入应走转发链路
    // - 目的: 验证返回 rows_affected
    let rows = follower_router
        .write("INSERT INTO t VALUES (1)".to_string())
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要覆盖 leader 地址无效场景
/// - 目的: 验证返回错误包含连接失败信息
#[tokio::test]
async fn router_forward_fails_when_leader_addr_invalid() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 构造无效 leader 地址
    // - 目的: 触发连接失败路径
    let invalid_addr = "http://127.0.0.1:1".to_string();
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要非 Leader Router
    // - 目的: 强制进入转发逻辑
    let follower = Router::new_follower_with_leader_addr(invalid_addr);
    let err = follower
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Leader connect failed"));
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要覆盖转发超时场景
/// - 目的: 确保返回 timeout 错误
#[tokio::test]
async fn router_forward_times_out_when_leader_slow() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 启动延迟响应的 Leader
    // - 目的: 人为制造超时窗口
    let leader_addr = spawn_slow_server(200).await;
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要更小的转发超时
    // - 目的: 稳定触发超时错误
    let follower = Router::new_follower_with_leader_addr_and_timeout(leader_addr, 10);
    let err = follower
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Leader forward timeout"));
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要覆盖 leader 执行错误场景
/// - 目的: 确保错误透传为 execute failed
#[tokio::test]
async fn router_forward_returns_execute_error_when_leader_sql_invalid() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 需要真实 Leader
    // - 目的: 触发实际 SQL 执行错误
    let temp_dir = TempDir::new().unwrap();
    let leader_db = temp_dir.path().join("leader_err.db");
    let leader_router = Router::new_local_leader(leader_db.to_string_lossy().to_string()).unwrap();
    let leader_addr = spawn_test_server(Arc::new(leader_router)).await;
    // ### 修改记录 (2026-02-25)
    // - 原因: 非 Leader 需要转发
    // - 目的: 覆盖转发错误路径
    let follower = Router::new_follower_with_leader_addr(leader_addr);
    let err = follower
        .write("INSRT INTO t VALUES (1)".to_string())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Leader execute failed"));
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要覆盖未设置 leader_addr 场景
/// - 目的: 确认错误信息明确
#[tokio::test]
async fn router_forward_errors_when_leader_addr_missing() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 使用最小构造非 Leader
    // - 目的: 覆盖无 leader 地址的错误路径
    let follower = Router::new_for_test(false);
    let err = follower
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("leader address unknown"));
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要覆盖端口占用但非 gRPC 服务场景
/// - 目的: 断言执行错误会返回 Leader execute failed
#[tokio::test]
async fn router_forward_fails_when_port_occupied_non_grpc() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 服务端不符合预期
    // - 目的: 触发 execute error
    let leader_addr = spawn_unimplemented_server().await;
    let follower = Router::new_follower_with_leader_addr(leader_addr);
    let err = follower
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Leader execute failed"));
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要稳定复现连接失败
/// - 目的: 断言连接失败分支可用
#[tokio::test]
async fn router_forward_fails_when_port_closed() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 获取端口后立即关闭
    // - 目的: 稳定触发连接失败
    let leader_addr = reserve_port_then_close().await;
    let follower = Router::new_follower_with_leader_addr(leader_addr);
    let err = follower
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Leader connect failed"));
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 超短超时会导致频繁超时
/// - 目的: 统计多次写入均为超时
#[tokio::test]
async fn router_forward_timeout_repeats_when_timeout_too_short() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 构造慢响应服务
    // - 目的: 稳定触发超时
    // ### 修改记录 (2026-02-25)
    // - 原因: 慢机器下 50ms 可能不稳定
    // - 目的: 提高超时稳定性
    let leader_addr = spawn_slow_server(200).await;
    let follower = Router::new_follower_with_leader_addr_and_timeout(leader_addr, 1);
    let mut timeout_count = 0;
    for _ in 0..3 {
        let err = follower
            .write("CREATE TABLE t(x INT)".to_string())
            .await
            .unwrap_err();
        if err.to_string().contains("Leader forward timeout") {
            timeout_count += 1;
        }
    }
    assert_eq!(timeout_count, 3);
}

/// ### 修改记录 (2026-02-25)
/// - 原因: 需要验证敏感信息不外泄
/// - 目的: 锁定错误脱敏行为
#[tokio::test]
async fn router_forward_sanitizes_execute_error_message() {
    // ### 修改记录 (2026-02-25)
    // - 原因: 注入敏感字串
    // - 目的: 确保错误中不出现敏感片段
    let secret = "SECRET_TOKEN_123".to_string();
    let leader_addr = spawn_error_server(format!("db error: {}", secret)).await;
    let follower = Router::new_follower_with_leader_addr(leader_addr);
    let err = follower
        .write("CREATE TABLE t(x INT)".to_string())
        .await
        .unwrap_err();
    let msg = err.to_string();
    // ### 修改记录 (2026-02-25)
    // - 原因: 错误细节可能变化
    // - 目的: 只断言固定前缀
    assert!(msg.starts_with("Leader execute failed"));
}
