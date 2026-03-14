//! ### 修改记录 (2026-02-17)
//! - 原因: 需要提供最小 Router 入口
//! - 目的: 支持 Leader 判断与转发路径

use crate::pb::database_service_client::DatabaseServiceClient;
use crate::pb::{ExecuteRequest, GetVersionRequest};
use crate::raft::raft_node::{RaftNode, TestCluster};
use crate::raft::state_machine::SqliteStateMachine;
use anyhow::{Result, anyhow};
use openraft::ServerState;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;

// ### 修改记录 (2026-03-10)
// - 原因: 统一 SQL 语法新增了标准错误码
// - 目的: 在 SQL 路由解析层稳定返回可断言的错误编码
const ROUTE_INVALID: &str = "ROUTE_INVALID";
// ### 修改记录 (2026-03-10)
// - 原因: 统一 SQL 规范要求键列不能是 eventual
// - 目的: 在路由入口前置拦截非法一致性声明
const COLUMN_CONSISTENCY_INVALID: &str = "COLUMN_CONSISTENCY_INVALID";
// ### 修改记录 (2026-03-10)
// - 原因: 方案A需要补齐目标存在性错误码
// - 目的: 路由目标不存在时返回稳定可断言的标准错误
const TARGET_NOT_FOUND: &str = "TARGET_NOT_FOUND";
const TARGET_METADATA_MISSING: &str = "TARGET_METADATA_MISSING";

#[derive(Debug, Clone)]
enum RouteTarget {
    Hub,
    EdgeDevice(String),
    EdgeGroup(String),
    EdgeAll,
}

struct ParsedRouteSql {
    body_sql: String,
    target: RouteTarget,
}

enum RouteTargetLookup {
    Exists,
    NotFound,
    MetadataMissing,
}

fn code_error(code: &str, message: &str) -> anyhow::Error {
    anyhow!("{code}: {message}")
}

fn parse_route_sql(raw_sql: &str) -> Result<ParsedRouteSql> {
    let trimmed = raw_sql.trim();
    if trimmed.is_empty() {
        return Err(code_error(ROUTE_INVALID, "SQL is empty"));
    }

    let without_semicolon = trimmed.trim_end_matches(';').trim_end();
    let upper = without_semicolon.to_ascii_uppercase();

    if upper.ends_with("FOR HUB") {
        let route_pos = upper
            .rfind("FOR HUB")
            .ok_or_else(|| code_error(ROUTE_INVALID, "invalid FOR HUB clause"))?;
        let body = without_semicolon[..route_pos].trim_end();
        if body.is_empty() {
            return Err(code_error(ROUTE_INVALID, "missing SQL body before FOR HUB"));
        }
        return Ok(ParsedRouteSql {
            body_sql: body.to_string(),
            target: RouteTarget::Hub,
        });
    }

    if upper.ends_with(')') && let Some(route_pos) = upper.rfind("FOR EDGE(") {
        let suffix_start = route_pos + "FOR EDGE(".len();
        if suffix_start >= without_semicolon.len() {
            return Err(code_error(ROUTE_INVALID, "missing EDGE route parameters"));
        }
        let body = without_semicolon[..route_pos].trim_end();
        if body.is_empty() {
            return Err(code_error(
                ROUTE_INVALID,
                "missing SQL body before FOR EDGE",
            ));
        }
        let params = &without_semicolon[suffix_start..without_semicolon.len() - 1];
        let mut parts = params.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        if key.is_empty() || value.is_empty() {
            return Err(code_error(
                ROUTE_INVALID,
                "EDGE route parameter must be key=value",
            ));
        }
        let target = if key.eq_ignore_ascii_case("device_id") {
            if value.eq_ignore_ascii_case("all") {
                RouteTarget::EdgeAll
            } else {
                RouteTarget::EdgeDevice(value.to_string())
            }
        } else if key.eq_ignore_ascii_case("group_id") {
            RouteTarget::EdgeGroup(value.to_string())
        } else {
            return Err(code_error(
                ROUTE_INVALID,
                "EDGE route only supports device_id/group_id",
            ));
        };
        return Ok(ParsedRouteSql {
            body_sql: body.to_string(),
            target,
        });
    }

    Ok(ParsedRouteSql {
        body_sql: without_semicolon.to_string(),
        target: RouteTarget::Hub,
    })
}

