use std::sync::Arc;

use super::Buffer;
use super::Emitter;
use super::TRAILING_OUTPUT_GRACE;
use super::spawn_exit_watcher;
use super::start_streaming_output;
use super::utf8_boundary;
use crate::session::tests::make_session_and_context_with_rx;
use crate::unified_exec::UnifiedExecContext;
use crate::unified_exec::UnifiedExecProcessManager;
use crate::unified_exec::process::OutputBuffers;
use crate::unified_exec::process::UnifiedExecProcess;
use codex_protocol::items::CommandExecutionStatus;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecCommandOutputDeltaEvent;
use codex_protocol::protocol::ExecOutputStream;
use codex_sandboxing::SandboxType;

use pretty_assertions::assert_eq;
use tokio::time::Duration;
use tokio::time::Instant;

struct StreamingOutputHarness {
    process: Arc<UnifiedExecProcess>,
    stdout_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    exit_tx: tokio::sync::oneshot::Sender<i32>,
    output_buffer: Arc<tokio::sync::Mutex<OutputBuffers>>,
    context: UnifiedExecContext,
    rx_event: async_channel::Receiver<Event>,
}

async fn streaming_output_harness() -> anyhow::Result<StreamingOutputHarness> {
    let (writer_tx, _writer_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let (stdout_tx, stdout_rx) = tokio::sync::broadcast::channel::<Vec<u8>>(8);
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<i32>();
    let spawned = codex_utils_pty::spawn_from_driver(codex_utils_pty::ProcessDriver {
        writer_tx,
        stdout_rx,
        stderr_rx: None,
        exit_rx,
        terminator: None,
        writer_handle: None,
        resizer: None,
        #[cfg(windows)]
        tty: false,
    });
    let process = Arc::new(UnifiedExecProcess::from_spawned(spawned, SandboxType::None).await?);
    let (session, turn, rx_event) = make_session_and_context_with_rx().await;
    let context = UnifiedExecContext::new(
        session,
        crate::session::step_context::StepContext::for_test(turn),
        tokio_util::sync::CancellationToken::new(),
        "streaming-output-test".to_string(),
    );
    let output_buffer = Arc::clone(&process.output_handles().output_buffer);

    Ok(StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        output_buffer,
        context,
        rx_event,
    })
}

#[test_case::test_case(b""; "no_late_output")]
#[test_case::test_case(b"late\n"; "with_late_output")]
#[tokio::test]
async fn completed_output_preserves_bytes_before_subscription(
    late_output: &[u8],
) -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        output_buffer,
        context,
        rx_event,
    } = streaming_output_harness().await?;

    let collected = process.output_handles().output_notify.notified();
    tokio::pin!(collected);
    collected.as_mut().enable();
    stdout_tx.send(b"early\n".to_vec())?;
    collected.await;

    let model_output = UnifiedExecProcessManager::collect_output_until_deadline(
        process.output_handles(),
        /*pause_state*/ None,
        Instant::now(),
    )
    .await;
    assert_eq!(model_output.to_bytes(), b"early\n");

    start_streaming_output(&process, &context);
    #[allow(deprecated)]
    let cwd = context.step_context.turn.cwd.clone().into();
    spawn_exit_watcher(
        Arc::clone(&process),
        &context,
        vec!["proof".to_string()],
        cwd,
        /*process_id*/ 123,
        /*plugin_attribution*/ None,
        output_buffer,
        Instant::now(),
        /*network_denial_monitor*/ None,
        /*plugin_metrics_sidecar*/ None,
    );
    stdout_tx.send(late_output.to_vec())?;
    drop(stdout_tx);
    exit_tx.send(0).expect("send exit");

    let item = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let EventMsg::ItemCompleted(completed) = rx_event.recv().await?.msg
                && let TurnItem::CommandExecution(item) = completed.item
            {
                return Ok::<_, async_channel::RecvError>(item);
            }
        }
    })
    .await??;
    let expected_output = String::from_utf8([b"early\n".as_slice(), late_output].concat())?;
    assert_eq!(
        (item.status, item.exit_code, item.aggregated_output),
        (
            CommandExecutionStatus::Completed,
            Some(0),
            Some(expected_output)
        )
    );
    Ok(())
}

