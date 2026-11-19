//! Whether a model's last message announced its next step instead of finishing
//! (hanzo-inc/cloud#759).
//!
//! A turn ends when the model answers without a tool call. Some models answer a tool
//! error with a line that only says what they will do next ("Now I have the base hash.
//! Applying all 5 config.toml edits.") and no call, so the turn ended there and
//! `dev exec` reported the task complete with nothing done. The turn loop asks such a
//! turn, once, to make the call it announced ([`FINISH`]).
//!
//! The reading is narrow on purpose: a final answer read as an announcement costs a
//! model call and a second answer. It reads the message's last line, and that line's
//! last sentence, for:
//!
//! - a closing colon ("Now let me run the tests:");
//! - a first-person announcement ("Let me…", "Now I'll…", "I'm going to…"), but not
//!   "Let me know…", which closes a report;
//! - a verbless step led by an action ("Applying all 5 config.toml edits.",
//!   "Running both doctors now."), but not a report whose subject is one ("Running the
//!   tests shows two failures."), and not a list item, which is how a summary lists
//!   what was done.
//!
//! A question, or a line that waits on the person ("…once you confirm", "your
//! approval"), is never an announcement. Cloud's coding harness asks the same of the
//! runs it drives (`apps/coding/outcome.go`, `narrating`); this is the reading for the
//! turns Dev runs on Hanzo.

/// What a turn that ended on an announcement is told before it continues.
pub const FINISH: &str = "Your last message announced a next step but made no tool call, so nothing ran \
and the task is not finished. If that step is still needed, make the tool call now and keep going until \
the task is done. If the task is already done, reply with your final answer.";

/// First-person announcements, read at the start of a line or of its last sentence,
/// after one filler word ("Great, let me…", "Now I'll…").
const ANNOUNCING: &[&str] = &[
    "let me ",
    "let's ",
    "i'll now ",
    "i will now ",
    "i'm now ",
    "i am now ",
    "i'm going to ",
    "i am going to ",
    "i'm about to ",
    "i am about to ",
    "about to ",
    "time to ",
];

/// A filler word that may open an announcement or a step: the lead is the next word.
/// "Now I'll", "Next, I will" and "Then I'll" announce; a bare "I'll" may close a report
/// ("I'll leave the rest as is."), so it counts only after one of these.
const FILLERS: &[&str] = &[
    "now", "next", "then", "so", "ok", "okay", "alright", "great", "good", "perfect", "first",
    "finally",
];

/// What a filler makes an announcement of: "Now I'll…", "Next I will…".
const AFTER_FILLER: &[&str] = &["i'll ", "i will "];

/// Openings that close a report rather than announce a step.
const CLOSING: &[&str] = &["let me know", "let's discuss", "let me be clear"];

/// Phrases that wait on the person: the turn should end there.
const WAITING: &[&str] = &[
    "if you",
    "once you",
    "when you",
    "until you",
    "unless you",
    "for you to",
    "you want",
    "you'd like",
    "you would like",
    "you prefer",
    "your confirmation",
    "your approval",
    "your go-ahead",
    "your input",
    "your answer",
    "your decision",
    "your call",
    "your permission",
];

/// Gerunds that lead a step ("Applying…", "Running…").
const ACTIONS: &[&str] = &[
    "adding",
    "applying",
    "building",
    "checking",
    "cleaning",
    "cloning",
    "committing",
    "compiling",
    "continuing",
    "creating",
    "debugging",
    "deleting",
    "deploying",
    "downloading",
    "editing",
    "examining",
    "executing",
    "fetching",
    "fixing",
    "generating",
    "implementing",
    "inspecting",
    "installing",
    "investigating",
    "launching",
    "looking",
    "moving",
    "opening",
    "patching",
    "polling",
    "proceeding",
    "pushing",
    "re-running",
    "reading",
    "refactoring",
    "removing",
    "renaming",
    "rerunning",
    "restarting",
    "retrying",
    "reviewing",
    "running",
    "searching",
    "starting",
    "switching",
    "testing",
    "trying",
    "updating",
    "verifying",
    "writing",
];

