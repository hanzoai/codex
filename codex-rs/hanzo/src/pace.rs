//! Pacing requests to a provider by the rate limit it reports, so a request is never sent
//! into a limit the provider has already said is spent.
//!
//! Every response can carry the OpenAI-style headers `x-ratelimit-{limit,remaining,reset}-
//! {requests,tokens}`, and a 429 or 503 can carry `Retry-After`. [`observe`] keeps the latest
//! of them per origin (scheme, host and port: one provider, every thread and agent of this
//! process); [`delay`] says how long to wait before the next request there, and counts that
//! request against what remains. The budget is shared by everyone the provider counts
//! together, so the wait is a floor the server's own answer replaces on every response.
//!
//! - Spent: no requests (or tokens) remain before the window resets, so wait for the reset.
//! - Low: a quarter or less of the requests remain, so spread them over what is left of the
//!   window (one per `left / remaining`), instead of spending them at once and stalling.
//! - Told: a `Retry-After` on a 429 or 503 holds every request until it passes.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// What one provider has said about its limits.
#[derive(Clone, Debug, Default, PartialEq)]
struct Budget {
    requests: Option<Window>,
    tokens: Option<Window>,
    /// No request before this (`Retry-After`).
    until: Option<Instant>,
}

/// One limit: how much of it is left until it resets.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Window {
    limit: Option<u64>,
    remaining: u64,
    reset: Instant,
}

static BUDGETS: LazyLock<Mutex<HashMap<String, Budget>>> = LazyLock::new(Default::default);

/// How long to wait before sending a request to `url`. The request is counted against the
/// provider's remaining budget, so requests made at once space themselves out.
pub fn delay(url: &str) -> Duration {
    let mut budgets = BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    budgets
        .get_mut(origin(url))
        .map(|budget| budget.take(Instant::now()))
        .unwrap_or_default()
}

/// Learns from a response from `url` with `status`, reading its headers through `header`
/// (a header's value by its lower-case name).
pub fn observe<'a>(url: &str, status: u16, header: impl Fn(&str) -> Option<&'a str>) {
    let now = Instant::now();
    let mut budgets = BUDGETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budget = budgets.entry(origin(url).to_string()).or_default();
    budget.learn(now, status, header);
}

impl Budget {
    fn learn<'a>(&mut self, now: Instant, status: u16, header: impl Fn(&str) -> Option<&'a str>) {
        if let Some(window) = Window::read(now, "requests", &header) {
            self.requests = Some(window);
        }
        if let Some(window) = Window::read(now, "tokens", &header) {
            self.tokens = Some(window);
        }
        if matches!(status, 429 | 502 | 503 | 504 | 520..=529)
            && let Some(wait) = header("retry-after").and_then(seconds)
        {
            self.until = Some(now + wait);
        }
    }

    /// The wait before one more request at `now`, which is then counted.
    fn take(&mut self, now: Instant) -> Duration {
        let mut wait = self
            .until
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(now));
        if let Some(window) = self.requests.as_mut().filter(|window| window.reset > now) {
            let left = window.reset - now;
            let low = window
                .limit
                .map_or(window.remaining <= 2, |limit| window.remaining * 4 <= limit);
            if window.remaining == 0 {
                wait = wait.max(left);
            } else if low {
                wait = wait.max(left / u32::try_from(window.remaining).unwrap_or(u32::MAX));
            }
            window.remaining = window.remaining.saturating_sub(1);
        }
        if let Some(window) = self.tokens.filter(|window| window.reset > now) {
            let spent = window.remaining == 0
                || window
                    .limit
                    .is_some_and(|limit| window.remaining * 20 <= limit);
            if spent {
                wait = wait.max(window.reset - now);
            }
        }
        wait
    }
}

impl Window {
    fn read<'a>(
        now: Instant,
        kind: &str,
        header: &impl Fn(&str) -> Option<&'a str>,
    ) -> Option<Self> {
        let remaining = header(&format!("x-ratelimit-remaining-{kind}"))?
            .trim()
            .parse()
            .ok()?;
        let reset = reset(header(&format!("x-ratelimit-reset-{kind}"))?)?;
        let limit = header(&format!("x-ratelimit-limit-{kind}"))
            .and_then(|value| value.trim().parse().ok());
        Some(Self {
            limit,
            remaining,
            reset: now + reset,
        })
    }
}

/// `scheme://host:port` of `url`: the provider a budget belongs to.
fn origin(url: &str) -> &str {
    let rest = url.find("://").map_or(0, |at| at + 3);
    let end = url[rest..].find('/').map_or(url.len(), |at| rest + at);
    &url[..end]
}

/// A reset header: a duration (`1s`, `6m0s`, `20ms`, `1h2m3.5s`), seconds (`30`, `0.5`), or
/// Unix seconds when the number is a moment rather than a span.
fn reset(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Some(span) = seconds(value) {
        let unix = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
        // A billion seconds is 2001: a value past it names a moment, not a span.
        return Some(if span.as_secs() > 1_000_000_000 {
            span.saturating_sub(unix)
        } else {
            span
        });
    }
    span(value)
}

/// Plain seconds, whole or fractional.
fn seconds(value: &str) -> Option<Duration> {
    let value: f64 = value.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| Duration::from_secs_f64(value))
}

/// A duration written as numbers each followed by `h`, `m`, `s` or `ms`.
fn span(value: &str) -> Option<Duration> {
    let mut total = Duration::ZERO;
    let mut rest = value;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .filter(|at| *at > 0)?;
        let number: f64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let (unit, len) = if rest.starts_with("ms") {
            (0.001, 2)
        } else if rest.starts_with('h') {
            (3600.0, 1)
        } else if rest.starts_with('m') {
            (60.0, 1)
        } else if rest.starts_with('s') {
            (1.0, 1)
        } else {
            return None;
        };
        total += Duration::from_secs_f64(number * unit);
        rest = &rest[len..];
    }
    (!value.is_empty()).then_some(total)
}

#[cfg(test)]
#[path = "pace_tests.rs"]
mod tests;