#[tokio::test]
async fn streaming_output_preserves_multibyte_characters_across_chunks() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        output_buffer,
        rx_event,
        context,
    } = streaming_output_harness().await?;
    start_streaming_output(&process, &context);
    let output_drained = process.output_drained_notify();
    let drained = output_drained.notified();
    tokio::pin!(drained);

    stdout_tx.send(vec![0xc3]).expect("send UTF-8 lead byte");
    stdout_tx
        .send(vec![0xa9])
        .expect("send UTF-8 continuation byte");
    drop(stdout_tx);
    exit_tx.send(0).expect("send exit");
    (&mut drained).await;

    let event = rx_event.recv().await.expect("receive output delta");
    let EventMsg::ExecCommandOutputDelta(delta) = event.msg else {
        panic!("expected ExecCommandOutputDelta");
    };
    assert_eq!(
        delta,
        ExecCommandOutputDeltaEvent {
            call_id: "streaming-output-test".to_string(),
            stream: ExecOutputStream::Stdout,
            chunk: "é".as_bytes().to_vec(),
        }
    );
    assert_eq!(
        output_buffer
            .lock()
            .await
            .transcript
            .to_bytes_with_omission_marker(),
        "é".as_bytes()
    );
    assert!(rx_event.try_recv().is_err());

    Ok(())
}

#[tokio::test]
async fn streaming_output_finishes_on_close_without_waiting_for_grace() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        output_buffer,
        context,
        ..
    } = streaming_output_harness().await?;
    start_streaming_output(&process, &context);
    let output_drained = process.output_drained_notify();
    let drained = output_drained.notified();
    tokio::pin!(drained);

    tokio::time::pause();
    let exited_at = Instant::now();
    exit_tx.send(0).expect("send exit");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stdout_tx
            .send(b"LATE-OUTPUT-MARKER\xc3".to_vec())
            .expect("send late output");
    });

    (&mut drained).await;
    let elapsed = Instant::now().saturating_duration_since(exited_at);
    tokio::time::resume();

    assert!(
        elapsed >= Duration::from_millis(50) && elapsed < TRAILING_OUTPUT_GRACE,
        "output close should finish before the grace fallback: {elapsed:?}"
    );
    assert_eq!(
        output_buffer
            .lock()
            .await
            .transcript
            .to_bytes_with_omission_marker(),
        b"LATE-OUTPUT-MARKER\xc3"
    );

    Ok(())
}

#[tokio::test]
async fn streaming_output_keeps_grace_as_fallback_without_close() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        output_buffer,
        rx_event,
        context,
    } = streaming_output_harness().await?;
    start_streaming_output(&process, &context);
    let output_drained = process.output_drained_notify();
    let drained = output_drained.notified();
    tokio::pin!(drained);

    tokio::time::pause();
    let exited_at = Instant::now();
    stdout_tx.send(vec![0xc3]).expect("send UTF-8 lead byte");
    exit_tx.send(0).expect("send exit");
    (&mut drained).await;
    let elapsed = Instant::now().saturating_duration_since(exited_at);
    tokio::time::resume();

    assert!(
        elapsed >= TRAILING_OUTPUT_GRACE
            && elapsed <= TRAILING_OUTPUT_GRACE + Duration::from_millis(10),
        "missing output close should use the grace fallback: {elapsed:?}"
    );
    assert_eq!(
        output_buffer
            .lock()
            .await
            .transcript
            .to_bytes_with_omission_marker(),
        vec![0xc3]
    );
    let event = rx_event.try_recv().expect("receive final output delta");
    let EventMsg::ExecCommandOutputDelta(delta) = event.msg else {
        panic!("expected ExecCommandOutputDelta");
    };
    assert_eq!(
        delta,
        ExecCommandOutputDeltaEvent {
            call_id: "streaming-output-test".to_string(),
            stream: ExecOutputStream::Stdout,
            chunk: vec![0xc3],
        }
    );
    assert!(rx_event.try_recv().is_err());

    Ok(())
}

