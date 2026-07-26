use std::sync::Arc;
use std::time::Duration;

use livestream_core::types::Protocol;
use livestream_transport::dispatcher::{EndReason, EventDispatcher, SessionEvent};
use livestream_transport::lifecycle::HandlerLifecycle;
use livestream_transport::registry::SessionRegistry;
use livestream_transport::registry::state::{SessionEndpoint, SessionState};
use tokio_util::sync::CancellationToken;

/// Poll registry until the expected state is observed, with a 1-second timeout.
async fn wait_for_state(
    registry: &SessionRegistry,
    live_id: &str,
    expected: SessionState,
) -> Option<SessionState> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let s = registry.get_state(live_id).await;
            if s == Some(expected) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .ok()
    .flatten()
}

// ── Test 5: SessionRegistry state transitions ──

#[tokio::test]
async fn test_session_registry_state_transitions() {
    let registry = Arc::new(SessionRegistry::new());
    let ct = CancellationToken::new();
    let endpoint = SessionEndpoint::new(None, None);

    let descriptor = Arc::new(tokio::sync::RwLock::new(
        livestream_transport::registry::state::SessionDescriptor {
            id: "test-stream".to_string(),
            protocol: Protocol::Rtmp,
            endpoint: endpoint.clone(),
            state: SessionState::Pending,
        },
    ));

    registry
        .register_session(descriptor, ct.child_token())
        .await
        .expect("register_session should succeed");

    // Verify initial state is Pending.
    let state = registry.get_state("test-stream").await;
    assert_eq!(state, Some(SessionState::Pending));

    // Transition: Pending → Connecting.
    registry
        .update_state("test-stream", SessionState::Connecting)
        .await
        .expect("update_state to Connecting should succeed");
    let state = registry.get_state("test-stream").await;
    assert_eq!(state, Some(SessionState::Connecting));

    // Transition: Connecting → Connected.
    registry
        .update_state("test-stream", SessionState::Connected)
        .await
        .expect("update_state to Connected should succeed");
    let state = registry.get_state("test-stream").await;
    assert_eq!(state, Some(SessionState::Connected));
}

// ── Test 6: EventDispatcher broadcast ──

#[tokio::test]
async fn test_event_dispatcher_broadcast() {
    let dispatcher = Arc::new(EventDispatcher::new());

    // Global subscription.
    let mut global_rx = dispatcher.subscribe_global();

    // Send a SessionStarted event.
    dispatcher.send(SessionEvent::SessionStarted {
        live_id: "s1".to_string(),
        protocol: Protocol::Rtmp,
    });

    // Global subscriber should receive it.
    let event = global_rx.try_recv();
    assert!(
        event.is_some(),
        "global subscriber should receive SessionStarted"
    );
    match event.unwrap() {
        SessionEvent::SessionStarted { live_id, protocol } => {
            assert_eq!(live_id, "s1");
            assert_eq!(protocol, Protocol::Rtmp);
        }
        other => panic!("expected SessionStarted, got: {:?}", other),
    }

    // Per-stream subscription.
    let mut stream_rx = dispatcher.subscribe("s2");
    dispatcher.send(SessionEvent::SessionStarted {
        live_id: "s2".to_string(),
        protocol: Protocol::Rtsp,
    });
    let event = stream_rx.try_recv();
    assert!(
        event.is_some(),
        "per-stream subscriber should receive SessionStarted"
    );
}

// ── Test 7: HandlerLifecycle full lifecycle ──

#[tokio::test]
async fn test_handler_lifecycle_full_lifecycle() {
    let registry = Arc::new(SessionRegistry::new());
    let dispatcher = Arc::new(EventDispatcher::new());
    let mut global_rx = dispatcher.subscribe_global();

    let lifecycle = HandlerLifecycle::new(
        "lifecycle-test".to_string(),
        Protocol::Rtmp,
        registry.clone(),
        dispatcher.clone(),
    );

    let endpoint = SessionEndpoint::new(Some(1935), None);
    let ct = CancellationToken::new();

    // pending: registers in Pending state.
    lifecycle
        .pending(endpoint, ct.child_token())
        .await
        .expect("pending should succeed");

    let state = registry.get_state("lifecycle-test").await;
    assert_eq!(state, Some(SessionState::Pending));

    // connecting: transitions to Connecting.
    lifecycle
        .connecting()
        .await
        .expect("connecting should succeed");
    let state = registry.get_state("lifecycle-test").await;
    assert_eq!(state, Some(SessionState::Connecting));

    // connect: transitions to Connected and fires SessionStarted.
    lifecycle.connect().await.expect("connect should succeed");
    let state = registry.get_state("lifecycle-test").await;
    assert_eq!(state, Some(SessionState::Connected));

    let event = global_rx.try_recv();
    assert!(
        event.is_some(),
        "dispatcher should emit SessionStarted after connect"
    );
    assert!(
        matches!(event.unwrap(), SessionEvent::SessionStarted { .. }),
        "expected SessionStarted event"
    );
}

// ── Test 8: HandlerLifecycle disconnect with reason ──

#[tokio::test]
async fn test_handler_lifecycle_disconnect_reason() {
    let registry = Arc::new(SessionRegistry::new());
    let dispatcher = Arc::new(EventDispatcher::new());
    let mut global_rx = dispatcher.subscribe_global();

    let lifecycle = HandlerLifecycle::new(
        "disconnect-test".to_string(),
        Protocol::Rtmp,
        registry.clone(),
        dispatcher.clone(),
    );

    let endpoint = SessionEndpoint::new(Some(1935), None);
    let ct = CancellationToken::new();

    // Set up full lifecycle to Connected.
    lifecycle
        .pending(endpoint, ct.child_token())
        .await
        .expect("pending should succeed");
    lifecycle
        .connecting()
        .await
        .expect("connecting should succeed");
    lifecycle.connect().await.expect("connect should succeed");

    // Drain the SessionStarted from connect().
    let _ = global_rx.try_recv();

    // Disconnect with reason.
    lifecycle.disconnect_with_reason(EndReason::Timeout);

    // Allow async cleanup to propagate.
    tokio::task::yield_now().await;

    // Verify registry state updated to Disconnected.
    let state = wait_for_state(&registry, "disconnect-test", SessionState::Disconnected)
        .await
        .expect("state should transition to Disconnected within 1 second");

    assert_eq!(state, SessionState::Disconnected);

    // Verify SessionEnded event emitted.
    let event = global_rx.try_recv();
    assert!(
        event.is_some(),
        "dispatcher should emit SessionEnded after disconnect"
    );
    match event.unwrap() {
        SessionEvent::SessionEnded {
            live_id, reason, ..
        } => {
            assert_eq!(live_id, "disconnect-test");
            assert!(matches!(reason, EndReason::Timeout));
        }
        other => panic!("expected SessionEnded, got: {:?}", other),
    }
}
