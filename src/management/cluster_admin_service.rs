use crate::pb::cluster_admin_server::ClusterAdmin;
use crate::pb::{AddHubRequest, AddHubResponse, NodeCompatibility};
use crate::management::cluster_admin_domain::{
    AddHubError, CompatibilityChecker, StrictCompatibilityChecker, TopologyService,
    add_hub_error_response, default_compatibility as domain_default_compatibility,
    global_topology_service, not_leader_response,
};
use crate::raft::network::RaftRouter;
use crate::raft::raft_node::RaftNode;
use anyhow::{Result, anyhow};
use openraft::{BasicNode, ServerState};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub use crate::management::cluster_admin_domain::ClusterBaseline;

struct ClusterState {
    nodes: HashMap<u64, Arc<RaftNode>>,
    voters: BTreeSet<u64>,
    request_cache: HashMap<String, AddHubResponse>,
    membership_version: u64,
    bootstrapped: bool,
}

impl ClusterState {
    fn new(local_node_id: u64, local_node: Arc<RaftNode>) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(local_node_id, local_node);
        let mut voters = BTreeSet::new();
        voters.insert(local_node_id);
        Self {
            nodes,
            voters,
            request_cache: HashMap::new(),
            membership_version: 1,
            bootstrapped: false,
        }
    }
}

#[derive(Clone)]
pub struct ClusterNodeManager {
    local_node_id: u64,
    leader_node: Arc<RaftNode>,
    raft_router: RaftRouter,
    compatibility_checker: Arc<dyn CompatibilityChecker>,
    topology_service: Arc<dyn TopologyService>,
    state: Arc<Mutex<ClusterState>>,
}

impl ClusterNodeManager {
    pub fn new(
        local_node_id: u64,
        local_grpc_addr: String,
        leader_node: Arc<RaftNode>,
        raft_router: RaftRouter,
        baseline: ClusterBaseline,
    ) -> Self {
        let compatibility_checker = Arc::new(StrictCompatibilityChecker::new(baseline));
        let topology_service = global_topology_service();
        Self::new_with_services(
            local_node_id,
            local_grpc_addr,
            leader_node,
            raft_router,
            compatibility_checker,
            topology_service,
        )
    }

    pub fn new_with_topology(
        local_node_id: u64,
        local_grpc_addr: String,
        leader_node: Arc<RaftNode>,
        raft_router: RaftRouter,
        baseline: ClusterBaseline,
        topology_service: Arc<dyn TopologyService>,
    ) -> Self {
        let compatibility_checker = Arc::new(StrictCompatibilityChecker::new(baseline));
        Self::new_with_services(
            local_node_id,
            local_grpc_addr,
            leader_node,
            raft_router,
            compatibility_checker,
            topology_service,
        )
    }

    pub fn new_with_checker(
        local_node_id: u64,
        local_grpc_addr: String,
        leader_node: Arc<RaftNode>,
        raft_router: RaftRouter,
        compatibility_checker: Arc<dyn CompatibilityChecker>,
    ) -> Self {
        let topology_service = global_topology_service();
        Self::new_with_services(
            local_node_id,
            local_grpc_addr,
            leader_node,
            raft_router,
            compatibility_checker,
            topology_service,
        )
    }

    pub fn new_with_services(
        local_node_id: u64,
        local_grpc_addr: String,
        leader_node: Arc<RaftNode>,
        raft_router: RaftRouter,
        compatibility_checker: Arc<dyn CompatibilityChecker>,
        topology_service: Arc<dyn TopologyService>,
    ) -> Self {
        topology_service.upsert_grpc_addr(local_node_id, local_grpc_addr);
        let state = ClusterState::new(local_node_id, leader_node.clone());
        Self {
            local_node_id,
            leader_node,
            raft_router,
            compatibility_checker,
            topology_service,
            state: Arc::new(Mutex::new(state)),
        }
    }

    async fn ensure_bootstrapped(&self) -> Result<()> {
        {
            let state = self.state.lock().await;
            if state.bootstrapped {
                return Ok(());
            }
        }

        let current_leader = self.current_leader_id().await?;
        if current_leader == Some(self.local_node_id) {
            let mut state = self.state.lock().await;
            state.bootstrapped = true;
            return Ok(());
        }

        let mut members = BTreeSet::new();
        members.insert(self.local_node_id);
        match self.leader_node.raft.initialize(members).await {
            Ok(_) => {}
            Err(err) => {
                let msg = err.to_string();
                if !msg.contains("already initialized") {
                    return Err(anyhow!(msg));
                }
            }
        }

        self.wait_for_leader().await?;
        let mut state = self.state.lock().await;
        state.bootstrapped = true;
        Ok(())
    }