/// Words that give a gerund-led sentence a verb of its own, which makes it a report
/// ("Running the tests shows two failures.") rather than a step.
const FINITE: &[&str] = &[
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "has",
    "have",
    "had",
    "does",
    "did",
    "do",
    "will",
    "would",
    "can",
    "could",
    "should",
    "may",
    "might",
    "must",
    "shows",
    "showed",
    "pass",
    "passes",
    "passed",
    "fails",
    "failed",
    "works",
    "worked",
    "fixes",
    "fixed",
    "made",
    "makes",
    "keeps",
    "kept",
    "means",
    "meant",
    "returns",
    "returned",
    "gives",
    "gave",
    "avoids",
    "avoided",
    "causes",
    "caused",
    "confirms",
    "confirmed",
    "breaks",
    "broke",
    "needs",
    "needed",
    "takes",
    "took",
    "succeeds",
    "succeeded",
    "produces",
    "produced",
    "leaves",
    "left",
    "ensures",
    "ensured",
    "matches",
    "matched",
    "resolves",
    "resolved",
    "solves",
    "solved",
    "requires",
    "required",
    "found",
    "finds",
    "helps",
    "helped",
    "reports",
    "reported",
    "prints",
    "printed",
    "says",
    "said",
    "won't",
    "isn't",
    "wasn't",
    "doesn't",
    "didn't",
    "can't",
];

/// A verbless step is short: a longer gerund-led sentence is read as prose.
const STEP_WORDS: usize = 16;

/// Whether `message`, the model's last message in a turn that made no tool call, only
/// announced the next step.
pub fn announces(message: &str) -> bool {
    let text = message.trim();
    if text.is_empty() {
        return false;
    }
    let line = last_line(text);
    let sentence = last_sentence(line);
    if waits(&plain(line)) {
        return false;
    }
    if line.trim_end_matches(['*', '_', '`', ' ']).ends_with(':') {
        return true;
    }
    if [line, sentence]
        .into_iter()
        .any(|part| announcing(&plain(part)))
    {
        return true;
    }
    !listed(line) && step(&plain(sentence))
}

/// "let me run the tests.", "now i'll apply the edits.", "great, let's run it."
fn announcing(plain: &str) -> bool {
    if plain.ends_with('?') || CLOSING.iter().any(|closing| plain.starts_with(closing)) {
        return false;
    }
    if ANNOUNCING.iter().any(|lead| plain.starts_with(lead)) {
        return true;
    }
    let (filler, rest) = split_lead(plain);
    FILLERS.contains(&filler)
        && !CLOSING.iter().any(|closing| rest.starts_with(closing))
        && ANNOUNCING
            .iter()
            .chain(AFTER_FILLER)
            .any(|lead| rest.starts_with(lead))
}

/// A verbless sentence led by an action: "applying all 5 config.toml edits.",
/// "running both doctors now.", "i'm running both doctors now."
fn step(sentence: &str) -> bool {
    if sentence.ends_with('?') {
        return false;
    }
    let words: Vec<&str> = sentence
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-'))
        .filter(|word| !word.is_empty())
        .collect();
    let mut at = usize::from(words.first().is_some_and(|word| FILLERS.contains(word)));
    if words.get(at) == Some(&"i'm") {
        at += 1;
    }
    let Some(lead) = words.get(at) else {
        return false;
    };
    ACTIONS.contains(lead)
        && words.len() <= STEP_WORDS
        && !words[at + 1..].iter().any(|word| FINITE.contains(word))
}

/// Whether the text waits on the person.
fn waits(plain: &str) -> bool {
    WAITING.iter().any(|phrase| plain.contains(phrase))
}

/// Whether the line is a list item ("- Added…", "2. Running…").
fn listed(line: &str) -> bool {
    let line = line.trim_start();
    if line.starts_with(['-', '*', '•', '+']) {
        return true;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(['.', ')'])
}

/// The first word, without the comma that may follow it, and the rest.
fn split_lead(plain: &str) -> (&str, &str) {
    match plain.split_once(' ') {
        Some((first, rest)) => (first.trim_end_matches([',', ':']), rest.trim_start()),
        None => (plain.trim_end_matches(['.', ',', '!']), ""),
    }
}

/// The last line with text on it.
fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(text)
}

/// The line's last sentence: what follows the last `.`, `!` or `?` that a space follows.
/// "config.toml" stays whole; "Now I have the hash. Applying the edits." ends in
/// "Applying the edits.".
fn last_sentence(line: &str) -> &str {
    let body = line.trim_end_matches(['.', '!', '?', '…', ' ']);
    let bytes = body.as_bytes();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(byte, b'.' | b'!' | b'?') && bytes.get(index + 1) == Some(&b' ') {
            start = index + 1;
        }
    }
    line[start..].trim()
}

/// Lowercased, without the list marker, emphasis or quotes a line may open with, and
/// with code spans unwrapped.
fn plain(text: &str) -> String {
    text.trim_start_matches(|c: char| {
        matches!(
            c,
            '-' | '*' | '•' | '+' | '>' | '#' | '_' | '`' | '"' | ' ' | ')' | '.'
        ) || c.is_ascii_digit()
    })
    .replace('`', "")
    .replace('’', "'")
    .to_lowercase()
}

#[cfg(test)]
#[path = "announce_tests.rs"]
mod tests;
