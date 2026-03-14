use check_program::management::cluster_admin_domain::MemoryTopologyService;
use check_program::management::cluster_admin_service::{
    ClusterAdminService, ClusterBaseline, ClusterNodeManager, DrainSignal,
    EdgeMetadataError, EdgeMetadataProvider, default_compatibility,
};
use check_program::pb::cluster_admin_server::ClusterAdmin;
use check_program::pb::{AddEdgeRequest, AddHubRequest, NodeCompatibility, RemoveHubRequest};
use check_program::raft::network::RaftRouter;
use check_program::raft::raft_node::RaftNode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use tonic::Request;
use uuid::Uuid;

struct MockDrainSignal {
    draining: AtomicBool,
    inflight: AtomicU64,
}

impl MockDrainSignal {
    fn new(inflight: u64) -> Self {
        Self {
            draining: AtomicBool::new(false),
            inflight: AtomicU64::new(inflight),
        }
    }

    fn set_inflight(&self, value: u64) {
        self.inflight.store(value, Ordering::Relaxed);
    }
}

impl DrainSignal for MockDrainSignal {
    fn set_reject_new(&self, reject_new: bool) {
        self.draining.store(reject_new, Ordering::Relaxed);
    }

    fn inflight_requests(&self) -> u64 {
        self.inflight.load(Ordering::Relaxed)
    }
}

struct MockEdgeMetadataProvider {
    pull_count: AtomicUsize,
    fail_kind: Option<&'static str>,
}

impl MockEdgeMetadataProvider {
    fn success() -> Self {
        Self {
            pull_count: AtomicUsize::new(0),
            fail_kind: None,
        }
    }

    fn with_fail_kind(fail_kind: &'static str) -> Self {
        Self {
            pull_count: AtomicUsize::new(0),
            fail_kind: Some(fail_kind),
        }
    }

    fn pull_count(&self) -> usize {
        self.pull_count.load(Ordering::Relaxed)
    }
}

#[tonic::async_trait]
impl EdgeMetadataProvider for MockEdgeMetadataProvider {
    async fn pull_group_metadata(
        &self,
        group_id: &str,
    ) -> std::result::Result<(), EdgeMetadataError> {
        self.pull_count.fetch_add(1, Ordering::Relaxed);
        match self.fail_kind {
            Some("TARGET_METADATA_MISSING") => Err(EdgeMetadataError::target_metadata_missing(
                format!("group metadata table missing for group_id={group_id}"),
            )),
            Some("TARGET_NOT_FOUND") => Err(EdgeMetadataError::target_not_found(format!(
                "group_id not found: {group_id}"
            ))),
            _ => Ok(()),
        }
    }
}

