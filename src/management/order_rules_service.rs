use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

use crate::hub::order_rules::OrderingRules;
use crate::pb::order_rules_admin_server::OrderRulesAdmin;
use crate::pb::{
    GetRulesVersionRequest, GetRulesVersionResponse, UpdateRulesRequest, UpdateRulesResponse,
};

// ### 修改记录 (2026-03-01)
// - 原因: 需要共享顺序规则的内存存储
// - 目的: 支持热更新时原子替换规则集
// - 备注: 规则采用 Arc<RwLock<Arc<...>>> 以减少读路径拷贝
pub struct OrderRulesStore {
    rules: RwLock<Arc<OrderingRules>>,
    version: AtomicU64,
}

// ### 修改记录 (2026-03-01)
// - 原因: clippy 提示 new_without_default
// - 目的: 提供一致的默认构造入口
impl Default for OrderRulesStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderRulesStore {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要在启动时初始化规则存储
    // - 目的: 为管理接口提供可用的初始状态
    // - 备注: 默认版本从 0 开始递增
    pub fn new() -> Self {
        Self {
            rules: RwLock::new(Arc::new(OrderingRules::default())),
            version: AtomicU64::new(0),
        }
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要对外提供规则快照
    // - 目的: 让调用方获取当前规则用于匹配
    // - 备注: 返回 Arc 避免深拷贝
    pub async fn snapshot(&self) -> Arc<OrderingRules> {
        self.rules.read().await.clone()
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要从 JSON 更新规则
    // - 目的: 支持管理接口动态下发
    // - 备注: 版本号在成功更新后递增
    pub async fn update_from_json(&self, raw: &str) -> Result<u64> {
        let rules = OrderingRules::from_json(raw)?;
        let mut guard = self.rules.write().await;
        *guard = Arc::new(rules);
        let version = self
            .version
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        Ok(version)
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要提供当前规则版本
    // - 目的: 便于管理侧校验热更新状态
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要对外提供顺序规则管理服务
// - 目的: 支持规则热更新与版本查询
// - 备注: 服务逻辑仅维护规则，不参与业务执行
#[derive(Clone)]
pub struct OrderRulesAdminService {
    store: Arc<OrderRulesStore>,
}

impl OrderRulesAdminService {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要注入共享规则存储
    // - 目的: 支持管理服务多实例共享状态
    pub fn new(store: Arc<OrderRulesStore>) -> Self {
        Self { store }
    }
}

#[tonic::async_trait]
impl OrderRulesAdmin for OrderRulesAdminService {
    // ### 修改记录 (2026-03-01)
    // - 原因: 需要处理规则热更新请求
    // - 目的: 将规则 JSON 替换为新版本
    // - 备注: 解析失败返回 invalid_argument
    async fn update_rules(
        &self,
        request: Request<UpdateRulesRequest>,
    ) -> Result<Response<UpdateRulesResponse>, Status> {
        let req = request.into_inner();
        let version = self
            .store
            .update_from_json(&req.rules_json)
            .await
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        Ok(Response::new(UpdateRulesResponse { version }))
    }

    // ### 修改记录 (2026-03-01)
    // - 原因: 需要提供规则版本查询
    // - 目的: 便于管理侧确认当前生效版本
    async fn get_rules_version(
        &self,
        _request: Request<GetRulesVersionRequest>,
    ) -> Result<Response<GetRulesVersionResponse>, Status> {
        Ok(Response::new(GetRulesVersionResponse {
            version: self.store.version(),
        }))
    }
}
