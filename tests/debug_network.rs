use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse, VoteRequest, VoteResponse};
use openraft::error::{RPCError, RaftError, InstallSnapshotError};
use openraft::network::RPCOption;
use async_trait::async_trait;

openraft::declare_raft_types!(
    pub TypeConfig:
        D = String,
        R = String,
        NodeId = u64,
        Node = BasicNode,
        Entry = openraft::Entry<TypeConfig>,
        SnapshotData = std::io::Cursor<Vec<u8>>,
        Responder = openraft::impls::OneshotResponder<TypeConfig>,
        AsyncRuntime = openraft::TokioRuntime,
);

struct MyNetwork {}

#[async_trait]
impl RaftNetwork<TypeConfig> for MyNetwork {
    async fn append_entries(
        &mut self,
        _req: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        todo!()
    }

    async fn install_snapshot(
        &mut self,
        _req: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<InstallSnapshotResponse<u64>, RPCError<u64, BasicNode, RaftError<u64, InstallSnapshotError>>> {
        todo!()
    }

    async fn vote(
        &mut self,
        _req: VoteRequest<u64>,
        _option: RPCOption,
    ) -> Result<VoteResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        todo!()
    }
}

struct MyFactory {}

#[async_trait]
impl RaftNetworkFactory<TypeConfig> for MyFactory {
    type Network = MyNetwork;

    async fn new_client(&mut self, _target: u64, _node: &BasicNode) -> Self::Network {
        MyNetwork {}
    }
}
