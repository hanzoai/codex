use super::*;

/// The transitional messages from hanzo-inc/cloud#759 (session 01a1c049, 9 November) and
/// #764, each the last message of a turn that made no tool call.
#[test]
fn the_reported_announcements_are_read_as_announcements() {
    for message in [
        "Now I have the base hash. Applying all 5 config.toml edits.",
        "All fixes applied. The doctor command failed only because I passed \
         TERM=xterm-256color as an arg instead of env. Running both doctors now.",
        "Now let me run the receipts test.",
        "The tiles endpoint returned successfully but no receipts are filed this month.\n\n\
         Now let me run the tests:",
    ] {
        assert!(announces(message), "{message:?} announces its next step");
    }
}

#[test]
fn first_person_announcements_are_announcements() {
    for message in [
        "Let me check the logs.",
        "Let's run the suite again.",
        "Great, let me apply the fix.",
        "Now I'll update the config.",
        "Next, I will rerun the build.",
        "Then I'll commit the change.",
        "The build failed on a missing import. I'm going to add it and rebuild.",
        "I'll now apply the patch.",
        "Go is being installed (downloading go1.27.1). Let me wait for it to complete by running again.",
        "- Let me look at `apps/coding/outcome.go`.",
    ] {
        assert!(announces(message), "{message:?} announces its next step");
    }
}

#[test]
fn verbless_steps_led_by_an_action_are_announcements() {
    for message in [
        "Applying the remaining edits.",
        "Running `cargo test -p hanzo` now.",
        "Now running the tests.",
        "OK, checking the other call sites…",
        "I'm running both doctors now.",
        "Proceeding with the migration.",
    ] {
        assert!(announces(message), "{message:?} announces its next step");
    }
}

#[test]
fn final_answers_are_not_announcements() {
    for message in [
        "All five edits are applied and both doctors pass.",
        "Updated `config.toml` in 5 places; `dev doctor` is clean.",
        "Running the tests shows two failures in `parser_tests.rs`, both from the old fixture.",
        "Testing: `cargo test -p hanzo` passes.",
        "Done. The bug was an off-by-one in `last_sentence`.",
        "I'll leave the generated files as they are.",
        "Let me know if you want the docs updated too.",
        "Let me know if anything else breaks.",
        "Here's what changed:\n\n- Added a test for the empty case\n- Fixed the window rounding",
        "Summary:\n- Adding the index fixed the slow query\n- Updating the lockfile was not needed",
        "Changes:\n1. Added `announce.rs`\n2. Running the suite passes",
        "",
        "   ",
    ] {
        assert!(!announces(message), "{message:?} is a final answer");
    }
}

#[test]
fn a_summary_list_item_is_not_a_step() {
    for message in [
        "What I did:\n- Applying the patch to both services\n- Adding tests for the parser",
        "Steps taken:\n2. Running the migration on staging",
    ] {
        assert!(
            !announces(message),
            "{message:?} lists work, it does not announce it"
        );
    }
}

#[test]
fn questions_and_waiting_on_the_person_are_not_announcements() {
    for message in [
        "Should I apply the same change to the other services?",
        "Let me know which option you prefer?",
        "Shall I run the migration now?",
        "I'm going to wait for your approval before deleting the branch.",
        "Deleting the old rows needs care. I'll proceed once you confirm.",
        "Let me run it on production if you want.",
    ] {
        assert!(!announces(message), "{message:?} waits on the person");
    }
}

#[test]
fn a_long_gerund_led_sentence_is_prose() {
    let message = "Adding a retry around the upload, with jitter and a cap on the total wait, \
                   and logging each attempt at debug level so the trace shows them.";
    assert!(!announces(message));
}

#[test]
fn the_last_sentence_is_split_on_a_stop_that_a_space_follows() {
    assert_eq!(
        last_sentence("Now I have the base hash. Applying all 5 config.toml edits."),
        "Applying all 5 config.toml edits."
    );
    assert_eq!(last_sentence("e.g. this is one"), "this is one");
    assert_eq!(last_sentence("One sentence only."), "One sentence only.");
    assert_eq!(last_sentence("Wait… "), "Wait…");
}