    async fn wait_for_leader(&self) -> Result<u64> {
        for _ in 0..100 {
            if let Some(leader_id) = self.current_leader_id().await? {
                return Ok(leader_id);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!("leader not elected"))
    }

    async fn current_leader_id(&self) -> Result<Option<u64>> {
        let nodes = {
            let state = self.state.lock().await;
            state.nodes.values().cloned().collect::<Vec<_>>()
        };

        for node in nodes {
            let metrics = node.raft.metrics().borrow().clone();
            if metrics.state == ServerState::Leader
                && matches!(metrics.current_leader, Some(id) if id == node.node_id())
            {
                return Ok(Some(node.node_id()));
            }
        }
        Ok(None)
    }

    async fn ensure_leader_or_redirect(&self) -> Result<Option<String>> {
        let leader_id = self.wait_for_leader().await?;
        if leader_id == self.local_node_id {
            return Ok(None);
        }
        Ok(Some(self.resolve_leader_hint(leader_id).await))
    }

    async fn precheck_add(
        &self,
        req: &AddHubRequest,
    ) -> std::result::Result<Option<AddHubResponse>, AddHubError> {
        if req.node_id == 0 {
            return Err(AddHubError::invalid_argument(
                "node_id must be > 0",
                "set node_id to a non-zero unique value",
            ));
        }
        if req.request_id.trim().is_empty() {
            return Err(AddHubError::invalid_argument(
                "request_id is required",
                "set request_id to a stable idempotency key",
            ));
        }
        if req.raft_addr.trim().is_empty() {
            return Err(AddHubError::invalid_argument(
                "raft_addr is required",
                "set raft_addr to a reachable raft endpoint",
            ));
        }
        if req.grpc_addr.trim().is_empty() {
            return Err(AddHubError::invalid_argument(
                "grpc_addr is required",
                "set grpc_addr to a reachable grpc endpoint",
            ));
        }

        let state = self.state.lock().await;
        if let Some(cached) = state.request_cache.get(&req.request_id) {
            return Ok(Some(cached.clone()));
        }
        if state.nodes.contains_key(&req.node_id) {
            return Err(AddHubError::already_exists(
                "node_id already exists",
                "use a different node_id or retry with original request_id",
            ));
        }
        Ok(None)
    }

    async fn resolve_leader_hint(&self, leader_id: u64) -> String {
        self.topology_service
            .get_grpc_addr(leader_id)
            .unwrap_or_else(|| format!("node://{}", leader_id))
    }

    pub async fn add_hub(
        &self,
        req: AddHubRequest,
    ) -> std::result::Result<AddHubResponse, AddHubError> {
        self.ensure_bootstrapped().await.map_err(|e| {
            AddHubError::invalid_argument(
                format!("cluster bootstrap failed: {}", e),
                "verify local node state and raft bootstrap condition",
            )
        })?;

        if let Some(existing) = self.precheck_add(&req).await? {
            return Ok(existing);
        }
        self.compatibility_checker.check(&req)?;

        let node_base_dir =
            std::env::temp_dir().join(format!("hub_dynamic_node_{}_{}", req.node_id, Uuid::new_v4()));
        let node = RaftNode::start(req.node_id, node_base_dir, self.raft_router.clone())
            .await
            .map_err(|e| {
                AddHubError::invalid_argument(
                    format!("failed to start node: {}", e),
                    "check node runtime and storage path permissions",
                )
            })?;
        self.raft_router.register(req.node_id, Arc::new(node.clone()));

        self.leader_node
            .raft
            .add_learner(req.node_id, BasicNode::new(req.raft_addr.clone()), true)
            .await
            .map_err(|e| {
                AddHubError::invalid_argument(
                    format!("add_learner failed: {}", e),
                    "verify raft_addr reachability and node_id uniqueness in raft membership",
                )
            })?;

        let promote_voters = if req.auto_promote {
            let mut voters = {
                let state = self.state.lock().await;
                state.voters.clone()
            };
            voters.insert(req.node_id);
            Some(voters)
        } else {
            None
        };

        if let Some(voters) = promote_voters.clone() {
            self.leader_node
                .raft
                .change_membership(voters, true)
                .await
                .map_err(|e| {
                    AddHubError::invalid_argument(
                        format!("change_membership failed: {}", e),
                        "retry after cluster stabilizes or set auto_promote=false first",
                    )
                })?;
        }

        let mut state = self.state.lock().await;
        state.nodes.insert(req.node_id, Arc::new(node));
        self.topology_service
            .upsert_grpc_addr(req.node_id, req.grpc_addr);
        if let Some(voters) = promote_voters {
            state.voters = voters;
        }
        state.membership_version = state.membership_version.saturating_add(1);
        if req.auto_promote {
            state.membership_version = state.membership_version.saturating_add(1);
        }
        let resp = AddHubResponse {
            membership_version: state.membership_version,
            leader_hint: String::new(),
            reason_code: String::new(),
            suggested_action: String::new(),
        };
        state.request_cache.insert(req.request_id, resp.clone());
        Ok(resp)
    }

    pub async fn node_count(&self) -> usize {
        let state = self.state.lock().await;
        state.nodes.len()
    }

    pub async fn register_known_grpc_addr(&self, node_id: u64, grpc_addr: String) {
        self.topology_service.upsert_grpc_addr(node_id, grpc_addr);
    }
}

#[derive(Clone)]
pub struct ClusterAdminService {
    manager: Arc<ClusterNodeManager>,
}

impl ClusterAdminService {
    pub fn new(manager: Arc<ClusterNodeManager>) -> Self {
        Self { manager }
    }
}

#[tonic::async_trait]
impl ClusterAdmin for ClusterAdminService {
    async fn add_hub(
        &self,
        request: Request<AddHubRequest>,
    ) -> Result<Response<AddHubResponse>, Status> {
        if let Some(leader_hint) = self
            .manager
            .ensure_leader_or_redirect()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?
        {
            return Ok(Response::new(not_leader_response(leader_hint)));
        }

        let req = request.into_inner();
        let resp = match self.manager.add_hub(req).await {
            Ok(resp) => resp,
            Err(err) => return Ok(Response::new(add_hub_error_response(err))),
        };
        Ok(Response::new(resp))
    }
}

pub fn default_compatibility(app_semver: String) -> NodeCompatibility {
    domain_default_compatibility(app_semver)
}