// ### 修改记录 (2026-03-10)
// - 原因: 统一 SQL 语法只扩展 FOR 尾部路由，不改 SQL 主体
// - 目的: 通过首关键字快速判定 DDL，降低运行期开销
fn first_keyword(sql: &str) -> String {
    sql.split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

// ### 修改记录 (2026-03-10)
// - 原因: 键列一致性只在 DDL 场景校验
// - 目的: 将 DDL 与 DML 约束路径解耦，便于后续扩展
fn is_ddl(sql: &str) -> bool {
    matches!(first_keyword(sql).as_str(), "CREATE" | "ALTER" | "DROP")
}

// ### 修改记录 (2026-03-10)
// - 原因: CREATE TABLE 列定义里可能包含函数括号
// - 目的: 只按顶层逗号拆分列定义，避免误切分
fn split_top_level_by_comma(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (idx, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                out.push(input[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    out.push(input[start..].trim());
    out
}

// ### 修改记录 (2026-03-10)
// - 原因: 规范要求主键/唯一键列不能声明 eventual
// - 目的: 在 SQL 进入状态机前做快速一致性拦截
fn validate_create_table_key_consistency(sql: &str) -> Result<()> {
    let upper = sql.to_ascii_uppercase();
    if !upper.starts_with("CREATE TABLE") {
        return Ok(());
    }
    if !upper.contains("CONSISTENCY:EVENTUAL") {
        return Ok(());
    }
    let start = sql
        .find('(')
        .ok_or_else(|| code_error(ROUTE_INVALID, "CREATE TABLE missing '('"))?;
    let end = sql
        .rfind(')')
        .ok_or_else(|| code_error(ROUTE_INVALID, "CREATE TABLE missing ')'"))?;
    if end <= start {
        return Err(code_error(ROUTE_INVALID, "CREATE TABLE column list is invalid"));
    }
    let columns = &sql[start + 1..end];
    let defs = split_top_level_by_comma(columns);
    for def in defs {
        let def_upper = def.to_ascii_uppercase();
        if def_upper.contains("CONSISTENCY:EVENTUAL")
            && (def_upper.contains("PRIMARY KEY") || def_upper.contains("UNIQUE"))
        {
            return Err(code_error(
                COLUMN_CONSISTENCY_INVALID,
                "key column cannot declare eventual consistency",
            ));
        }
    }
    Ok(())
}

// ### 修改记录 (2026-03-10)
// - 原因: ALTER TABLE ADD COLUMN 也可能携带一致性注释
// - 目的: 保持 DDL 路径对键列一致性约束一致
fn validate_alter_add_column_key_consistency(sql: &str) -> Result<()> {
    let upper = sql.to_ascii_uppercase();
    if !upper.starts_with("ALTER TABLE") || !upper.contains("ADD COLUMN") {
        return Ok(());
    }
    if upper.contains("CONSISTENCY:EVENTUAL")
        && (upper.contains("PRIMARY KEY") || upper.contains("UNIQUE"))
    {
        return Err(code_error(
            COLUMN_CONSISTENCY_INVALID,
            "key column cannot declare eventual consistency",
        ));
    }
    Ok(())
}

// ### 修改记录 (2026-03-10)
// - 原因: 系统表 sys_column_consistency 是一致性元数据事实源
// - 目的: 防止将 is_key_column=1 的元数据写成 eventual
fn validate_sys_column_consistency_dml(sql: &str) -> Result<()> {
    let upper = sql.to_ascii_uppercase();
    if !upper.contains("SYS_COLUMN_CONSISTENCY") {
        return Ok(());
    }
    if upper.contains("EVENTUAL") && upper.contains("IS_KEY_COLUMN") && upper.contains("1") {
        return Err(code_error(
            COLUMN_CONSISTENCY_INVALID,
            "sys_column_consistency key column must stay strong",
        ));
    }
    Ok(())
}

// ### 修改记录 (2026-03-10)
// - 原因: 需要在单一入口同时处理路由尾缀与一致性约束
// - 目的: 返回可直接执行的标准 SQL 主体，避免 SQLite 语法报错
fn normalize_sql_for_write(raw_sql: &str) -> Result<String> {
    let parsed = parse_route_sql(raw_sql)?;
    match &parsed.target {
        RouteTarget::Hub | RouteTarget::EdgeAll => {}
        RouteTarget::EdgeDevice(device_id) => {
            if device_id.is_empty() {
                return Err(code_error(ROUTE_INVALID, "device_id cannot be empty"));
            }
        }
        RouteTarget::EdgeGroup(group_id) => {
            if group_id.is_empty() {
                return Err(code_error(ROUTE_INVALID, "group_id cannot be empty"));
            }
        }
    }
    if is_ddl(&parsed.body_sql) {
        validate_create_table_key_consistency(&parsed.body_sql)?;
        validate_alter_add_column_key_consistency(&parsed.body_sql)?;
    }
    validate_sys_column_consistency_dml(&parsed.body_sql)?;
    Ok(parsed.body_sql)
}

#[derive(Clone)]
pub struct BatchConfig {
    pub max_batch_size: usize,
    pub max_delay_ms: u64,
    pub max_queue_size: usize,
    pub max_wait_ms: u64,
}

struct BatchItem {
    sql: String,
    resp: oneshot::Sender<Result<usize>>,
    cancelled: Arc<AtomicBool>,
}

struct BatchWriter {
    sender: mpsc::Sender<BatchItem>,
}

impl BatchWriter {
    /// ### 修改记录 (2026-02-28)
    /// - 原因: Smart Batcher 需要将聚合后的请求提交给 RaftNode
    /// - 目的: 修复 Batcher 绕过 Raft 直接写库的架构缺陷
    fn new(raft_node: RaftNode, config: BatchConfig) -> Self {
        let queue_size = config.max_queue_size.max(1);
        let (tx, mut rx) = mpsc::channel::<BatchItem>(queue_size);
        let max_batch_size = config.max_batch_size;
        let max_delay_ms = config.max_delay_ms;

        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(max_batch_size);

            loop {
                // 1. 获取第一个元素 (如果通道关闭则退出)
                let first = match rx.recv().await {
                    Some(item) => item,
                    None => break,
                };

                batch.push(first);

                // 2. 尝试收集更多元素，直到超时或达到最大批次
                let deadline = tokio::time::Instant::now() + Duration::from_millis(max_delay_ms);

                loop {
                    if batch.len() >= max_batch_size {
                        break;
                    }

                    let timeout = tokio::time::sleep_until(deadline);
                    tokio::pin!(timeout);

                    tokio::select! {
                        _ = timeout => {
                            break;
                        }
                        res = rx.recv() => {
                            match res {
                                Some(item) => batch.push(item),
                                None => break, // 通道关闭，处理剩余批次
                            }
                        }
                    }
                }

                // 3. 过滤已取消的请求
                let mut active_items = Vec::with_capacity(batch.len());
                for item in batch.drain(..) {
                    if item.cancelled.load(Ordering::SeqCst) {
                        let _ = item.resp.send(Err(anyhow!("Batch cancelled")));
                    } else {
                        active_items.push(item);
                    }
                }

                if active_items.is_empty() {
                    continue;
                }

                // 4. 提交给 RaftNode
                let sqls: Vec<String> = active_items.iter().map(|item| item.sql.clone()).collect();
                // 批次大小用于返回结果 (目前 Raft 响应只是受影响行数，这里简化处理)
                // 理想情况下 Raft 应返回每条 SQL 的执行结果
                match raft_node.apply_sql_batch(sqls).await {
                    Ok(_count) => {
                        // 暂时假设每条 SQL 成功执行，返回 1 (或平均分配?)
                        // 由于 apply_sql_batch 返回的是批次大小
                        // 我们给每个请求返回 1 表示成功
                        for item in active_items {
                            let _ = item.resp.send(Ok(1));
                        }
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        for item in active_items {
                            let _ = item.resp.send(Err(anyhow!(err_msg.clone())));
                        }
                    }
                }
            }
        });

        Self { sender: tx }
    }

    async fn enqueue(&self, sql: String, max_wait_ms: u64) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.sender
            .try_send(BatchItem {
                sql,
                resp: tx,
                cancelled: cancelled.clone(),
            })
            .map_err(|_| anyhow!("Batch queue full"))?;
        if max_wait_ms == 0 {
            return rx.await.map_err(|_| anyhow!("Batch response dropped"))?;
        }
        let res = tokio::time::timeout(Duration::from_millis(max_wait_ms), rx)
            .await
            .map_err(|_| {
                cancelled.store(true, Ordering::SeqCst);
                anyhow!("Batch wait timeout")
            })?;
        res.map_err(|_| anyhow!("Batch response dropped"))?
    }
}

/// ### 修改记录 (2026-02-17)
/// - 原因: 需要封装路由判断
/// - 目的: 统一 write/read 的入口
pub struct Router {
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 测试需要控制 Leader 状态
    /// - 目的: 简化最小实现验证
    is_leader: bool,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要在本地执行写入
    /// - 目的: 在单节点或 leader 场景复用状态机
    state_machine: Option<SqliteStateMachine>,
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要接入 Raft 写路径
    /// - 目的: 统一 Router 写入到 RaftNode
    raft_node: Option<RaftNode>,
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 非 Leader 需要转发到 Leader
    /// - 目的: 保存 Leader 地址以便执行 gRPC 转发
    leader_addr: Option<String>,
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 转发需要超时保护
    /// - 目的: 避免转发请求无限等待
    forward_timeout_ms: Option<u64>,
    batcher: Option<BatchWriter>,
    batch_max_wait_ms: Option<u64>,
    test_cluster: Option<Arc<Mutex<TestCluster>>>,
}

impl Router {
    // ### 修改记录 (2026-03-10)
    // - 原因: 路由目标校验需要构造安全 SQL 字面量
    // - 目的: 防止 device_id/group_id 包含单引号时破坏查询语句
    fn escape_sql_literal(raw: &str) -> String {
        raw.replace('\'', "''")
    }

    // ### 修改记录 (2026-03-10)
    // - 原因: 方案A要求在写入前校验路由目标是否存在
    // - 目的: 抽象统一查询入口，便于 state_machine/raft_node 复用
    async fn query_route_target_exists(&self, sql: String) -> Result<RouteTargetLookup> {
        if let Some(state_machine) = &self.state_machine {
            match state_machine.query_scalar(sql.clone()).await {
                Ok(value) => {
                    return if value.trim() == "1" {
                        Ok(RouteTargetLookup::Exists)
                    } else {
                        Ok(RouteTargetLookup::NotFound)
                    };
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.to_ascii_lowercase().contains("no such table") {
                        return Ok(RouteTargetLookup::MetadataMissing);
                    }
                    return Err(e);
                }
            }
        }
        if let Some(raft_node) = &self.raft_node {
            match raft_node.query_scalar(sql).await {
                Ok(value) => {
                    return if value.trim() == "1" {
                        Ok(RouteTargetLookup::Exists)
                    } else {
                        Ok(RouteTargetLookup::NotFound)
                    };
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.to_ascii_lowercase().contains("no such table") {
                        return Ok(RouteTargetLookup::MetadataMissing);
                    }
                    return Err(e);
                }
            }
        }
        Ok(RouteTargetLookup::Exists)
    }

    // ### 修改记录 (2026-03-10)
    // - 原因: 统一 SQL 规范要求边缘路由目标可校验
    // - 目的: 在执行前拦截不存在的 group/device，返回 TARGET_NOT_FOUND
    async fn validate_route_target_exists(&self, target: &RouteTarget) -> Result<()> {
        match target {
            RouteTarget::Hub | RouteTarget::EdgeAll => Ok(()),
            RouteTarget::EdgeDevice(device_id) => {
                let exists_sql = format!(
                    "SELECT EXISTS(SELECT 1 FROM sys_edge_device WHERE device_id='{}')",
                    Self::escape_sql_literal(device_id)
                );
                match self.query_route_target_exists(exists_sql).await? {
                    RouteTargetLookup::Exists => Ok(()),
                    RouteTargetLookup::NotFound => Err(code_error(
                        TARGET_NOT_FOUND,
                        "edge target device_id not found in sys_edge_device",
                    )),
                    RouteTargetLookup::MetadataMissing => Err(code_error(
                        TARGET_METADATA_MISSING,
                        "sys_edge_device metadata table is missing",
                    )),
                }
            }
            RouteTarget::EdgeGroup(group_id) => {
                let exists_sql = format!(
                    "SELECT EXISTS(SELECT 1 FROM sys_edge_group WHERE group_id='{}')",
                    Self::escape_sql_literal(group_id)
                );
                match self.query_route_target_exists(exists_sql).await? {
                    RouteTargetLookup::Exists => Ok(()),
                    RouteTargetLookup::NotFound => Err(code_error(
                        TARGET_NOT_FOUND,
                        "edge target group_id not found in sys_edge_group",
                    )),
                    RouteTargetLookup::MetadataMissing => Err(code_error(
                        TARGET_METADATA_MISSING,
                        "sys_edge_group metadata table is missing",
                    )),
                }
            }
        }
    }

    fn is_raft_leader(&self) -> bool {
        if let Some(raft_node) = &self.raft_node {
            let metrics_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                raft_node.raft.metrics().borrow().clone()
            }));
            if let Ok(metrics) = metrics_result {
                return metrics.state == ServerState::Leader
                    && matches!(metrics.current_leader, Some(id) if id == raft_node.node_id());
            }
            return false;
        }
        self.is_leader
    }

    async fn forward_write_to_leader(&self, sql: String) -> Result<usize> {
        let leader_addr = self
            .leader_addr
            .as_ref()
            .ok_or_else(|| anyhow!("Not leader and leader address unknown"))?;
        let mut client = DatabaseServiceClient::connect(leader_addr.clone())
            .await
            .map_err(|e| anyhow!("Leader connect failed: {}", e))?;
        let req = ExecuteRequest { sql };
        let timeout_ms = self.forward_timeout_ms.unwrap_or(2000);
        let res = tokio::time::timeout(Duration::from_millis(timeout_ms), client.execute(req))
            .await
            .map_err(|_| anyhow!("Leader forward timeout"))?;
        let resp = res.map_err(|e| anyhow!("Leader execute failed: {}", e))?;
        Ok(resp.into_inner().rows_affected as usize)
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 测试需要构造最小 Router
    /// - 目的: 避免依赖真实 Raft 实例
    pub fn new_for_test(is_leader: bool) -> Self {
        // ### 修改记录 (2026-03-03)
        // - 原因: 测试需要可写的状态机
        // - 目的: 保证 write_batch 能返回非 0 结果
        // - 备注: 使用临时路径避免污染本地文件
        let state_machine = if is_leader {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let mut path = std::env::temp_dir();
            // ### 修改记录 (2026-03-10)
            // - 原因: 并发测试中纳秒时间戳可能重复导致 SQLite 文件冲突
            // - 目的: 追加 UUID 彻底消除测试库路径碰撞
            path.push(format!("check_program_test_{}_{}.db", suffix, Uuid::new_v4()));
            let path_str = path.to_string_lossy().to_string();
            // ### 修改记录 (2026-03-03)
            // - 原因: 需要确保测试立刻失败而非静默返回 0
            // - 目的: 对齐 new_for_test 的可写预期
            Some(
                SqliteStateMachine::new(path_str)
                    .expect("new_for_test requires sqlite state machine"),
            )
        } else {
            None
        };
        Self {
            is_leader,
            state_machine,
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
            test_cluster: None,
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要统一写入口
    /// - 目的: 根据 Leader 状态选择路径
    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要返回 rows_affected
    /// - 目的: 保持 SQLite 风格的返回值
    /// ### 修改记录 (2026-02-25)
    /// - 原因: 非 Leader 需要转发到 Leader
    /// - 目的: 统一由 Router 承担转发职责
    pub async fn write(&self, sql: String) -> Result<usize> {
        // ### 修改记录 (2026-03-12)
        // - 原因: follower 预归一化会丢失 FOR EDGE 路由尾缀，导致 leader 无法做目标校验
        // - 目的: 非 Leader 场景转发原始 SQL，确保错误码由 leader 侧统一产出与透传
        if !self.is_raft_leader() {
            return self.forward_write_to_leader(sql).await;
        }
        let parsed_route = parse_route_sql(&sql)?;
        self.validate_route_target_exists(&parsed_route.target).await?;
        let normalized_sql = normalize_sql_for_write(&sql)?;
        if let Some(cluster) = &self.test_cluster {
            let guard = cluster.lock().await;
            guard.write(normalized_sql).await?;
            return Ok(1);
        }
        if let Some(batcher) = &self.batcher {
            let wait_ms = self.batch_max_wait_ms.unwrap_or(0);
            batcher.enqueue(normalized_sql, wait_ms).await
        } else if let Some(raft_node) = &self.raft_node {
            raft_node.apply_sql(normalized_sql).await
        } else if let Some(state_machine) = &self.state_machine {
            state_machine.apply_write(normalized_sql).await
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-03-01)
    /// - 原因: EdgeGateway Smart Batcher 需要批量写入接口
    /// - 目的: 将一批 SQL 作为一个写入单元提交
    pub async fn write_batch(&self, sqls: Vec<String>) -> Result<usize> {
        if sqls.is_empty() {
            return Ok(0);
        }
        // ### 修改记录 (2026-03-12)
        // - 原因: follower 批量路径此前转发的是归一化 SQL，leader 无法识别路由上下文
        // - 目的: 非 Leader 批量写入直接转发原始 SQL，保持与单条写入一致的错误语义
        if !self.is_raft_leader() {
            let mut total_rows = 0usize;
            for sql in sqls {
                total_rows = total_rows.saturating_add(self.forward_write_to_leader(sql).await?);
            }
            return Ok(total_rows);
        }
        let mut normalized_sqls = Vec::with_capacity(sqls.len());
        for sql in sqls {
            let parsed_route = parse_route_sql(&sql)?;
            self.validate_route_target_exists(&parsed_route.target).await?;
            normalized_sqls.push(normalize_sql_for_write(&sql)?);
        }
        if let Some(cluster) = &self.test_cluster {
            let guard = cluster.lock().await;
            return guard.write_batch(normalized_sqls).await;
        }

        if let Some(raft_node) = &self.raft_node {
            raft_node.apply_sql_batch(normalized_sqls).await
        } else if let Some(state_machine) = &self.state_machine {
            let results = state_machine.apply_batch(normalized_sqls).await?;
            Ok(results.len())
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要提供最小读路径
    /// - 目的: 支持一致性校验读取
    pub async fn get_version(&self, table: String) -> Result<i32> {
        let is_leader = self.is_raft_leader();
        if is_leader {
            if let Some(state_machine) = &self.state_machine {
                state_machine.get_max_version(table).await
            } else if let Some(raft_node) = &self.raft_node {
                let sql = format!("SELECT MAX(version) FROM {}", table);
                let value = raft_node.query_scalar(sql).await?;
                Ok(value.parse::<i32>().unwrap_or(0))
            } else {
                Ok(0)
            }
        } else if let Some(leader_addr) = &self.leader_addr {
            // ### 修改记录 (2026-02-26)
            // - 原因: 非 Leader 读可能返回陈旧数据
            // - 目的: 统一转发到 Leader 保持一致性
            let mut client = DatabaseServiceClient::connect(leader_addr.clone())
                .await
                .map_err(|e| anyhow!("Leader connect failed: {}", e))?;
            // ### 修改记录 (2026-02-26)
            // - 原因: 读路径需要与 gRPC GetVersion 对齐
            // - 目的: 复用协议定义避免重复接口
            let req = GetVersionRequest { table };
            // ### 修改记录 (2026-02-26)
            // - 原因: 读转发同样需要超时保护
            // - 目的: 避免长时间阻塞调用方
            let timeout_ms = self.forward_timeout_ms.unwrap_or(2000);
            let res =
                tokio::time::timeout(Duration::from_millis(timeout_ms), client.get_version(req))
                    .await
                    .map_err(|_| anyhow!("Leader forward timeout"))?;
            // ### 修改记录 (2026-02-26)
            // - 原因: 需要保留原始 gRPC 错误
            // - 目的: 便于定位 Leader 读失败原因
            let resp = res.map_err(|e| anyhow!("Leader get_version failed: {}", e))?;
            Ok(resp.into_inner().version)
        } else {
            Err(anyhow!("Not leader and leader address unknown"))
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要在本地 leader 场景执行写入
    /// - 目的: 为 gRPC 服务提供最小可用写入路径
    pub fn new_local_leader(path: String) -> Result<Self> {
        let state_machine = SqliteStateMachine::new(path)?;
        Ok(Self {
            is_leader: true,
            state_machine: Some(state_machine),
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
            test_cluster: None,
        })
    }

    pub fn new_local_leader_with_batch(raft_node: RaftNode, config: BatchConfig) -> Result<Self> {
        let batcher = BatchWriter::new(raft_node.clone(), config.clone());
        Ok(Self {
            is_leader: true,
            state_machine: None,
            raft_node: Some(raft_node),
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: Some(batcher),
            batch_max_wait_ms: Some(config.max_wait_ms),
            test_cluster: None,
        })
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要 Router 绑定 RaftNode
    /// - 目的: 让写入路径走 RaftNode 封装
    pub fn new_with_raft(raft_node: RaftNode) -> Self {
        let metrics = raft_node.raft.metrics().borrow().clone();
        let is_leader = metrics.state == ServerState::Leader
            && matches!(metrics.current_leader, Some(id) if id == raft_node.node_id());
        Self {
            is_leader,
            state_machine: None,
            raft_node: Some(raft_node),
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
            test_cluster: None,
        }
    }

    pub fn new_with_test_cluster(cluster: Arc<Mutex<TestCluster>>) -> Self {
        Self {
            is_leader: true,
            state_machine: None,
            raft_node: None,
            leader_addr: None,
            forward_timeout_ms: None,
            batcher: None,
            batch_max_wait_ms: None,
            test_cluster: Some(cluster),
        }
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要明确构造非 Leader Router
    /// - 目的: 支持转发到指定 Leader 地址
    pub fn new_follower_with_leader_addr(leader_addr: String) -> Self {
        // ### 修改记录 (2026-02-25)
        // - 原因: 需要默认超时参数
        // - 目的: 保持旧构造函数行为不变
        Self::new_follower_with_leader_addr_and_timeout(leader_addr, 2000)
    }

    /// ### 修改记录 (2026-02-25)
    /// - 原因: 需要可配置转发超时
    /// - 目的: 让测试可稳定触发超时路径
    pub fn new_follower_with_leader_addr_and_timeout(leader_addr: String, timeout_ms: u64) -> Self {
        Self {
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 构造需要固定角色
            // - 目的: 确保进入转发逻辑
            is_leader: false,
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不持有状态机
            // - 目的: 避免误走本地写路径
            state_machine: None,
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不持有 RaftNode
            // - 目的: 防止绕过转发
            raft_node: None,
            // ### 修改记录 (2026-02-25)
            // - 原因: 转发需要 Leader 地址
            // - 目的: 作为 gRPC 目标
            leader_addr: Some(leader_addr),
            // ### 修改记录 (2026-02-25)
            // - 原因: 需要精确控制超时
            // - 目的: 让超时测试可控
            forward_timeout_ms: Some(timeout_ms),
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不使用批处理
            // - 目的: 保持行为简单
            batcher: None,
            // ### 修改记录 (2026-02-25)
            // - 原因: 非 Leader 不使用批处理
            // - 目的: 避免无效配置
            batch_max_wait_ms: None,
            test_cluster: None,
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要更新元数据
    /// - 目的: 为快照调度提供写入口
    pub async fn update_meta(&self, key: String, value: String) -> Result<()> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                state_machine.update_meta(key, value).await
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取元数据
    /// - 目的: 支持快照调度判断
    pub async fn get_meta_u64(&self, key: &str) -> Result<u64> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                let sql = format!("SELECT value FROM _raft_meta WHERE key='{}'", key);
                let value = state_machine.query_scalar(sql).await?;
                Ok(value.parse::<u64>().unwrap_or(0))
            } else if let Some(raft_node) = &self.raft_node {
                // ### 修改记录 (2026-02-28)
                // - 原因: RaftNode 场景也需要查询元数据
                // - 目的: 修复快照调度在 Raft 模式下的空指针异常
                let sql = format!("SELECT value FROM _raft_meta WHERE key='{}'", key);
                let value = raft_node.query_scalar(sql).await?;
                Ok(value.parse::<u64>().unwrap_or(0))
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要读取 SQLite 时间
    /// - 目的: 统一以数据库时间为基准
    pub async fn current_unix_timestamp(&self) -> Result<u64> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                let value = state_machine
                    .query_scalar("SELECT strftime('%s','now')".to_string())
                    .await?;
                Ok(value.parse::<u64>().unwrap_or(0))
            } else if let Some(raft_node) = &self.raft_node {
                // ### 修改记录 (2026-02-28)
                // - 原因: RaftNode 场景也需要查询时间
                // - 目的: 修复快照调度在 Raft 模式下的空指针异常
                let value = raft_node
                    .query_scalar("SELECT strftime('%s','now')".to_string())
                    .await?;
                Ok(value.parse::<u64>().unwrap_or(0))
            } else {
                Ok(0)
            }
        } else {
            Ok(0)
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 需要执行 VACUUM INTO
    /// - 目的: 生成快照文件
    pub async fn vacuum_into(&self, snapshot_path: String) -> Result<()> {
        if self.is_leader {
            let safe_path = snapshot_path.replace('\'', "''");
            let sql = format!("VACUUM INTO '{}'", safe_path);

            if let Some(state_machine) = &self.state_machine {
                let _ = state_machine.apply_write(sql).await?;
                Ok(())
            } else if let Some(raft_node) = &self.raft_node {
                // ### 修改记录 (2026-02-28)
                // - 原因: RaftNode 场景也需要执行快照
                // - 目的: 通过 RaftLog 触发各节点快照 (注意：VACUUM INTO 是本地操作还是分布式操作？
                //        通常快照是本地状态机行为，但如果是 SQL 触发，则会复制到所有节点)
                //        这里我们假设它是通过 Raft 复制的指令
                raft_node.apply_sql(sql).await?;
                Ok(())
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    /// ### 修改记录 (2026-02-17)
    /// - 原因: 快照后需要轮转 WAL
    /// - 目的: 控制日志膨胀
    pub async fn rotate_wal(&self) -> Result<()> {
        if self.is_leader {
            if let Some(state_machine) = &self.state_machine {
                state_machine.rotate_wal().await
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }
}

// ### 修改记录 (2026-03-02)
// - 原因: 需要复现测试环境 new_for_test 写入返回 0 的问题
// - 目的: 以单元测试锁定批量写入的期望行为
// - 目的: 为后续修复提供可验证的失败用例
// - 备注: 该测试仅覆盖测试构造函数，不影响生产路径
// - 备注: 通过最小 SQL 组合验证写入结果长度
#[cfg(test)]
mod tests {
    use super::*;

    // ### 修改记录 (2026-03-02)
    // - 原因: 需要异步验证 write_batch 返回值
    // - 目的: 证明 new_for_test(true) 必须具备可写状态机
    // - 目的: 在修复前触发断言失败以满足 TDD 约束
    // - 备注: SQL 组合包含 DDL + DML，确保 batch 返回长度可判断
    // - 备注: 该测试仅验证行为，不依赖外部网络或 gRPC
    #[tokio::test]
    async fn new_for_test_leader_write_batch_returns_rows() -> Result<()> {
        // ### 修改记录 (2026-03-02)
        // - 原因: 需要复现实例化路径
        // - 目的: 覆盖 new_for_test(true) 的真实行为
        let router = Router::new_for_test(true);

        // ### 修改记录 (2026-03-02)
        // - 原因: 需要最小化写入场景
        // - 目的: 组合 DDL 与 DML 形成可验证的 batch
        // - 备注: CREATE TABLE IF NOT EXISTS 保证重复执行安全
        let sqls = vec![
            "CREATE TABLE IF NOT EXISTS t(id INTEGER)".to_string(),
            "INSERT INTO t(id) VALUES (1)".to_string(),
        ];

        // ### 修改记录 (2026-03-02)
        // - 原因: 需要验证批量写入返回值
        // - 目的: 确保返回条数与提交 SQL 数量一致
        let rows = router.write_batch(sqls).await?;
        assert_eq!(rows, 2);

        Ok(())
    }

    #[tokio::test]
    async fn write_accepts_for_hub_suffix() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 统一 SQL 规范要求 FOR HUB 由路由层识别而非 SQLite
        // - 目的: 锁定尾缀剥离后 SQL 仍可执行
        let router = Router::new_for_test(true);
        let res = router
            .write("CREATE TABLE IF NOT EXISTS hub_only(id INTEGER) FOR HUB;".to_string())
            .await;
        assert!(res.is_ok(), "expected FOR HUB to be accepted, got {res:?}");
    }

    #[tokio::test]
    async fn write_rejects_invalid_edge_route_clause() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 规范要求 FOR EDGE 仅允许 device_id/group_id 参数
        // - 目的: 锁定 ROUTE_INVALID 错误码行为
        let router = Router::new_for_test(true);
        let err = router
            .write("INSERT INTO t(id) VALUES (1) FOR EDGE(foo=1);".to_string())
            .await
            .expect_err("invalid route clause should fail");
        assert!(
            err.to_string().contains("ROUTE_INVALID"),
            "expected ROUTE_INVALID, got {err}"
        );
    }

    #[tokio::test]
    async fn write_rejects_eventual_consistency_on_key_column() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 键列一致性必须强一致
        // - 目的: 锁定 COLUMN_CONSISTENCY_INVALID 错误码
        let router = Router::new_for_test(true);
        let err = router
            .write(
                "CREATE TABLE telemetry_k(id TEXT PRIMARY KEY /* consistency:eventual */, temp REAL) FOR HUB;"
                    .to_string(),
            )
            .await
            .expect_err("key column eventual should fail");
        assert!(
            err.to_string().contains("COLUMN_CONSISTENCY_INVALID"),
            "expected COLUMN_CONSISTENCY_INVALID, got {err}"
        );
    }

    #[tokio::test]
    async fn write_allows_eventual_consistency_on_non_key_column() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 规范允许非键列声明 eventual
        // - 目的: 避免误伤合法 SQL
        let router = Router::new_for_test(true);
        let res = router
            .write(
                "CREATE TABLE telemetry_ok(id TEXT PRIMARY KEY, temp REAL /* consistency:eventual */) FOR HUB;"
                    .to_string(),
            )
            .await;
        assert!(res.is_ok(), "non-key eventual should be allowed, got {res:?}");
    }

    #[tokio::test]
    async fn write_batch_rejects_invalid_route_clause() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 批量写入也必须逐条经过统一路由校验
        // - 目的: 防止非法路由 SQL 混入批次
        let router = Router::new_for_test(true);
        let err = router
            .write_batch(vec![
                "CREATE TABLE IF NOT EXISTS batch_t(id INTEGER) FOR HUB;".to_string(),
                "INSERT INTO batch_t(id) VALUES (1) FOR EDGE(xxx=1);".to_string(),
            ])
            .await
            .expect_err("invalid route in batch should fail");
        assert!(
            err.to_string().contains("ROUTE_INVALID"),
            "expected ROUTE_INVALID, got {err}"
        );
    }

    #[tokio::test]
    async fn write_accepts_for_edge_group_suffix() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 新规范增加 FOR EDGE(group_id=...) 路由形态
        // - 目的: 锁定 group 路由尾缀可被正确剥离并执行
        // ### 修改记录 (2026-03-10)
        // - 原因: 方案A要求在执行前校验目标存在
        // - 目的: 用最小系统表数据保障“存在时可通过”语义
        let router = Router::new_for_test(true);
        let setup = router
            .write(
                "CREATE TABLE IF NOT EXISTS sys_edge_group(group_id TEXT PRIMARY KEY) FOR HUB;"
                    .to_string(),
            )
            .await;
        assert!(
            setup.is_ok(),
            "failed to create sys_edge_group, got {setup:?}"
        );
        let seed = router
            .write("INSERT INTO sys_edge_group(group_id) VALUES ('12') FOR HUB;".to_string())
            .await;
        assert!(seed.is_ok(), "failed to seed sys_edge_group, got {seed:?}");
        let res = router
            .write("CREATE TABLE IF NOT EXISTS edge_group_ok(id INTEGER) FOR EDGE(group_id=12);".to_string())
            .await;
        assert!(
            res.is_ok(),
            "expected FOR EDGE(group_id=...) to be accepted, got {res:?}"
        );
    }

    #[tokio::test]
    async fn write_rejects_for_edge_group_when_target_not_found() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 方案A要求补齐目标存在性校验
        // - 目的: 锁定 group_id 不存在时必须返回 TARGET_NOT_FOUND
        let router = Router::new_for_test(true);
        let setup = router
            .write(
                "CREATE TABLE IF NOT EXISTS sys_edge_group(group_id TEXT PRIMARY KEY) FOR HUB;"
                    .to_string(),
            )
            .await;
        assert!(
            setup.is_ok(),
            "failed to create sys_edge_group, got {setup:?}"
        );
        let err = router
            .write("CREATE TABLE IF NOT EXISTS edge_group_miss(id INTEGER) FOR EDGE(group_id=404);".to_string())
            .await
            .expect_err("missing group target should fail");
        assert!(
            err.to_string().contains("TARGET_NOT_FOUND"),
            "expected TARGET_NOT_FOUND, got {err}"
        );
    }

    #[tokio::test]
    async fn write_rejects_for_edge_device_when_target_not_found() {
        // ### 修改记录 (2026-03-11)
        // - 原因: 计划要求补齐 device 目标不存在场景
        // - 目的: 保证 device/group 两类 EDGE 路由错误语义一致
        let router = Router::new_for_test(true);
        let setup = router
            .write(
                "CREATE TABLE IF NOT EXISTS sys_edge_device(device_id TEXT PRIMARY KEY) FOR HUB;"
                    .to_string(),
            )
            .await;
        assert!(
            setup.is_ok(),
            "failed to create sys_edge_device, got {setup:?}"
        );
        let err = router
            .write(
                "CREATE TABLE IF NOT EXISTS edge_device_miss(id INTEGER) FOR EDGE(device_id=dev-404);"
                    .to_string(),
            )
            .await
            .expect_err("missing device target should fail");
        assert!(
            err.to_string().contains("TARGET_NOT_FOUND"),
            "expected TARGET_NOT_FOUND, got {err}"
        );
    }

    #[tokio::test]
    async fn write_batch_rejects_for_edge_device_when_target_not_found() {
        // ### 修改记录 (2026-03-11)
        // - 原因: 批量入口也需要覆盖 device 目标不存在场景
        // - 目的: 防止 write 与 write_batch 在目标校验上出现行为分叉
        let router = Router::new_for_test(true);
        let setup = router
            .write(
                "CREATE TABLE IF NOT EXISTS sys_edge_device(device_id TEXT PRIMARY KEY) FOR HUB;"
                    .to_string(),
            )
            .await;
        assert!(
            setup.is_ok(),
            "failed to create sys_edge_device, got {setup:?}"
        );
        let err = router
            .write_batch(vec![
                "CREATE TABLE IF NOT EXISTS batch_device_ok(id INTEGER) FOR HUB;".to_string(),
                "INSERT INTO batch_device_ok(id) VALUES (1) FOR EDGE(device_id=dev-404);"
                    .to_string(),
            ])
            .await
            .expect_err("missing device target in batch should fail");
        assert!(
            err.to_string().contains("TARGET_NOT_FOUND"),
            "expected TARGET_NOT_FOUND, got {err}"
        );
    }

    #[tokio::test]
    async fn write_batch_rejects_for_edge_group_when_target_not_found() {
        // ### 修改记录 (2026-03-11)
        // - 原因: 需要补齐 batch 在 group 目标不存在时的行为覆盖
        // - 目的: 保证 device/group 在批量入口上的错误语义一致
        let router = Router::new_for_test(true);
        let setup = router
            .write(
                "CREATE TABLE IF NOT EXISTS sys_edge_group(group_id TEXT PRIMARY KEY) FOR HUB;"
                    .to_string(),
            )
            .await;
        assert!(
            setup.is_ok(),
            "failed to create sys_edge_group, got {setup:?}"
        );
        let err = router
            .write_batch(vec![
                "CREATE TABLE IF NOT EXISTS batch_group_ok(id INTEGER) FOR HUB;".to_string(),
                "INSERT INTO batch_group_ok(id) VALUES (1) FOR EDGE(group_id=404);".to_string(),
            ])
            .await
            .expect_err("missing group target in batch should fail");
        assert!(
            err.to_string().contains("TARGET_NOT_FOUND"),
            "expected TARGET_NOT_FOUND, got {err}"
        );
    }

    #[tokio::test]
    async fn write_rejects_for_edge_device_when_metadata_table_missing() {
        let router = Router::new_for_test(true);
        let err = router
            .write(
                "CREATE TABLE IF NOT EXISTS edge_device_meta_missing(id INTEGER) FOR EDGE(device_id=dev-404);"
                    .to_string(),
            )
            .await
            .expect_err("missing sys_edge_device table should fail");
        assert!(
            err.to_string().contains("TARGET_METADATA_MISSING"),
            "expected TARGET_METADATA_MISSING, got {err}"
        );
    }

    #[tokio::test]
    async fn write_batch_rejects_for_edge_group_when_metadata_table_missing() {
        let router = Router::new_for_test(true);
        let err = router
            .write_batch(vec![
                "CREATE TABLE IF NOT EXISTS batch_meta_missing(id INTEGER) FOR HUB;".to_string(),
                "INSERT INTO batch_meta_missing(id) VALUES (1) FOR EDGE(group_id=404);".to_string(),
            ])
            .await
            .expect_err("missing sys_edge_group table in batch should fail");
        assert!(
            err.to_string().contains("TARGET_METADATA_MISSING"),
            "expected TARGET_METADATA_MISSING, got {err}"
        );
    }

    #[tokio::test]
    async fn write_rejects_for_edge_group_when_metadata_table_missing() {
        // ### 修改记录 (2026-03-12)
        // - 原因: 当前仅覆盖了 write/device 与 write_batch/group 的缺表场景
        // - 目的: 补齐 write/group 缺失 sys_edge_group 的对称回归覆盖
        let router = Router::new_for_test(true);
        let err = router
            .write(
                "CREATE TABLE IF NOT EXISTS edge_group_meta_missing(id INTEGER) FOR EDGE(group_id=404);"
                    .to_string(),
            )
            .await
            .expect_err("missing sys_edge_group table should fail");
        assert!(
            err.to_string().contains("TARGET_METADATA_MISSING"),
            "expected TARGET_METADATA_MISSING, got {err}"
        );
    }

    #[tokio::test]
    async fn write_rejects_sys_column_consistency_key_eventual_insert() {
        // ### 修改记录 (2026-03-10)
        // - 原因: 系统表约束要求 is_key_column=1 的一致性必须是 strong
        // - 目的: 锁定元数据写入路径的硬约束行为
        let router = Router::new_for_test(true);
        let err = router
            .write(
                "INSERT INTO sys_column_consistency(schema_name, table_name, column_name, consistency_level, is_key_column) VALUES ('main', 't', 'id', 'eventual', 1) FOR HUB;"
                    .to_string(),
            )
            .await
            .expect_err("sys_column_consistency key eventual should fail");
        assert!(
            err.to_string().contains("COLUMN_CONSISTENCY_INVALID"),
            "expected COLUMN_CONSISTENCY_INVALID, got {err}"
        );
    }
}
