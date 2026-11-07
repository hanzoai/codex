use super::*;
use std::collections::HashMap;

fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn learned(status: u16, pairs: &[(&str, &str)], now: Instant) -> Budget {
    let headers = headers(pairs);
    let mut budget = Budget::default();
    budget.learn(now, status, |name| headers.get(name).map(String::as_str));
    budget
}

fn near(actual: Duration, expected: Duration) -> bool {
    actual.abs_diff(expected) < Duration::from_millis(5)
}

#[test]
fn a_spent_window_waits_for_its_reset() {
    let now = Instant::now();
    let mut budget = learned(
        200,
        &[
            ("x-ratelimit-limit-requests", "40"),
            ("x-ratelimit-remaining-requests", "0"),
            ("x-ratelimit-reset-requests", "3s"),
        ],
        now,
    );
    assert!(near(budget.take(now), Duration::from_secs(3)));
    // Once the window has reset, nothing it said holds a request back.
    assert_eq!(budget.take(now + Duration::from_secs(4)), Duration::ZERO);
}

#[test]
fn a_full_window_sends_at_once_and_counts_what_it_sends() {
    let now = Instant::now();
    let mut budget = learned(
        200,
        &[
            ("x-ratelimit-limit-requests", "40"),
            ("x-ratelimit-remaining-requests", "11"),
            ("x-ratelimit-reset-requests", "30s"),
        ],
        now,
    );
    assert_eq!(budget.take(now), Duration::ZERO);
    assert_eq!(budget.requests.map(|window| window.remaining), Some(10));
    // At a quarter of the limit, the rest is spread over what is left of the window.
    assert!(near(budget.take(now), Duration::from_secs(3)));
}

#[test]
fn retry_after_on_a_429_or_503_holds_every_request() {
    let now = Instant::now();
    let mut budget = learned(429, &[("retry-after", "2")], now);
    assert!(near(budget.take(now), Duration::from_secs(2)));
    let mut budget = learned(503, &[("retry-after", "1.5")], now);
    assert!(near(budget.take(now), Duration::from_millis(1500)));
    // A success's Retry-After is not advice to wait.
    let mut budget = learned(200, &[("retry-after", "9")], now);
    assert_eq!(budget.take(now), Duration::ZERO);
}

#[test]
fn spent_tokens_wait_for_their_reset() {
    let now = Instant::now();
    let mut budget = learned(
        200,
        &[
            ("x-ratelimit-limit-tokens", "100000"),
            ("x-ratelimit-remaining-tokens", "1000"),
            ("x-ratelimit-reset-tokens", "6m0s"),
        ],
        now,
    );
    assert!(near(budget.take(now), Duration::from_secs(360)));
}

#[test]
fn resets_read_as_durations_seconds_or_moments() {
    assert_eq!(reset("6m0s"), Some(Duration::from_secs(360)));
    assert_eq!(reset("20ms"), Some(Duration::from_millis(20)));
    assert_eq!(reset("1h2m3.5s"), Some(Duration::from_secs_f64(3723.5)));
    assert_eq!(reset("30"), Some(Duration::from_secs(30)));
    assert_eq!(reset("0.5"), Some(Duration::from_millis(500)));
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let moment = reset(&(unix + 10).to_string()).unwrap();
    assert!(moment <= Duration::from_secs(10) && moment >= Duration::from_secs(8));
    assert_eq!(reset("soon"), None);
    assert_eq!(reset("5x"), None);
    assert_eq!(reset(""), None);
}

#[test]
fn a_budget_belongs_to_the_origin() {
    assert_eq!(
        origin("https://api.hanzo.ai/v1/responses"),
        "https://api.hanzo.ai"
    );
    assert_eq!(origin("http://127.0.0.1:8080/v1"), "http://127.0.0.1:8080");
    assert_eq!(origin("http://host"), "http://host");
    let url = "http://pace-test.invalid:1/v1/responses";
    observe(url, 429, |name| (name == "retry-after").then_some("2"));
    assert!(near(
        delay("http://pace-test.invalid:1/v1/models"),
        Duration::from_secs(2)
    ));
    assert_eq!(delay("http://other.invalid/v1/responses"), Duration::ZERO);
}