#[tokio::test]
async fn add_hub_idempotent_with_same_request_id() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(format!("cluster_admin_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let manager = ClusterNodeManager::new_with_topology(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    );

    let request = AddHubRequest {
        node_id: 2,
        raft_addr: "127.0.0.1:31002".to_string(),
        grpc_addr: "127.0.0.1:32002".to_string(),
        auto_promote: false,
        request_id: "rid-1".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };
    let first = manager.add_hub(request.clone()).await.unwrap();
    let second = manager.add_hub(request).await.unwrap();

    assert_eq!(first.membership_version, second.membership_version);
    assert_eq!(manager.node_count().await, 2);
    let events = manager.audit_events_snapshot().await;
    assert!(
        events
            .iter()
            .any(|event| event.action == "cluster.add_hub" && event.status == "ok")
    );
}

#[tokio::test]
async fn add_hub_reject_when_sqlite_schema_mismatch() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(format!("cluster_admin_schema_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    let service = ClusterAdminService::new(manager.clone());
    let req = AddHubRequest {
        node_id: 3,
        raft_addr: "127.0.0.1:31003".to_string(),
        grpc_addr: "127.0.0.1:32003".to_string(),
        auto_promote: false,
        request_id: "rid-schema-mismatch".to_string(),
        compatibility: Some(NodeCompatibility {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 2,
            sled_format_version: 1,
            log_codec_version: 1,
        }),
    };

    let resp = service.add_hub(Request::new(req)).await.unwrap().into_inner();
    assert_eq!(resp.membership_version, 0);
    assert_eq!(resp.reason_code, "INCOMPATIBLE_SQLITE_SCHEMA");
    assert!(!resp.suggested_action.is_empty());
}

#[tokio::test]
async fn add_hub_on_follower_returns_real_leader_hint() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(format!("cluster_admin_redirect_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        2,
        "http://127.0.0.1:50052".to_string(),
        Arc::new(leader.clone()),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    manager
        .register_known_grpc_addr(1, "127.0.0.1:50051".to_string())
        .await;
    let service = ClusterAdminService::new(manager.clone());

    let req = AddHubRequest {
        node_id: 4,
        raft_addr: "127.0.0.1:31004".to_string(),
        grpc_addr: "127.0.0.1:32004".to_string(),
        auto_promote: false,
        request_id: "rid-redirect".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };
    let resp = service.add_hub(Request::new(req)).await.unwrap().into_inner();
    assert_eq!(resp.reason_code, "NOT_LEADER");
    assert_eq!(resp.leader_hint, "http://127.0.0.1:50051");
    let events = manager.audit_events_snapshot().await;
    assert!(
        events
            .iter()
            .any(|event| event.action == "cluster.redirect" && event.status == "ok")
    );
}

#[tokio::test]
async fn add_hub_on_follower_fallback_then_resolve_real_leader_hint() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir =
        std::env::temp_dir().join(format!("cluster_admin_redirect_fallback_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        2,
        "http://127.0.0.1:50052".to_string(),
        Arc::new(leader.clone()),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    let service = ClusterAdminService::new(manager.clone());

    let first_req = AddHubRequest {
        node_id: 5,
        raft_addr: "127.0.0.1:31005".to_string(),
        grpc_addr: "127.0.0.1:32005".to_string(),
        auto_promote: false,
        request_id: "rid-redirect-fallback-1".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };
    let first_resp = service
        .add_hub(Request::new(first_req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first_resp.reason_code, "NOT_LEADER");
    assert_eq!(first_resp.leader_hint, "node://1");

    manager
        .register_known_grpc_addr(1, "127.0.0.1:50051".to_string())
        .await;

    let second_req = AddHubRequest {
        node_id: 6,
        raft_addr: "127.0.0.1:31006".to_string(),
        grpc_addr: "127.0.0.1:32006".to_string(),
        auto_promote: false,
        request_id: "rid-redirect-fallback-2".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };
    let second_resp = service
        .add_hub(Request::new(second_req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(second_resp.reason_code, "NOT_LEADER");
    assert_eq!(second_resp.leader_hint, "http://127.0.0.1:50051");
}

// ### ????
// - 2026-03-14: ??: ??????????????
// - 2026-03-14: ??: ?? add_hub ?? leader ??? NOT_LEADER + leader_hint
// - 2026-03-15: ??: ?? stash ? remove_hub/add_edge ??
// - 2026-03-15: ??: ????????????/????
#[tokio::test]
async fn add_hub_redirects_when_leader_not_elected() {
    // ### ???? (2026-03-14)
    // - ??: ??????????????
    // - ??: ???????
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir =
        std::env::temp_dir().join(format!("cluster_admin_no_leader_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));

    // ### ???? (2026-03-14)
    // - ??: local_node_id ? leader_node ????? follower ??
    // - ??: ??? leader ????????
    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        2,
        "http://127.0.0.1:50052".to_string(),
        Arc::new(leader.clone()),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    let service = ClusterAdminService::new(manager);

    // ### ???? (2026-03-14)
    // - ??: ???? add_hub ??
    // - ??: ?? ensure_leader_or_redirect
    let req = AddHubRequest {
        node_id: 7,
        raft_addr: "127.0.0.1:31007".to_string(),
        grpc_addr: "127.0.0.1:32007".to_string(),
        auto_promote: false,
        request_id: "rid-no-leader".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };

    let resp = service.add_hub(Request::new(req)).await.unwrap().into_inner();
    assert_eq!(resp.reason_code, "NOT_LEADER");
    assert!(!resp.leader_hint.is_empty());
}

// ### ????
// - 2026-03-15: ??: ? stash ?? remove_hub ??
// - 2026-03-15: ??: ????????????????
#[tokio::test]
async fn remove_hub_requires_mark_before_commit() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(format!("cluster_admin_remove_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    let service = ClusterAdminService::new(manager.clone());

    let add_req = AddHubRequest {
        node_id: 2,
        raft_addr: "127.0.0.1:31002".to_string(),
        grpc_addr: "127.0.0.1:32002".to_string(),
        auto_promote: true,
        request_id: "rid-remove-add".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };
    let _ = service.add_hub(Request::new(add_req)).await.unwrap();

    let commit_without_mark = RemoveHubRequest {
        node_id: 2,
        request_id: "rid-remove-commit-1".to_string(),
        phase: "COMMIT".to_string(),
        force: false,
        drain_timeout_ms: 1000,
    };
    let commit_resp = service
        .remove_hub(Request::new(commit_without_mark))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(commit_resp.reason_code, "NOT_DRAINING");

    let mark_req = RemoveHubRequest {
        node_id: 2,
        request_id: "rid-remove-mark-1".to_string(),
        phase: "MARK_DRAINING".to_string(),
        force: false,
        drain_timeout_ms: 1000,
    };
    let mark_resp = service
        .remove_hub(Request::new(mark_req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(mark_resp.reason_code, "DRAINING_MARKED");

    let commit_req = RemoveHubRequest {
        node_id: 2,
        request_id: "rid-remove-commit-2".to_string(),
        phase: "COMMIT".to_string(),
        force: false,
        drain_timeout_ms: 1000,
    };
    let ok_resp = service
        .remove_hub(Request::new(commit_req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ok_resp.reason_code, "");
    assert_eq!(manager.node_count().await, 1);
}

// ### ???? (2026-03-15)
// - ??: ? stash ?? remove_hub ???????
// - ??: ??????????? leader
#[tokio::test]
async fn remove_hub_rejects_commit_when_target_is_current_leader() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir =
        std::env::temp_dir().join(format!("cluster_admin_remove_leader_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    let service = ClusterAdminService::new(manager.clone());

    let mark_req = RemoveHubRequest {
        node_id: 1,
        request_id: "rid-remove-self-mark".to_string(),
        phase: "MARK_DRAINING".to_string(),
        force: false,
        drain_timeout_ms: 1000,
    };
    let _ = service.remove_hub(Request::new(mark_req)).await.unwrap();

    let commit_req = RemoveHubRequest {
        node_id: 1,
        request_id: "rid-remove-self-commit".to_string(),
        phase: "COMMIT".to_string(),
        force: false,
        drain_timeout_ms: 1000,
    };
    let resp = service
        .remove_hub(Request::new(commit_req))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.reason_code, "LEADER_TRANSFER_TIMEOUT");
    assert_eq!(manager.node_count().await, 1);
}

// ### ???? (2026-03-15)
// - ??: ? stash ?? remove_hub ??????
// - ??: ?? leader ???????
#[tokio::test]
async fn remove_hub_commit_auto_transfers_leader_and_succeeds() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(
        format!("cluster_admin_remove_auto_transfer_{}", Uuid::new_v4()),
    );
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    let service = ClusterAdminService::new(manager.clone());

    let add_req = AddHubRequest {
        node_id: 2,
        raft_addr: "127.0.0.1:31002".to_string(),
        grpc_addr: "127.0.0.1:32002".to_string(),
        auto_promote: true,
        request_id: "rid-auto-transfer-add".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };
    let _ = service.add_hub(Request::new(add_req)).await.unwrap();

    let node1_signal = Arc::new(MockDrainSignal::new(0));
    manager
        .register_drain_signal(1, node1_signal as Arc<dyn DrainSignal>)
        .await;

    let mark_req = RemoveHubRequest {
        node_id: 1,
        request_id: "rid-auto-transfer-mark".to_string(),
        phase: "MARK_DRAINING".to_string(),
        force: false,
        drain_timeout_ms: 1000,
    };
    let _ = service.remove_hub(Request::new(mark_req)).await.unwrap();

    let commit_req = RemoveHubRequest {
        node_id: 1,
        request_id: "rid-auto-transfer-commit".to_string(),
        phase: "COMMIT".to_string(),
        force: false,
        drain_timeout_ms: 5000,
    };
    let resp = service
        .remove_hub(Request::new(commit_req))
        .await
        .unwrap()
        .into_inner();
    let final_resp = if resp.reason_code == "LEADER_TRANSFER_TIMEOUT" {
        let retry_commit = RemoveHubRequest {
            node_id: 1,
            request_id: "rid-auto-transfer-commit-retry".to_string(),
            phase: "COMMIT".to_string(),
            force: false,
            drain_timeout_ms: 5000,
        };
        service
            .remove_hub(Request::new(retry_commit))
            .await
            .unwrap()
            .into_inner()
    } else {
        resp
    };
    assert_eq!(final_resp.reason_code, "");
    assert_eq!(final_resp.status, "REMOVED");
    assert_eq!(manager.node_count().await, 1);
}

// ### ???? (2026-03-15)
// - ??: ? stash ?? remove_hub ????
// - ??: ?? inflight ????? DRAIN_TIMEOUT
#[tokio::test]
async fn remove_hub_commit_times_out_when_inflight_not_drained() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(
        format!("cluster_admin_remove_drain_timeout_{}", Uuid::new_v4()),
    );
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    let manager = Arc::new(ClusterNodeManager::new_with_topology(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router.clone(),
        ClusterBaseline {
            app_semver: env!("CARGO_PKG_VERSION").to_string(),
            sqlite_schema_version: 1,
            sled_format_version: 1,
            log_codec_version: 1,
        },
        topology,
    ));
    let service = ClusterAdminService::new(manager.clone());

    let add_req = AddHubRequest {
        node_id: 2,
        raft_addr: "127.0.0.1:31002".to_string(),
        grpc_addr: "127.0.0.1:32002".to_string(),
        auto_promote: true,
        request_id: "rid-drain-timeout-add".to_string(),
        compatibility: Some(default_compatibility(env!("CARGO_PKG_VERSION").to_string())),
    };
    let _ = service.add_hub(Request::new(add_req)).await.unwrap();

    let node2_signal = Arc::new(MockDrainSignal::new(5));
    manager
        .register_drain_signal(2, node2_signal.clone() as Arc<dyn DrainSignal>)
        .await;

    let mark_req = RemoveHubRequest {
        node_id: 2,
        request_id: "rid-drain-timeout-mark".to_string(),
        phase: "MARK_DRAINING".to_string(),
        force: false,
        drain_timeout_ms: 1000,
    };
    let _ = service.remove_hub(Request::new(mark_req)).await.unwrap();

    let timeout_commit = RemoveHubRequest {
        node_id: 2,
        request_id: "rid-drain-timeout-commit1".to_string(),
        phase: "COMMIT".to_string(),
        force: false,
        drain_timeout_ms: 20,
    };
    let timeout_resp = service
        .remove_hub(Request::new(timeout_commit))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(timeout_resp.reason_code, "DRAIN_TIMEOUT");
    assert_eq!(manager.node_count().await, 2);

    node2_signal.set_inflight(0);
    let success_commit = RemoveHubRequest {
        node_id: 2,
        request_id: "rid-drain-timeout-commit2".to_string(),
        phase: "COMMIT".to_string(),
        force: false,
        drain_timeout_ms: 2000,
    };
    let success_resp = service
        .remove_hub(Request::new(success_commit))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(success_resp.reason_code, "");
    assert_eq!(success_resp.status, "REMOVED");
    assert_eq!(manager.node_count().await, 1);
}

// ### ????
// - 2026-03-15: ??: ? stash ?? add_edge ??
// - 2026-03-15: ??: ?? standalone/?????????
#[tokio::test]
async fn addedge_standalone_does_not_pull_group_metadata() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(format!("cluster_admin_addedge_a_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let provider = Arc::new(MockEdgeMetadataProvider::success());
    let manager = ClusterNodeManager::new_with_services_and_edge_metadata(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router,
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology,
        provider.clone(),
    );

    let resp = manager
        .add_edge(AddEdgeRequest {
            edge_id: "edge-standalone-1".to_string(),
            group_id: "".to_string(),
            request_id: "rid-addedge-standalone-1".to_string(),
        })
        .await;

    assert!(resp.ready);
    assert_eq!(resp.reason_code, "");
    assert_eq!(provider.pull_count(), 0);
    let events = manager.audit_events_snapshot().await;
    assert!(!events.iter().any(|event| event.action == "cluster.add_edge.group_metadata_pull"));
}

#[tokio::test]
async fn addedge_with_group_id_pulls_group_metadata_before_ready() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir = std::env::temp_dir().join(format!("cluster_admin_addedge_b_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let provider = Arc::new(MockEdgeMetadataProvider::success());
    let manager = ClusterNodeManager::new_with_services_and_edge_metadata(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router,
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology,
        provider.clone(),
    );

    let resp = manager
        .add_edge(AddEdgeRequest {
            edge_id: "edge-grouped-1".to_string(),
            group_id: "group-42".to_string(),
            request_id: "rid-addedge-grouped-1".to_string(),
        })
        .await;

    assert!(resp.ready);
    assert_eq!(resp.reason_code, "");
    assert_eq!(provider.pull_count(), 1);
    let events = manager.audit_events_snapshot().await;
    let pull_idx = events
        .iter()
        .position(|event| event.action == "cluster.add_edge.group_metadata_pull")
        .unwrap();
    let ready_idx = events
        .iter()
        .position(|event| event.action == "cluster.add_edge.ready")
        .unwrap();
    assert!(
        pull_idx < ready_idx,
        "group metadata pull should happen before ready state"
    );
}

#[tokio::test]
async fn addedge_with_group_id_returns_target_metadata_missing_when_group_metadata_absent() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir =
        std::env::temp_dir().join(format!("cluster_admin_addedge_b_missing_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let provider = Arc::new(MockEdgeMetadataProvider::with_fail_kind(
        "TARGET_METADATA_MISSING",
    ));
    let manager = ClusterNodeManager::new_with_services_and_edge_metadata(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router,
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology,
        provider.clone(),
    );

    let resp = manager
        .add_edge(AddEdgeRequest {
            edge_id: "edge-grouped-missing".to_string(),
            group_id: "group-missing".to_string(),
            request_id: "rid-addedge-grouped-missing".to_string(),
        })
        .await;

    assert!(!resp.ready);
    assert_eq!(resp.reason_code, "TARGET_METADATA_MISSING");
    assert_eq!(provider.pull_count(), 1);
}

#[tokio::test]
async fn addedge_with_group_id_returns_target_not_found_when_group_absent() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir =
        std::env::temp_dir().join(format!("cluster_admin_addedge_b_notfound_{}", Uuid::new_v4()));
    let leader = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader.clone()));
    let provider = Arc::new(MockEdgeMetadataProvider::with_fail_kind("TARGET_NOT_FOUND"));
    let manager = ClusterNodeManager::new_with_services_and_edge_metadata(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader),
        router,
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology,
        provider.clone(),
    );

    let resp = manager
        .add_edge(AddEdgeRequest {
            edge_id: "edge-grouped-notfound".to_string(),
            group_id: "group-notfound".to_string(),
            request_id: "rid-addedge-grouped-notfound".to_string(),
        })
        .await;

    assert!(!resp.ready);
    assert_eq!(resp.reason_code, "TARGET_NOT_FOUND");
    assert_eq!(provider.pull_count(), 1);
}

#[tokio::test]
async fn addedge_on_follower_forwards_and_preserves_target_metadata_missing() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir =
        std::env::temp_dir().join(format!("cluster_admin_addedge_forward_missing_{}", Uuid::new_v4()));
    let leader_node = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader_node.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader_node.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader_node.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    let leader_provider = Arc::new(MockEdgeMetadataProvider::with_fail_kind(
        "TARGET_METADATA_MISSING",
    ));
    let follower_provider = Arc::new(MockEdgeMetadataProvider::success());
    let leader_manager = Arc::new(ClusterNodeManager::new_with_services_and_edge_metadata(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader_node.clone()),
        router.clone(),
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology.clone(),
        leader_provider.clone(),
    ));
    let follower_manager = Arc::new(ClusterNodeManager::new_with_services_and_edge_metadata(
        2,
        "http://127.0.0.1:50052".to_string(),
        Arc::new(leader_node),
        router,
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology,
        follower_provider.clone(),
    ));
    follower_manager
        .register_edge_forward_target("127.0.0.1:50051".to_string(), leader_manager)
        .await;

    let resp = follower_manager
        .add_edge(AddEdgeRequest {
            edge_id: "edge-forward-missing".to_string(),
            group_id: "group-forward-missing".to_string(),
            request_id: "rid-addedge-forward-missing".to_string(),
        })
        .await;

    assert!(!resp.ready);
    assert_eq!(resp.reason_code, "TARGET_METADATA_MISSING");
    assert_eq!(leader_provider.pull_count(), 1);
    assert_eq!(follower_provider.pull_count(), 0);
}

#[tokio::test]
async fn addedge_on_follower_forwards_and_preserves_target_not_found() {
    let topology = Arc::new(MemoryTopologyService::default());
    let router = RaftRouter::new();
    let base_dir =
        std::env::temp_dir().join(format!("cluster_admin_addedge_forward_notfound_{}", Uuid::new_v4()));
    let leader_node = RaftNode::start(1, base_dir, router.clone()).await.unwrap();
    router.register(1, Arc::new(leader_node.clone()));
    let mut members = std::collections::BTreeSet::new();
    members.insert(1);
    leader_node.raft.initialize(members).await.unwrap();
    for _ in 0..50 {
        if leader_node.raft.metrics().borrow().state == openraft::ServerState::Leader {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    let leader_provider = Arc::new(MockEdgeMetadataProvider::with_fail_kind("TARGET_NOT_FOUND"));
    let follower_provider = Arc::new(MockEdgeMetadataProvider::success());
    let leader_manager = Arc::new(ClusterNodeManager::new_with_services_and_edge_metadata(
        1,
        "http://127.0.0.1:50051".to_string(),
        Arc::new(leader_node.clone()),
        router.clone(),
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology.clone(),
        leader_provider.clone(),
    ));
    let follower_manager = Arc::new(ClusterNodeManager::new_with_services_and_edge_metadata(
        2,
        "http://127.0.0.1:50052".to_string(),
        Arc::new(leader_node),
        router,
        Arc::new(
            check_program::management::cluster_admin_domain::StrictCompatibilityChecker::new(
                ClusterBaseline {
                    app_semver: env!("CARGO_PKG_VERSION").to_string(),
                    sqlite_schema_version: 1,
                    sled_format_version: 1,
                    log_codec_version: 1,
                },
            ),
        ),
        topology,
        follower_provider.clone(),
    ));
    follower_manager
        .register_edge_forward_target("127.0.0.1:50051".to_string(), leader_manager)
        .await;

    let resp = follower_manager
        .add_edge(AddEdgeRequest {
            edge_id: "edge-forward-notfound".to_string(),
            group_id: "group-forward-notfound".to_string(),
            request_id: "rid-addedge-forward-notfound".to_string(),
        })
        .await;

    assert!(!resp.ready);
    assert_eq!(resp.reason_code, "TARGET_NOT_FOUND");
    assert_eq!(leader_provider.pull_count(), 1);
    assert_eq!(follower_provider.pull_count(), 0);
}
