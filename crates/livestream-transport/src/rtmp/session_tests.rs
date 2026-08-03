//! Unit tests for `SessionGuard` and `SessionGuardBuilder`.
//!
//! Decision-layer tests drive a real `rml_rtmp::ClientSession` peer over a
//! `tokio::io::duplex` socket pair, because `accept_request`/`reject_request`
//! require outstanding requests produced by real protocol traffic.

use super::*;
use crate::dispatcher::EventDispatcher;
use crate::lifecycle::HandlerLifecycle;
use livestream_core::types::Protocol;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType, ServerSessionConfig,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn make_guard(registry: Arc<SessionRegistry>) -> (SessionGuard, tokio::io::DuplexStream) {
    let (a, b) = tokio::io::duplex(65536);
    let connection = RtmpConnection::new(Box::new(a));
    let (session, initial) = ServerSession::new(ServerSessionConfig::new()).unwrap();
    let mut guard = match SessionGuardBuilder::new(connection)
        .with_session(session)
        .with_appname("live".to_string())
        .with_registry(registry)
        .build()
    {
        Ok(guard) => guard,
        Err(e) => panic!("failed to build SessionGuard: {e}"),
    };
    // 把 server 初始消息（SetChunkSize/WindowAckSize）写回对端
    let ct = CancellationToken::new();
    guard.handle_results(initial, &ct).await.unwrap();
    (guard, b)
}

fn descriptor(id: &str) -> Arc<tokio::sync::RwLock<SessionDescriptor>> {
    Arc::new(tokio::sync::RwLock::new(SessionDescriptor {
        id: id.to_string(),
        protocol: Protocol::Rtmp,
        endpoint: SessionEndpoint::new(None, None),
        state: SessionState::Pending,
    }))
}

/// Feed client-session responses back to the peer, collect client events.
async fn pump(
    client: &mut ClientSession,
    b: &mut tokio::io::DuplexStream,
) -> Vec<ClientSessionEvent> {
    let mut buf = [0u8; 4096];
    let mut events = Vec::new();
    loop {
        let n = match tokio::time::timeout(Duration::from_millis(50), b.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => break,
        };
        if n == 0 {
            break;
        }
        for result in client.handle_input(&buf[..n]).unwrap() {
            relay_client_result(b, &mut events, result).await;
        }
    }
    events
}

async fn relay_client_result(
    b: &mut tokio::io::DuplexStream,
    events: &mut Vec<ClientSessionEvent>,
    result: ClientSessionResult,
) {
    match result {
        ClientSessionResult::OutboundResponse(packet) => {
            b.write_all(&packet.bytes).await.unwrap();
        }
        ClientSessionResult::RaisedEvent(event) => events.push(event),
        ClientSessionResult::UnhandleableMessageReceived(_) => {}
    }
}

async fn write_outbound(b: &mut tokio::io::DuplexStream, result: ClientSessionResult) {
    if let ClientSessionResult::OutboundResponse(packet) = result {
        b.write_all(&packet.bytes).await.unwrap();
    }
}

/// One server-side round: read, respond, and dispatch events until a
/// handler builder emerges.
async fn drive_server_round(
    guard: &mut SessionGuard,
    pending: &Arc<DashMap<String, HandlerLifecycle>>,
    ct: &CancellationToken,
) -> Option<HandlerBuilder> {
    let results = guard.read_result(ct).await.unwrap();
    let events = guard.handle_results(results, ct).await.unwrap();
    for event in events {
        match guard.handle_connect_event(event, pending, ct).await {
            Ok(Some(builder)) => return Some(builder),
            Ok(None) => {}
            Err(e) => panic!("handle_connect_event failed: {e}"),
        }
    }
    None
}

async fn pending_lifecycle(
    registry: Arc<SessionRegistry>,
) -> Arc<DashMap<String, HandlerLifecycle>> {
    let pending = Arc::new(DashMap::<String, HandlerLifecycle>::new());
    pending.insert(
        "foo".to_string(),
        HandlerLifecycle::new(
            "foo".to_string(),
            Protocol::Rtmp,
            registry,
            Arc::new(EventDispatcher::new()),
        ),
    );
    pending
}

