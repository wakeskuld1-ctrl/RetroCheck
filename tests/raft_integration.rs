use check_program::raft::network::RaftRouter;
use check_program::raft::raft_node::RaftNode;
use check_program::raft::types::{NodeId, Request, TypeConfig};
use openraft::{BasicNode, Raft};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn test_raft_cluster_integration() -> anyhow::Result<()> {
    // 1. Setup Router and Directories
    let router = RaftRouter::new();
    let dir1 = TempDir::new()?;
    let dir2 = TempDir::new()?;
    let dir3 = TempDir::new()?;

    // 2. Start Node 1
    let node1 = RaftNode::start(1, dir1.path().to_path_buf(), router.clone()).await?;
    let node1 = Arc::new(node1);
    router.register(1, node1.clone());

    // 3. Start Node 2
    let node2 = RaftNode::start(2, dir2.path().to_path_buf(), router.clone()).await?;
    let node2 = Arc::new(node2);
    router.register(2, node2.clone());

    // 4. Start Node 3
    let node3 = RaftNode::start(3, dir3.path().to_path_buf(), router.clone()).await?;
    let node3 = Arc::new(node3);
    router.register(3, node3.clone());

    // 5. Initialize Cluster (Node 1 as Leader)
    println!("Initializing cluster with Node 1...");
    let mut nodes = BTreeMap::new();
    nodes.insert(1, BasicNode::new("127.0.0.1:9001"));
    
    node1.raft.initialize(nodes).await?;

    // Wait for Node 1 to become leader
    // Simple loop to check metrics
    wait_for_leader(&node1.raft, 1).await?;

    // 6. Add Node 2 and Node 3 as Learners
    println!("Adding learners...");
    node1.raft.add_learner(2, BasicNode::new("127.0.0.1:9002"), true).await?;
    node1.raft.add_learner(3, BasicNode::new("127.0.0.1:9003"), true).await?;

    // 7. Change Membership to [1, 2, 3]
    println!("Changing membership to [1, 2, 3]...");
    let mut members = HashSet::new();
    members.insert(1);
    members.insert(2);
    members.insert(3);
    node1.raft.change_membership(members, true).await?;

    // Wait for all nodes to have the new config
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 8. Create Table
    println!("Creating table...");
    let req = Request::Write {
        sql: "CREATE TABLE test (id INTEGER PRIMARY KEY, name TEXT)".to_string(),
    };
    node1.raft.client_write(req).await?;

    // 9. Write Logs
    println!("Writing logs...");
    for i in 0..10 {
        let req = Request::Write {
            sql: format!("INSERT INTO test (id, name) VALUES ({}, 'name{}')", i, i),
        };
        node1.raft.client_write(req).await?;
    }

    // 10. Verify Logs on Followers
    println!("Verifying logs on followers...");
    // Wait for replication
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check last log index on all nodes
    let metrics1 = node1.raft.metrics().borrow().clone();
    let metrics2 = node2.raft.metrics().borrow().clone();
    let metrics3 = node3.raft.metrics().borrow().clone();

    println!("Node 1 Last Log: {:?}", metrics1.last_log_index);
    println!("Node 2 Last Log: {:?}", metrics2.last_log_index);
    println!("Node 3 Last Log: {:?}", metrics3.last_log_index);

    assert_eq!(metrics1.last_log_index, metrics2.last_log_index);
    assert_eq!(metrics1.last_log_index, metrics3.last_log_index);

    // 11. Trigger Snapshot
    println!("Triggering snapshot on Node 1...");
    node1.raft.trigger().snapshot().await?;

    // Wait for snapshot
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // Check snapshot status
    let snapshot_meta = node1.raft.metrics().borrow().snapshot.clone();
    println!("Node 1 Snapshot: {:?}", snapshot_meta);
    assert!(snapshot_meta.is_some());

    // 12. Test Snapshot Installation & Restart
    println!("Testing node restart and snapshot installation...");

    // Stop Node 2
    println!("Stopping Node 2...");
    node2.shutdown().await?;

    // Write more logs to Leader (Node 1)
    println!("Writing more logs to Leader...");
    for i in 10..20 {
        let req = Request::Write {
            sql: format!("INSERT INTO test (id, name) VALUES ({}, 'name{}')", i, i),
        };
        node1.raft.client_write(req).await?;
    }

    // Trigger snapshot on Leader again to ensure we have a recent snapshot covering new logs
    println!("Triggering 2nd snapshot on Node 1...");
    node1.raft.trigger().snapshot().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Purge logs on Leader to force snapshot replication
    let metrics1 = node1.raft.metrics().borrow().clone();
    if let Some(last_log_index) = metrics1.last_log_index {
        println!("Purging logs on Node 1 up to {}...", last_log_index);
        node1.raft.trigger().purge_log(last_log_index).await?;
    }

    // Restart Node 2
    println!("Restarting Node 2...");
    // Note: In a real scenario, we would close the previous node instance.
    // shutdown() calls raft.shutdown(), but the structs might still be alive in Arc.
    // We create a new one.
    let node2_restarted = RaftNode::start(2, dir2.path().to_path_buf(), router.clone()).await?;
    let node2_restarted = Arc::new(node2_restarted);
    // Update router
    router.register(2, node2_restarted.clone());

    // Wait for Node 2 to catch up
    println!("Waiting for Node 2 to catch up...");
    // Give it enough time to transfer snapshot and apply
    tokio::time::sleep(Duration::from_secs(5)).await;

    let metrics2_new = node2_restarted.raft.metrics().borrow().clone();
    println!("Node 2 (Restarted) Last Log: {:?}", metrics2_new.last_log_index);
    println!("Node 2 (Restarted) Snapshot: {:?}", metrics2_new.snapshot);

    // Verify data via SQL query on Node 2
    // We expect 20 rows (0..20)
    let count_res = node2_restarted.query_scalar("SELECT count(*) FROM test".to_string()).await?;
    println!("Node 2 Row Count: {}", count_res);
    assert_eq!(count_res, "20");

    println!("Integration test passed!");
    Ok(())
}

async fn wait_for_leader(raft: &Raft<TypeConfig>, target_node: NodeId) -> anyhow::Result<()> {
    for _ in 0..20 {
        let metrics = raft.metrics().borrow().clone();
        if let Some(leader) = metrics.current_leader {
            if leader == target_node {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("Timeout waiting for leader election")
}
