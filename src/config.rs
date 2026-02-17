use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsistencyMode {
    Strong, // All nodes must succeed
    Quorum, // N/2 + 1 nodes must succeed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeRole {
    Master,
    Slave,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub master_addr: String,
    pub slave_addrs: Vec<String>,
    pub mode: ConsistencyMode,
    pub committed_history_limit: usize,
}

impl ClusterConfig {
    pub fn new(master_addr: &str, slave_addrs: Vec<&str>, mode: ConsistencyMode) -> Self {
        Self {
            master_addr: master_addr.to_string(),
            slave_addrs: slave_addrs.iter().map(|s| s.to_string()).collect(),
            mode,
            committed_history_limit: 32,
        }
    }
}