fn expect_err(result: Result<Option<HandlerBuilder>>, what: &str) -> String {
    match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("{what}: expected an error"),
    }
}

/// Dispatch one event through `handle_connect_event`, panicking on error.
async fn dispatch(
    guard: &mut SessionGuard,
    event: ServerSessionEvent,
    pending: &Arc<DashMap<String, HandlerLifecycle>>,
    ct: &CancellationToken,
) -> Option<HandlerBuilder> {
    match guard.handle_connect_event(event, pending, ct).await {
        Ok(Some(builder)) => Some(builder),
        Ok(None) => None,
        Err(e) => panic!("handle_connect_event failed: {e}"),
    }
}

/// Establish a real client connection against a fresh guard.
/// Returns the connected pair; `pending` holds the "foo" lifecycle.
async fn connect_pair(
    registry: Arc<SessionRegistry>,
) -> (
    SessionGuard,
    ClientSession,
    tokio::io::DuplexStream,
    Arc<DashMap<String, HandlerLifecycle>>,
    CancellationToken,
) {
    let ct = CancellationToken::new();
    let pending = pending_lifecycle(registry.clone()).await;
    let (mut guard, mut b) = make_guard(registry).await;
    let (mut client, _initial) = ClientSession::new(ClientSessionConfig::new()).unwrap();
    let out = client.request_connection("live".to_string()).unwrap();
    write_outbound(&mut b, out).await;
    drive_server_round(&mut guard, &pending, &ct).await;
    let events = pump(&mut client, &mut b).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ClientSessionEvent::ConnectionRequestAccepted)),
        "client should observe the connection accepted"
    );
    (guard, client, b, pending, ct)
}

/// One createStream+publish round trip for the given stream key.
/// Returns the server-side outcome of the publish request.
async fn publish_round(
    guard: &mut SessionGuard,
    client: &mut ClientSession,
    b: &mut tokio::io::DuplexStream,
    pending: &Arc<DashMap<String, HandlerLifecycle>>,
    ct: &CancellationToken,
    key: &str,
) -> Result<Option<HandlerBuilder>> {
    let out = client
        .request_publishing(format!("live/{key}"), PublishRequestType::Live)
        .unwrap();
    write_outbound(b, out).await;
    // createStream 轮：server 响应 stream id
    if let Some(builder) = server_round(guard, pending, ct).await? {
        return Ok(Some(builder));
    }
    // client 收到 _result 后自动发 publish 命令
    pump(client, b).await;
    // publish 轮
    server_round(guard, pending, ct).await
}

/// One server-side read+dispatch round, propagating errors.
async fn server_round(
    guard: &mut SessionGuard,
    pending: &Arc<DashMap<String, HandlerLifecycle>>,
    ct: &CancellationToken,
) -> Result<Option<HandlerBuilder>> {
    let results = guard.read_result(ct).await?;
    let events = guard.handle_results(results, ct).await?;
    for event in events {
        if let Some(builder) = guard.handle_connect_event(event, pending, ct).await? {
            return Ok(Some(builder));
        }
    }
    Ok(None)
}

#[test]
fn set_chunk_size_clamps() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (mut guard, _b) = runtime.block_on(make_guard(Arc::new(SessionRegistry::new())));
    guard.set_chunk_size(0);
    assert_eq!(guard.chunk_size, MIN_CHUNK_SIZE);
    guard.set_chunk_size(u32::MAX);
    assert_eq!(guard.chunk_size, MAX_CHUNK_SIZE);
    guard.set_chunk_size(2048);
    assert_eq!(guard.chunk_size, 2048);
    guard.set_chunk_size(MIN_CHUNK_SIZE - 1);
    assert_eq!(guard.chunk_size, MIN_CHUNK_SIZE);
    guard.set_chunk_size(MAX_CHUNK_SIZE + 1);
    assert_eq!(guard.chunk_size, MAX_CHUNK_SIZE);
}