#[tokio::test]
async fn exit_watcher_waits_for_late_network_denial_before_classifying_end() -> anyhow::Result<()> {
    let StreamingOutputHarness {
        process,
        stdout_tx,
        exit_tx,
        output_buffer,
        mut context,
        rx_event,
    } = streaming_output_harness().await?;
    start_streaming_output(&process, &context);

    tokio::time::pause();
    let process_for_late_denial = Arc::clone(&process);
    let (late_denial_armed_tx, late_denial_armed_rx) = tokio::sync::oneshot::channel();
    let network_denial_monitor = tokio::spawn(async move {
        let sleep = tokio::time::sleep(Duration::from_millis(10));
        tokio::pin!(sleep);
        late_denial_armed_tx.send(()).expect("arm late denial");
        sleep.await;
        process_for_late_denial.fail_and_terminate("LATE_DENIAL".to_string());
    });
    late_denial_armed_rx.await.expect("late denial armed");

    #[allow(deprecated)]
    let cwd = context.step_context.turn.cwd.clone().into();
    let step = Arc::get_mut(&mut context.step_context).expect("unshared test step");
    let model_info = Arc::make_mut(&mut Arc::make_mut(&mut step.settings).model_info);
    model_info.truncation_policy = codex_protocol::openai_models::TruncationPolicyConfig {
        mode: codex_protocol::openai_models::TruncationMode::Bytes,
        limit: 4,
    };
    spawn_exit_watcher(
        Arc::clone(&process),
        &context,
        vec!["proof".to_string()],
        cwd,
        /*process_id*/ 123,
        /*plugin_attribution*/ None,
        output_buffer,
        Instant::now(),
        Some(network_denial_monitor),
        /*plugin_metrics_sidecar*/ None,
    );

    let exited_at = Instant::now();
    exit_tx.send(0).expect("send exit");
    drop(stdout_tx);

    let event = rx_event.recv().await.expect("command end event");
    let elapsed = Instant::now().saturating_duration_since(exited_at);
    tokio::time::resume();
    let EventMsg::ItemCompleted(completed) = event.msg else {
        panic!("expected ItemCompleted");
    };
    let TurnItem::CommandExecution(item) = completed.item else {
        panic!("expected CommandExecution");
    };
    assert_eq!(
        (
            item.status,
            item.exit_code,
            item.aggregated_output.as_deref()
        ),
        (
            CommandExecutionStatus::Failed,
            Some(-1),
            Some("LATE_DENIAL")
        )
    );
    assert!(
        elapsed >= Duration::from_millis(10) && elapsed < TRAILING_OUTPUT_GRACE,
        "completion should wait for denial without falling back to the output grace: {elapsed:?}"
    );

    Ok(())
}

#[test]
fn utf8_boundary_preserves_complete_characters() {
    assert_eq!(utf8_boundary(b"hello"), 5);

    let bytes = "ééé".as_bytes();
    assert_eq!(utf8_boundary(&bytes[..3]), 2);

    let bytes = "😀".as_bytes();
    assert_eq!(utf8_boundary(bytes), bytes.len());
    for len in 1..bytes.len() {
        assert_eq!(utf8_boundary(&bytes[..len]), 0);
    }
    assert_eq!(utf8_boundary(&[0xf0, 0x9f, 0x98, 0x80, 0xc3]), 4);
}

#[test]
fn utf8_boundary_batches_malformed_output() {
    assert_eq!(utf8_boundary(&[0xff, b'a', b'b']), 3);
    assert_eq!(utf8_boundary(&[0xff, 0xc3]), 1);
    assert_eq!(utf8_boundary(&[0xff, 0xc3, 0xa9]), 3);
    assert_eq!(utf8_boundary(&[0xe0, 0x80]), 2);

    assert_eq!(utf8_boundary(b"a\xffbbb"), 5);
}

#[tokio::test]
async fn streaming_output_bounds_invalid_bytes() {
    let (session, turn, rx_event) = make_session_and_context_with_rx().await;
    let mut output = Buffer::<8> {
        pending: Vec::new(),
        emitter: Emitter {
            remaining_deltas: 2,
            session,
            turn,
            call_id: "bounded-output-test".to_string(),
        },
    };

    // The first frame splits 😀; the last allowed frame leaves é incomplete.
    let bytes = b"\xff\xff\xff\xff\xff\xff\xf0\x9f\x98\x80\xff\xff\xff\xc3\xa9";
    output.push(bytes.to_vec()).await;
    output.push(vec![0xfe, 0xfe]).await;
    output.finish().await;

    let mut chunks = Vec::new();
    while let Ok(event) = rx_event.try_recv() {
        let EventMsg::ExecCommandOutputDelta(delta) = event.msg else {
            panic!("expected ExecCommandOutputDelta");
        };
        chunks.push(delta.chunk);
    }
    assert_eq!(
        chunks,
        vec![
            b"\xff\xff\xff\xff\xff\xff".to_vec(),
            b"\xf0\x9f\x98\x80\xff\xff\xff".to_vec(),
        ]
    );
}

