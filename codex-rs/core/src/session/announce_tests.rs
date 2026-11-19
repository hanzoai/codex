use super::*;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TurnInputRequest;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::sync::Arc;

/// The message enso-auto ended a `dev exec` turn on in hanzo-inc/cloud#759.
const ANNOUNCED: &str = "Now I have the base hash. Applying all 5 config.toml edits.";

fn item_ids() -> ItemIdGenerator {
    Arc::new(|prefix: &str| codex_protocol::ResponseItemId::with_suffix(prefix, "ask"))
}

async fn reply(server: &wiremock::MockServer, id: &str, text: &str) -> ResponseMock {
    mount_sse_once(
        server,
        sse(vec![
            ev_response_created(id),
            ev_assistant_message(&format!("msg-{id}"), text),
            ev_completed(id),
        ]),
    )
    .await
}

/// Runs one turn on `model` and returns the agent message it completed with.
async fn turn(server: &wiremock::MockServer, model: &str) -> anyhow::Result<Option<String>> {
    let test = test_codex().with_model(model).build(server).await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Set the five keys in ~/.hanzo/dev/config.toml.".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let EventMsg::TurnComplete(done) =
        wait_for_event(&test.codex, |event| matches!(event, EventMsg::TurnComplete(_))).await
    else {
        unreachable!("waited for TurnComplete");
    };
    Ok(done.last_agent_message)
}

fn asked(mock: &ResponseMock) -> bool {
    mock.single_request()
        .message_input_texts("user")
        .iter()
        .any(|text| text.contains(hanzo::announce::FINISH))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hanzo_turn_that_ends_on_an_announcement_is_asked_to_make_the_call()
-> anyhow::Result<()> {
    let server = start_mock_server().await;
    let first = reply(&server, "resp-1", ANNOUNCED).await;
    let second = reply(&server, "resp-2", "All five keys are set in config.toml.").await;

    let last = turn(&server, "enso-auto").await?;

    assert!(!asked(&first), "the first request carries no ask");
    assert!(asked(&second), "the turn continued with the ask");
    assert_eq!(last.as_deref(), Some("All five keys are set in config.toml."));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_announcement_that_answers_the_ask_ends_the_turn() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let first = reply(&server, "resp-1", ANNOUNCED).await;
    let second = reply(&server, "resp-2", "Running both doctors now.").await;

    let last = turn(&server, "enso-auto").await?;

    first.single_request();
    assert!(asked(&second));
    assert_eq!(server.received_requests().await.map(|r| r.len()), Some(2));
    assert_eq!(last.as_deref(), Some("Running both doctors now."));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_final_answer_on_hanzo_ends_the_turn() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let only = reply(&server, "resp-1", "All five keys are set in config.toml.").await;

    let last = turn(&server, "enso-auto").await?;

    assert!(!asked(&only));
    assert_eq!(server.received_requests().await.map(|r| r.len()), Some(1));
    assert_eq!(last.as_deref(), Some("All five keys are set in config.toml."));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_off_hanzo_ends_on_an_announcement_as_upstream_does() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let only = reply(&server, "resp-1", ANNOUNCED).await;

    let last = turn(&server, "gpt-5.5").await?;

    assert!(!asked(&only));
    assert_eq!(server.received_requests().await.map(|r| r.len()), Some(1));
    assert_eq!(last.as_deref(), Some(ANNOUNCED));
    Ok(())
}

#[test]
fn one_ask_in_a_row_and_another_after_a_tool_call() {
    let ids = item_ids();
    let mut announce = Announce::default();

    let ask = announce.ask(true, Some(ANNOUNCED), &ids);
    let Some(ResponseItem::Message { role, content, .. }) = ask else {
        panic!("an announcement is asked about: {ask:?}");
    };
    assert_eq!(role, "user");
    let parsed = codex_protocol::items::parse_hook_prompt_message(None, &content)
        .expect("the ask is recorded as a hook prompt");
    assert_eq!(parsed.fragments.len(), 1);
    assert_eq!(parsed.fragments[0].text, hanzo::announce::FINISH);
    assert_eq!(parsed.fragments[0].hook_run_id, ASK_RUN_ID);

    assert!(announce.ask(true, Some(ANNOUNCED), &ids).is_none());
    announce.acted();
    assert!(announce.ask(true, Some(ANNOUNCED), &ids).is_some());
}

#[test]
fn nothing_is_asked_of_a_final_answer_an_empty_turn_or_a_turn_it_does_not_apply_to() {
    let ids = item_ids();
    let mut announce = Announce::default();
    assert!(announce.ask(true, Some("All five keys are set."), &ids).is_none());
    assert!(announce.ask(true, None, &ids).is_none());
    assert!(announce.ask(false, Some(ANNOUNCED), &ids).is_none());
    assert!(announce.ask(true, Some(ANNOUNCED), &ids).is_some());
}

#[test]
fn it_applies_to_turns_on_hanzo_that_do_work() {
    let exec = SessionSource::Exec;
    let default = ModeKind::Default;
    assert!(applies("hanzo", None, "fable", &exec, default));
    assert!(applies("custom", Some("https://api.hanzo.ai/v1"), "opus", &exec, default));
    assert!(applies("openai", None, "zen6-coder", &SessionSource::Cli, default));
    assert!(applies(
        "hanzo",
        None,
        "enso-auto",
        &SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: codex_protocol::ThreadId::new(),
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        }),
        default
    ));

    assert!(!applies("openai", Some("https://api.openai.com/v1"), "gpt-5.5", &exec, default));
    assert!(!applies("hanzo", None, "enso-auto", &exec, ModeKind::Plan));
    for source in [
        SessionSource::Internal(InternalSessionSource::MemoryConsolidation),
        SessionSource::Internal(InternalSessionSource::Guardian),
        SessionSource::SubAgent(SubAgentSource::Review),
        SessionSource::SubAgent(SubAgentSource::Compact),
    ] {
        assert!(!applies("hanzo", None, "enso-auto", &source, default), "{source:?}");
    }
}