#[test]
fn extract_stream_key_parses() {
    let re = Regex::new(r"^/?(?P<app>[^/?]+)(?:/(?P<stream_key>[^?]+))?(?:\?.*)?$").unwrap();
    assert_eq!(
        SessionGuard::extract_stream_key(&re, "live/foo?x=1", "live"),
        "foo"
    );
    assert_eq!(
        SessionGuard::extract_stream_key(&re, "/live/bar", "live"),
        "bar"
    );
    assert_eq!(SessionGuard::extract_stream_key(&re, "foo", "live"), "foo");
    assert_eq!(
        SessionGuard::extract_stream_key(&re, "?token=1", "live"),
        "live"
    );
}

#[tokio::test]
async fn builder_validation_errors() {
    let (a, _b) = tokio::io::duplex(4096);
    let connection = RtmpConnection::new(Box::new(a));

    let err = match SessionGuardBuilder::new(connection).build() {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected missing-session error"),
    };
    assert!(err.contains("Session is required"));

    let (a, _b) = tokio::io::duplex(4096);
    let connection = RtmpConnection::new(Box::new(a));
    let (session, _) = ServerSession::new(ServerSessionConfig::new()).unwrap();
    let err = match SessionGuardBuilder::new(connection)
        .with_session(session)
        .build()
    {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected missing-appname error"),
    };
    assert!(err.contains("App name is required"));

    let (a, _b) = tokio::io::duplex(4096);
    let connection = RtmpConnection::new(Box::new(a));
    let (session, _) = ServerSession::new(ServerSessionConfig::new()).unwrap();
    let err = match SessionGuardBuilder::new(connection)
        .with_session(session)
        .with_appname("live".to_string())
        .build()
    {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected missing-registry error"),
    };
    assert!(err.contains("Registry is required"));
}

#[tokio::test]
async fn handle_results_writes_outbound_and_collects_events() {
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    let (mut guard, mut b) = make_guard(registry).await;
    // 排空 make_guard 写回的 server 初始消息（SetChunkSize/WindowAckSize）
    let mut buf = [0u8; 256];
    let _ = tokio::time::timeout(Duration::from_millis(100), b.read(&mut buf)).await;
    let (mut client, _initial) = ClientSession::new(ClientSessionConfig::new()).unwrap();
    let out = client.request_connection("live".to_string()).unwrap();
    write_outbound(&mut b, out).await;

    // read_result 解析真实 connect 请求
    let results = guard.read_result(&ct).await.unwrap();
    assert!(results.iter().any(|r| matches!(
        r,
        ServerSessionResult::RaisedEvent(ServerSessionEvent::ConnectionRequested { .. })
    )));

    // handle_results 收集事件；未 accept 前无响应字节
    let events = guard.handle_results(results, &ct).await.unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerSessionEvent::ConnectionRequested { .. }))
    );
    let mut buf = [0u8; 256];
    let timed_out = tokio::time::timeout(Duration::from_millis(100), b.read(&mut buf)).await;
    assert!(timed_out.is_err(), "no accept yet, so no bytes written");
}

#[tokio::test]
async fn connect_event_accepts_matching_app() {
    // connect_pair 内部完成完整 connect 握手，断言连接被接受
    let (guard, _client, _b, _pending, _ct) = connect_pair(Arc::new(SessionRegistry::new())).await;
    // 连接已建立：client 收到 ConnectionRequestAccepted（connect_pair 断言），
    // guard 存活即可
    drop(guard);
}

#[tokio::test]
async fn connect_event_rejects_wrong_app() {
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    let pending = pending_lifecycle(registry.clone()).await;
    let (mut guard, mut b) = make_guard(registry).await;
    let (mut client, _initial) = ClientSession::new(ClientSessionConfig::new()).unwrap();
    let out = client.request_connection("other".to_string()).unwrap();
    write_outbound(&mut b, out).await;

    let results = guard.read_result(&ct).await.unwrap();
    let events = guard.handle_results(results, &ct).await.unwrap();
    let event = events
        .iter()
        .find(|e| matches!(e, ServerSessionEvent::ConnectionRequested { .. }))
        .expect("expected a ConnectionRequested event")
        .clone();

    // 拒绝路径：决策层返回 Err，reject 字节写回对端
    let err = expect_err(
        guard.handle_connect_event(event, &pending, &ct).await,
        "connect_event_rejects_wrong_app",
    );
    assert!(err.contains("unexpected app"));
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(Duration::from_millis(100), b.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0);
}