#[tokio::test]
async fn terminate_all_processes_drains_exit_watcher_before_live_thread_shutdown()
-> anyhow::Result<()> {
    let (mut session, turn) = crate::session::tests::make_session_and_context().await;
    let store = Arc::new(codex_thread_store::InMemoryThreadStore::default());
    let thread_store: Arc<dyn codex_thread_store::ThreadStore> = store.clone();
    let config = session.get_config().await;
    let live_thread = codex_thread_store::LiveThread::create(
        Arc::clone(&thread_store),
        codex_thread_store::CreateThreadParams {
            attached_thread: false,
            creator_user_id: None,
            creator_account_id: None,
            session_id: session.session_id(),
            thread_id: session.thread_id,
            extra_config: None,
            forked_from_id: None,
            parent_thread_id: None,
            source: codex_protocol::protocol::SessionSource::Exec,
            thread_source: None,
            originator: "test_originator".to_string(),
            base_instructions: codex_protocol::models::BaseInstructions::default(),
            dynamic_tools: Vec::new(),
            selected_capability_roots: Vec::new(),
            multi_agent_version: None,
            history_mode: codex_protocol::protocol::ThreadHistoryMode::Paginated,
            subagent_history_start_ordinal: None,
            history_base: None,
            initial_window_id: codex_protocol::ContextWindowId::new_uuid_v7(),
            runtime_workspace_roots: None,
            metadata: codex_thread_store::ThreadPersistenceMetadata {
                cwd: Some(config.cwd.to_path_buf()),
                model_provider: config.model_provider_id.clone(),
                memory_mode: codex_protocol::protocol::ThreadMemoryMode::Disabled,
            },
        },
    )
    .await?;
    session.services.thread_store = thread_store;
    session.services.live_thread = Some(live_thread);
    let session = Arc::new(session);

    let (writer_tx, _writer_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let (stdout_tx, stdout_rx) = tokio::sync::broadcast::channel::<Vec<u8>>(8);
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<i32>();
    let exit_tx = Arc::new(std::sync::Mutex::new(Some(exit_tx)));
    let exit_tx_for_term = Arc::clone(&exit_tx);
    let spawned = codex_utils_pty::spawn_from_driver(codex_utils_pty::ProcessDriver {
        writer_tx,
        stdout_rx,
        stderr_rx: None,
        exit_rx,
        terminator: Some(Box::new(move || {
            if let Some(tx) = exit_tx_for_term
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = tx.send(-1);
            }
        })),
        writer_handle: None,
        resizer: None,
        #[cfg(windows)]
        tty: false,
    });
    let process = Arc::new(UnifiedExecProcess::from_spawned(spawned, SandboxType::None).await?);
    let output_buffer = Arc::clone(&process.output_handles().output_buffer);
    let turn = Arc::new(turn);
    let context = UnifiedExecContext::new(
        Arc::clone(&session),
        crate::session::step_context::StepContext::for_test(Arc::clone(&turn)),
        tokio_util::sync::CancellationToken::new(),
        "shutdown-drain-test".to_string(),
    );

    let stream_handle = start_streaming_output(&process, &context);
    session
        .services
        .unified_exec_manager
        .track_watcher(stream_handle)
        .await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone().into();
    let exit_handle = spawn_exit_watcher(
        Arc::clone(&process),
        &context,
        vec!["sleep".to_string(), "10".to_string()],
        cwd,
        /*process_id*/ 4242,
        /*plugin_attribution*/ None,
        output_buffer,
        Instant::now(),
        /*network_denial_monitor*/ None,
        /*plugin_metrics_sidecar*/ None,
    );
    session
        .services
        .unified_exec_manager
        .track_watcher(exit_handle)
        .await;

    // Terminate via the process handle right as shutdown starts, keeping stdout_tx open so
    // start_streaming_output hits TRAILING_OUTPUT_GRACE before notifying output_drained.
    process.terminate();
    let _keep_stdout_open = stdout_tx;

    // Session shutdown runs `terminate_all_processes()` before `live_thread.shutdown()`.
    // `terminate_all_processes()` must drain the watchers before returning so ExecCommandEnd
    // persists while the thread is still open instead of racing into ThreadNotFound.
    session
        .services
        .unified_exec_manager
        .terminate_all_processes()
        .await;
    session
        .live_thread()
        .expect("live thread attached")
        .shutdown()
        .await?;
    let calls = store.calls().await;
    assert_eq!(calls.shutdown_thread, 1);
    assert!(
        calls.append_items >= 1,
        "expected exit watcher event to be persisted before live_thread shutdown, got {calls:?}"
    );
    Ok(())
}
