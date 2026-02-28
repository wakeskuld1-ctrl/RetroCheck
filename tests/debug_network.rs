// ### 修改记录 (2026-02-27)
// - 原因: 需要最小网络测试实现
// - 目的: 覆盖 RaftNetwork 接口占位符
use openraft::{BasicNode, RaftNetwork, RaftNetworkFactory};
// ### 修改记录 (2026-02-27)
// - 原因: 需要 RPC 请求响应类型
// - 目的: 生成成功或失败的结果
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
// ### 修改记录 (2026-02-27)
// - 原因: RaftNetwork 使用 OpenRaft 宏生成 async 接口
// - 目的: 避免自定义 async_trait 引起签名不一致
use openraft::error::{Fatal, InstallSnapshotError, RPCError, RaftError, RemoteError};
use openraft::network::RPCOption;

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

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要可配置网络行为
/// - 目的: 支持成功/失败两种路径
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkMode {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要稳定成功模式
    /// - 目的: 便于成功路径测试
    AlwaysOk,
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要稳定失败模式
    /// - 目的: 便于失败路径测试
    AlwaysFail,
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要最小网络实现
/// - 目的: 实现 RaftNetwork 接口
struct MyNetwork {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要保存网络模式
    /// - 目的: 决定成功或失败
    mode: NetworkMode,
}

impl MyNetwork {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要构造网络实例
    /// - 目的: 注入可配置行为
    fn new(mode: NetworkMode) -> Self {
        Self { mode }
    }
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 使用 OpenRaft 提供的 trait 形式
/// - 目的: 与宏生成签名保持一致
impl RaftNetwork<TypeConfig> for MyNetwork {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要失败路径
        // - 目的: 模拟网络不可达
        if self.mode == NetworkMode::AlwaysFail {
            let target = req.vote.leader_id.voted_for().unwrap_or_default();
            return Err(RPCError::RemoteError(RemoteError::new(
                target,
                RaftError::Fatal(Fatal::Panicked),
            )));
        }
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要成功路径
        // - 目的: 返回成功响应
        Ok(AppendEntriesResponse::Success)
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<u64>,
        RPCError<u64, BasicNode, RaftError<u64, InstallSnapshotError>>,
    > {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要失败路径
        // - 目的: 模拟网络不可达
        if self.mode == NetworkMode::AlwaysFail {
            let target = req.vote.leader_id.voted_for().unwrap_or_default();
            return Err(RPCError::RemoteError(RemoteError::new(
                target,
                RaftError::Fatal(Fatal::Panicked),
            )));
        }
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要成功路径
        // - 目的: 返回成功响应
        Ok(InstallSnapshotResponse { vote: req.vote })
    }

    async fn vote(
        &mut self,
        req: VoteRequest<u64>,
        _option: RPCOption,
    ) -> Result<VoteResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要失败路径
        // - 目的: 模拟网络不可达
        if self.mode == NetworkMode::AlwaysFail {
            let target = req.vote.leader_id.voted_for().unwrap_or_default();
            return Err(RPCError::RemoteError(RemoteError::new(
                target,
                RaftError::Fatal(Fatal::Panicked),
            )));
        }
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要成功路径
        // - 目的: 返回成功响应
        Ok(VoteResponse::new(req.vote, req.last_log_id, true))
    }
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要网络工厂
/// - 目的: 注入网络模式
struct MyFactory {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要保存网络模式
    /// - 目的: 生成一致的网络行为
    mode: NetworkMode,
}

impl MyFactory {
    /// ### 修改记录 (2026-02-27)
    /// - 原因: 需要构造工厂
    /// - 目的: 注入模式配置
    fn new(mode: NetworkMode) -> Self {
        Self { mode }
    }
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 使用 OpenRaft 提供的 trait 形式
/// - 目的: 与宏生成签名保持一致
impl RaftNetworkFactory<TypeConfig> for MyFactory {
    type Network = MyNetwork;

    async fn new_client(&mut self, _target: u64, _node: &BasicNode) -> Self::Network {
        // ### 修改记录 (2026-02-27)
        // - 原因: 需要生成网络实例
        // - 目的: 复用模式配置
        MyNetwork::new(self.mode)
    }
}

/// ### 修改记录 (2026-02-27)
/// - 原因: 需要引用测试网络结构
/// - 目的: 避免 dead_code 报错
#[tokio::test]
async fn debug_network_factory_smoke() {
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要创建基本节点
    // - 目的: 构造 new_client 参数
    let node = BasicNode::new("node-1");
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要构造工厂
    // - 目的: 使用 AlwaysOk 模式
    let mut factory = MyFactory::new(NetworkMode::AlwaysOk);
    // ### 修改记录 (2026-02-27)
    // - 原因: 需要触发 new_client
    // - 目的: 覆盖构造路径
    let _network = factory.new_client(1, &node).await;
}