#[tokio::test]
async fn play_requested_accepts_active_stream() {
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    registry
        .register_session(descriptor("foo"), ct.child_token())
        .await
        .unwrap();
    registry
        .update_state("foo", SessionState::Connected)
        .await
        .unwrap();
    let (mut guard, mut client, mut b, pending, _ct) = connect_pair(registry).await;

    // createStream 轮 + pump（client 收到 _result 后自动发 play 命令）
    let out = client.request_playback("live/foo".to_string()).unwrap();
    write_outbound(&mut b, out).await;
    drive_server_round(&mut guard, &pending, &ct).await;
    pump(&mut client, &mut b).await;

    // play 轮 → 接受并产生 play builder
    let builder = drive_server_round(&mut guard, &pending, &ct)
        .await
        .expect("active stream play should be accepted");
    assert_eq!(builder.stream_key(), "foo");
}

#[tokio::test]
async fn play_requested_rejects_inactive_stream() {
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    let (mut guard, mut client, mut b, pending, _ct) = connect_pair(registry).await;

    let out = client.request_playback("live/missing".to_string()).unwrap();
    write_outbound(&mut b, out).await;
    drive_server_round(&mut guard, &pending, &ct).await;
    pump(&mut client, &mut b).await;

    let results = guard.read_result(&ct).await.unwrap();
    let events = guard.handle_results(results, &ct).await.unwrap();
    let event = events
        .iter()
        .find(|e| matches!(e, ServerSessionEvent::PlayStreamRequested { .. }))
        .expect("expected a PlayStreamRequested event")
        .clone();
    let err = expect_err(
        guard.handle_connect_event(event, &pending, &ct).await,
        "play inactive stream",
    );
    assert!(err.contains("non-existent or inactive"));
}

#[tokio::test]
async fn publish_requested_state_machine() {
    // 场景 1：pending → accept + connecting
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    registry
        .register_session(descriptor("foo"), ct.child_token())
        .await
        .unwrap();
    let (mut guard, mut client, mut b, pending, _ct) = connect_pair(registry.clone()).await;
    let builder = publish_round(&mut guard, &mut client, &mut b, &pending, &ct, "foo")
        .await
        .expect("publish round should not error")
        .expect("pending stream should produce a publish builder");
    assert_eq!(builder.stream_key(), "foo");
    assert_eq!(
        guard.registry.get_state("foo").await,
        Some(SessionState::Connecting)
    );
    drop((guard, client, b, pending));

    // 场景 2：无 pending lifecycle → 报错
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    registry
        .register_session(descriptor("nope"), ct.child_token())
        .await
        .unwrap();
    let (mut guard, mut client, mut b, pending, _ct) = connect_pair(registry).await;
    let err = expect_err(
        publish_round(&mut guard, &mut client, &mut b, &pending, &ct, "nope").await,
        "publish without lifecycle",
    );
    assert!(err.contains("no pending lifecycle"));
    drop((guard, client, b, pending));

    // 场景 3：registry 无该 stream → 拒绝并断开 lifecycle
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    let (mut guard, mut client, mut b, pending, _ct) = connect_pair(registry).await;
    pending.insert(
        "gone".to_string(),
        HandlerLifecycle::new(
            "gone".to_string(),
            Protocol::Rtmp,
            Arc::new(SessionRegistry::new()),
            Arc::new(EventDispatcher::new()),
        ),
    );
    let err = expect_err(
        publish_round(&mut guard, &mut client, &mut b, &pending, &ct, "gone").await,
        "publish to unknown stream",
    );
    assert!(err.contains("does not exist"));
    assert!(pending.get("gone").unwrap().disconnected());
    drop((guard, client, b, pending));

    // 场景 4：stream 已活跃 → 拒绝
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    registry
        .register_session(descriptor("busy"), ct.child_token())
        .await
        .unwrap();
    registry
        .update_state("busy", SessionState::Connected)
        .await
        .unwrap();
    let (mut guard, mut client, mut b, pending, _ct) = connect_pair(registry).await;
    pending.insert(
        "busy".to_string(),
        HandlerLifecycle::new(
            "busy".to_string(),
            Protocol::Rtmp,
            Arc::new(SessionRegistry::new()),
            Arc::new(EventDispatcher::new()),
        ),
    );
    let err = expect_err(
        publish_round(&mut guard, &mut client, &mut b, &pending, &ct, "busy").await,
        "publish to active stream",
    );
    assert!(err.contains("already active"));
}

