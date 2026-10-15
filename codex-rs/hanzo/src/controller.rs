//! What a controller decides inside the agent loop, as data the loop applies.
//!
//! The loop asks at three points and applies the answer: which tools the model sees directly
//! this step, whether a command may run, and which routing hints ride the turn's requests. A
//! controller installed on the thread answers. Nothing it says can loosen what the loop already
//! decided: a verdict joins the policy's on `allow < ask < deny`, a shortlist only moves tools
//! out of direct view, and a hint is metadata the gateway may read.
//!
//! The loop finds the controller as a [`Handle`] in the thread's extension data, where an
//! extension the binary registers (codex-app-server's `register`) puts it; with none, the
//! loop runs as upstream's does.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A controller's answer, computed asynchronously.
pub type Answer<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Whether an action may proceed. The order is the lattice: `Allow < Ask < Deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verdict {
    Allow,
    Ask,
    Deny,
}

impl Verdict {
    /// The stricter of the two.
    pub fn join(self, other: Verdict) -> Verdict {
        self.max(other)
    }

    pub fn name(self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Ask => "ask",
            Verdict::Deny => "deny",
        }
    }
}

/// A tool on offer: its model-visible name and what it says it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub name: String,
    pub description: String,
}

/// A turn's request as the loop received it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// The user's text.
    pub text: String,
    /// Whether it carries images.
    pub images: bool,
}

pub trait Controller: Send + Sync {
    /// Of `cards`, the tools to keep in direct view for a step of `turn`; `None` keeps all.
    fn tools<'a>(&'a self, turn: &'a str, cards: Vec<Card>) -> Answer<'a, Option<HashSet<String>>>;

    /// The verdict for the command `call` in `turn` would run, which the policy rates
    /// `policy`. `action` is the command as the approval path describes it.
    fn command<'a>(
        &'a self,
        turn: &'a str,
        call: &'a str,
        policy: Verdict,
        action: serde_json::Value,
    ) -> Answer<'a, Verdict>;

    /// Routing hints for `turn`'s model requests.
    fn route<'a>(&'a self, turn: &'a str, request: Request)
    -> Answer<'a, BTreeMap<String, String>>;

    /// Compact transcript history, pruning stale tool outputs while preserving error diagnostics verbatim.
    fn compact<'a>(
        &'a self,
        _turn: &'a str,
        _messages: Vec<serde_json::Value>,
    ) -> Answer<'a, Option<Vec<serde_json::Value>>> {
        Box::pin(async { None })
    }

    /// Decides or verifies a typed Action for `turn`, returning Some(action) to execute directly,
    /// or None to fall back to unconstrained generative model planning.
    fn action<'a>(
        &'a self,
        _turn: &'a str,
        _state: serde_json::Value,
    ) -> Answer<'a, Option<serde_json::Value>> {
        Box::pin(async { None })
    }
}

/// The controller installed on a thread.
#[derive(Clone)]
pub struct Handle(pub Arc<dyn Controller>);

impl Handle {
    /// Decides a typed Action: fast-path or Kai selected action.
    pub async fn action(&self, turn: &str, state: serde_json::Value) -> Option<serde_json::Value> {
        self.0.action(turn, state).await
    }
    /// The command's verdict: the policy's joined with the controller's, never looser.
    pub async fn command(
        &self,
        turn: &str,
        call: &str,
        policy: Verdict,
        action: serde_json::Value,
    ) -> Verdict {
        self.0
            .command(turn, call, policy, action)
            .await
            .join(policy)
    }

    pub async fn tools(&self, turn: &str, cards: Vec<Card>) -> Option<Shortlist> {
        self.0.tools(turn, cards).await.map(Shortlist)
    }

    pub async fn route(&self, turn: &str, request: Request) -> BTreeMap<String, String> {
        self.0.route(turn, request).await
    }

    pub async fn compact(&self, turn: &str, messages: Vec<serde_json::Value>) -> Option<Vec<serde_json::Value>> {
        self.0.compact(turn, messages).await
    }
}

/// A step's shortlist: the tools that stay in direct view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortlist(pub HashSet<String>);

impl Shortlist {
    pub fn keeps(&self, tool: &str) -> bool {
        self.0.contains(tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Loose;

    impl Controller for Loose {
        fn tools<'a>(&'a self, _: &'a str, _: Vec<Card>) -> Answer<'a, Option<HashSet<String>>> {
            Box::pin(async { None })
        }
        fn command<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: Verdict,
            _: serde_json::Value,
        ) -> Answer<'a, Verdict> {
            Box::pin(async { Verdict::Allow })
        }
        fn route<'a>(&'a self, _: &'a str, _: Request) -> Answer<'a, BTreeMap<String, String>> {
            Box::pin(async { BTreeMap::new() })
        }
    }

    #[test]
    fn the_lattice_orders_allow_ask_deny() {
        assert_eq!(Verdict::Allow.join(Verdict::Ask), Verdict::Ask);
        assert_eq!(Verdict::Deny.join(Verdict::Ask), Verdict::Deny);
        assert_eq!(Verdict::Allow.join(Verdict::Allow), Verdict::Allow);
    }

    #[tokio::test]
    async fn a_controller_that_allows_cannot_loosen_the_policy() {
        let handle = Handle(Arc::new(Loose));
        for policy in [Verdict::Allow, Verdict::Ask, Verdict::Deny] {
            let verdict = handle
                .command("t", "c", policy, serde_json::Value::Null)
                .await;
            assert_eq!(verdict, policy);
        }
    }

    #[tokio::test]
    async fn a_controller_default_compact_returns_none() {
        let handle = Handle(Arc::new(Loose));
        let res = handle.compact("t", vec![]).await;
        assert_eq!(res, None);
    }
}
