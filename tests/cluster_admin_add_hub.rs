use check_program::management::cluster_admin_domain::MemoryTopologyService;
use check_program::management::cluster_admin_service::{
    ClusterAdminService, ClusterBaseline, ClusterNodeManager, default_compatibility,
};
use check_program::pb::cluster_admin_server::ClusterAdmin;
use check_program::pb::{AddHubRequest, NodeCompatibility};
use check_program::raft::network::RaftRouter;
use check_program::raft::raft_node::RaftNode;
use std::sync::Arc;
use tonic::Request;
use uuid::Uuid;

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
    let service = ClusterAdminService::new(manager);
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
    let service = ClusterAdminService::new(manager);

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