#[tokio::test]
async fn optional_amf0_commands() {
    let ct = CancellationToken::new();
    let pending = pending_lifecycle(Arc::new(SessionRegistry::new())).await;
    let (mut guard, mut b) = make_guard(Arc::new(SessionRegistry::new())).await;

    let handled = dispatch(
        &mut guard,
        ServerSessionEvent::UnhandleableAmf0Command {
            command_name: "_checkbw".to_string(),
            transaction_id: 3.0,
            command_object: Amf0Value::Null,
            additional_values: Vec::new(),
        },
        &pending,
        &ct,
    )
    .await;
    assert!(handled.is_none());
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(Duration::from_millis(100), b.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0);

    // 未知命令 → 无响应
    let handled = dispatch(
        &mut guard,
        ServerSessionEvent::UnhandleableAmf0Command {
            command_name: "unknown_cmd".to_string(),
            transaction_id: 4.0,
            command_object: Amf0Value::Null,
            additional_values: Vec::new(),
        },
        &pending,
        &ct,
    )
    .await;
    assert!(handled.is_none());
    let mut buf = [0u8; 256];
    let timed_out = tokio::time::timeout(Duration::from_millis(100), b.read(&mut buf)).await;
    assert!(timed_out.is_err(), "unknown command must not write bytes");
}

#[tokio::test]
async fn unhandled_event_errors() {
    let ct = CancellationToken::new();
    let pending = pending_lifecycle(Arc::new(SessionRegistry::new())).await;
    let (mut guard, _b) = make_guard(Arc::new(SessionRegistry::new())).await;

    let err = expect_err(
        guard
            .handle_connect_event(
                ServerSessionEvent::AcknowledgementReceived { bytes_received: 0 },
                &pending,
                &ct,
            )
            .await,
        "unhandled event",
    );
    assert!(err.contains("Unhandled session event"));
}

#[tokio::test]
async fn read_result_detects_closed_connection() {
    let ct = CancellationToken::new();
    let (mut guard, b) = make_guard(Arc::new(SessionRegistry::new())).await;
    drop(b);
    let err = guard.read_result(&ct).await.unwrap_err();
    assert!(err.to_string().contains("Connection closed"));
}

#[tokio::test]
async fn e2e_connect_and_publish_via_client_session() {
    let ct = CancellationToken::new();
    let registry = Arc::new(SessionRegistry::new());
    registry
        .register_session(descriptor("foo"), ct.child_token())
        .await
        .unwrap();
    let (mut guard, mut client, mut b, pending, _ct) = connect_pair(registry).await;

    // publish：client 请求，server 决策层返回 publish builder
    let builder = publish_round(&mut guard, &mut client, &mut b, &pending, &ct, "foo")
        .await
        .expect("publish round should not error")
        .expect("publish should produce a handler builder");
    assert_eq!(builder.stream_key(), "foo");
    assert_eq!(
        guard.registry.get_state("foo").await,
        Some(SessionState::Connecting)
    );

    // client 侧确认 publish 被接受
    let client_events = pump(&mut client, &mut b).await;
    assert!(
        client_events
            .iter()
            .any(|e| matches!(e, ClientSessionEvent::PublishRequestAccepted)),
        "client should observe the publish accepted"
    );
}
