use anyhow::{Result, anyhow};
use check_program::hub::edge_schema::{decode_auth_ack, decode_signed_response};
use check_program::hub::protocol::{
    MSG_TYPE_AUTH_HELLO, MSG_TYPE_RESPONSE, MSG_TYPE_SESSION_REQUEST,
};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::Duration;

mod edge_tcp_common;
use edge_tcp_common::{
    build_auth_hello, build_session_request, connect_with_retry, header_prefix_for_response,
    read_frame, send_frame, spawn_gateway, spawn_gateway_with_handle,
};

#[tokio::test]
async fn edge_tcp_allows_parallel_sessions() -> Result<()> {
    let (addr, handle) = spawn_gateway().await?;
    let mut tasks = Vec::new();

    for i in 0..8u64 {
        let addr = addr.clone();
        tasks.push(tokio::spawn(async move {
            let mut stream = connect_with_retry(&addr).await?;
            let auth_payload = build_auth_hello(100 + i, 1000 + i, 1 + i);
            send_frame(&mut stream, MSG_TYPE_AUTH_HELLO, 1000 + i, &auth_payload).await?;
            let (auth_header, auth_resp) = read_frame(&mut stream).await?;
            if auth_header.msg_type != MSG_TYPE_RESPONSE {
                return Err(anyhow!("unexpected auth response type"));
            }
            let ack = decode_auth_ack(&auth_resp)?;

            let session_payload =
                build_session_request(ack.session_id, 2000 + i, 2 + i, &ack.session_key);
            send_frame(
                &mut stream,
                MSG_TYPE_SESSION_REQUEST,
                2000 + i,
                &session_payload,
            )
            .await?;
            let (resp_header, resp_payload) = read_frame(&mut stream).await?;
            if resp_header.msg_type != MSG_TYPE_RESPONSE {
                return Err(anyhow!("unexpected session response type"));
            }
            let header_prefix = header_prefix_for_response(resp_header.request_id);
            let signed = decode_signed_response(&resp_payload, &header_prefix, |id| {
                if id == ack.session_id {
                    Some(ack.session_key.clone())
                } else {
                    None
                }
            })?;

            let response: Value = serde_json::from_slice(&signed.payload)?;
            if response["success"].as_u64().unwrap_or(0) != 1 {
                return Err(anyhow!("unexpected payload: {:?}", response));
            }
            Ok::<(), anyhow::Error>(())
        }));
    }

    for task in tasks {
        task.await??;
    }

    handle.abort();
    Ok(())
}

// ### 修改记录 (2026-03-15)
// - 原因: 需要先用 TDD 引入排空回归测试的占位调用
// - 目的: 让后续新增的 spawn_gateway_with_handle 在编译期被强约束
// - 备注: 该测试会先失败，作为实现前的 RED 阶段
// - 追加: 进入实现阶段后，用连接失败断言替换占位逻辑
#[tokio::test]
async fn edge_tcp_draining_rejects_connect_then_allows_after_resume() -> Result<()> {
    // ### 修改记录 (2026-03-15)
    // - 原因: 排空测试需要能直接控制网关 draining 状态
    // - 目的: 暴露网关句柄以便后续测试逻辑挂接
    // - 备注: 真实回归测试需要验证 connect 直接失败（A 判定）
    let (addr, gateway, handle) = spawn_gateway_with_handle().await?;
    // ### 修改记录 (2026-03-15)
    // - 原因: 排空状态切换需要同步给网关主循环
    // - 目的: 保证后续 connect 行为可重现失败
    gateway.set_draining(true);
    // ### 修改记录 (2026-03-15)
    // - 原因: 允许运行循环感知 draining 并关闭监听
    // - 目的: 避免测试因为时序过早而误判
    tokio::time::sleep(Duration::from_millis(50)).await;

    for _ in 0..5 {
        // ### 修改记录 (2026-03-15)
        // - 原因: A 判定要求 connect 直接失败
        // - 目的: 约束排空期间不能建立 TCP 连接
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            TcpStream::connect(&addr),
        )
        .await;
        // ### 修改记录 (2026-03-15)
        // - 原因: 部分系统在无监听时会表现为连接超时
        // - 目的: 将 timeout 视为 connect 失败的一种形式，避免误报
        match result {
            Ok(Ok(_stream)) => {
                return Err(anyhow!("connect should fail while draining"));
            }
            Ok(Err(_)) => {}
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // ### 修改记录 (2026-03-15)
    // - 原因: 排空结束后需要恢复连接能力
    // - 目的: 验证关闭监听后仍可重新 bind
    gateway.set_draining(false);
    let _stream = connect_with_retry(&addr).await?;
    handle.abort();
    Ok(())
}

// ### 修改记录 (2026-03-15)
// - 原因: 需要覆盖 inflight_requests 的并发连接统计
// - 目的: 确保连接生命周期与 inflight 计数一致
// - 备注: 通过保持连接存活来避免提前 drop
#[tokio::test]
async fn edge_tcp_inflight_requests_tracks_concurrent_connections() -> Result<()> {
    // ### 修改记录 (2026-03-15)
    // - 原因: 测试需要读取 inflight_requests 值
    // - 目的: 获取网关句柄并控制生命周期
    let (addr, gateway, handle) = spawn_gateway_with_handle().await?;
    let mut streams = Vec::new();

    // ### 修改记录 (2026-03-15)
    // - 原因: 并发连接需要先建立稳定的 TCP 会话
    // - 目的: 形成 inflight 计数的可观测窗口
    for _ in 0..8 {
        streams.push(connect_with_retry(&addr).await?);
    }

    // ### 修改记录 (2026-03-15)
    // - 原因: accept 与计数存在异步时序
    // - 目的: 留出 inflight 累加时间以降低抖动
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        gateway.inflight_requests() >= 8,
        "inflight should reflect concurrent connections"
    );

    drop(streams);
    // ### 修改记录 (2026-03-15)
    // - 原因: 连接关闭后计数需要回落到 0
    // - 目的: 验证 guard drop 能正常减计数
    for _ in 0..10 {
        if gateway.inflight_requests() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(gateway.inflight_requests(), 0);

    handle.abort();
    Ok(())
}
