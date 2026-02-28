use check_program::raft::raft_node::TestCluster;
use openraft::ServerState;
use std::time::Duration;

#[tokio::test]
async fn test_raft_cluster_bootstrap_and_elect_leader() {
    // 1. Start a 3-node cluster
    let cluster = TestCluster::new(3).await.expect("Failed to start cluster");

    // 2. Wait for leader election
    let mut leader_id = None;
    for _ in 0..100 {
        for node in &cluster.nodes {
            let metrics = node.raft.metrics().borrow().clone();
            if metrics.state == ServerState::Leader {
                leader_id = Some(node.node_id());
                break;
            }
        }
        if leader_id.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        leader_id.is_some(),
        "Leader should be elected within timeout"
    );
    let leader_id = leader_id.unwrap();
    println!("Leader elected: {}", leader_id);

    // 3. Verify other nodes are followers
    for node in &cluster.nodes {
        if node.node_id() != leader_id {
            let metrics = node.raft.metrics().borrow().clone();
            assert_eq!(
                metrics.state,
                ServerState::Follower,
                "Node {} should be follower",
                node.node_id()
            );
        }
    }

    // 4. Try to write to leader (if supported)
    // We can't easily write yet because client_write is not fully exposed/tested in TestCluster
    // But we can check if leader has committed initial membership
    // The leader should have committed at least one log entry (initial membership)

    let leader_node = cluster.get_node(leader_id).unwrap();
    let metrics = leader_node.raft.metrics().borrow().clone();
    assert!(
        metrics.last_log_index.unwrap_or(0) >= 1,
        "Leader should have committed at least one log"
    );

    // 5. Cleanup (optional, temp dirs are used)
}
