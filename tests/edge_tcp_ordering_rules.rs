use anyhow::Result;

use check_program::hub::order_rules::OrderingRules;
use check_program::hub::order_scheduler::OrderingScheduler;
use check_program::management::order_rules_service::{OrderRulesAdminService, OrderRulesStore};
use check_program::pb::order_rules_admin_client::OrderRulesAdminClient;
use check_program::pb::order_rules_admin_server::OrderRulesAdminServer;
use check_program::pb::{GetRulesVersionRequest, UpdateRulesRequest};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

// ### 修改记录 (2026-03-01)
// - 原因: 需要验证顺序规则的 msg_type 匹配能力
// - 目的: 确保规则在 device_prefix + msg_type 条件下返回正确阶段
// - 备注: 该测试先行于实现，用于遵循 TDD 红灯阶段
// - 备注: 规则中包含 priority 以覆盖优先级字段解析
// - 备注: msg_type 使用协议层常量值 (3) 以贴近实际报文
#[tokio::test]
async fn ordering_rules_match_group_and_stage() -> Result<()> {
    let rules = OrderingRules::from_json(
        r#"{
        "rules":[{"device_prefix":"A","msg_type":3,"order_group":"g1","stage":1,"priority":10}]
    }"#,
    )?;
    let matched = rules.match_device("A-001", 3).expect("rule should match");
    assert_eq!(matched.order_group, "g1");
    assert_eq!(matched.stage, 1);
    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要验证 msg_type 精确匹配优先于通配规则
// - 目的: 防止通配规则覆盖更具体的顺序约束
// - 备注: 该测试用于锁定优先级选择逻辑
// - 备注: 优先级字段与 rule 顺序不应影响精确匹配的优先级
#[tokio::test]
async fn ordering_rules_prefers_specific_msg_type() -> Result<()> {
    let rules = OrderingRules::from_json(
        r#"{
        "rules":[
            {"device_prefix":"A","msg_type":3,"order_group":"g1","stage":1,"priority":1},
            {"device_prefix":"A","order_group":"g2","stage":2,"priority":99}
        ]
    }"#,
    )?;
    let matched = rules.match_device("A-002", 3).expect("rule should match");
    assert_eq!(matched.order_group, "g1");
    assert_eq!(matched.stage, 1);
    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要验证顺序域阶段屏障生效
// - 目的: 确保 stage 2 不会在 stage 1 完成前被调度
// - 备注: 该测试先行于实现，用于锁定调度器行为
// - 备注: stage_parallelism=1 便于观察阶段推进
// - 备注: 显式指定调度器泛型以消除类型推断歧义
#[tokio::test]
async fn ordering_scheduler_enforces_stage_barrier() -> Result<()> {
    let scheduler = OrderingScheduler::<String>::new(1, 10);
    scheduler.enqueue("g1", 1, "t1".to_string()).await?;
    scheduler.enqueue("g1", 2, "t2".to_string()).await?;
    let first = scheduler.next_ready().await.expect("first task");
    assert_eq!(first.stage, 1);
    scheduler.mark_done("g1", 1).await;
    let second = scheduler.next_ready().await.expect("second task");
    assert_eq!(second.stage, 2);
    Ok(())
}

// ### 修改记录 (2026-03-01)
// - 原因: 需要验证规则热更新接口可用
// - 目的: 确保新版本只影响新规则快照
// - 备注: 该测试先行于服务实现，用于锁定热更新行为
// - 备注: 通过读取旧快照与新快照对比验证生效范围
#[tokio::test]
async fn ordering_rules_hot_reload_only_affects_new_requests() -> Result<()> {
    let store = Arc::new(OrderRulesStore::new());
    let service = OrderRulesAdminService::new(store.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let incoming = TcpListenerStream::new(listener);
    tokio::spawn(
        Server::builder()
            .add_service(OrderRulesAdminServer::new(service))
            .serve_with_incoming(incoming),
    );

    let mut client = OrderRulesAdminClient::connect(format!("http://{}", addr)).await?;
    let v1 = client
        .update_rules(UpdateRulesRequest {
            rules_json: r#"{"rules":[{"device_prefix":"A","msg_type":3,"order_group":"g1","stage":1,"priority":1}]}"#.to_string(),
        })
        .await?
        .into_inner()
        .version;
    let old_snapshot = store.snapshot().await;

    let v2 = client
        .update_rules(UpdateRulesRequest {
            rules_json: r#"{"rules":[{"device_prefix":"A","msg_type":3,"order_group":"g2","stage":2,"priority":1}]}"#.to_string(),
        })
        .await?
        .into_inner()
        .version;
    let new_snapshot = store.snapshot().await;

    let current = client
        .get_rules_version(GetRulesVersionRequest {})
        .await?
        .into_inner();
    assert!(v2 > v1);
    assert_eq!(current.version, v2);
    assert_eq!(
        old_snapshot.match_device("A-1", 3).unwrap().order_group,
        "g1"
    );
    assert_eq!(
        new_snapshot.match_device("A-1", 3).unwrap().order_group,
        "g2"
    );
    Ok(())
}
