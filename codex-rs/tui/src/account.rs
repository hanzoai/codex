//! The account behind /login, /logout, /auth and /status, when the binary has one.
//!
//! Upstream signs in through the app server alone. A binary with an account of its
//! own registers it here before the TUI starts. With none registered, /login and
//! /auth are not offered, and /logout and /status do what upstream's do.

use std::sync::OnceLock;

/// What the account commands run; each line the account prints goes to the
/// interface through the `emit` it is given.
#[derive(Clone, Copy)]
pub struct Account {
    /// Sign in. It may run beside the interface, so `emit` outlives the call.
    pub login: fn(Box<dyn Fn(String) + Send + Sync>),
    /// Sign out, ahead of the app server's own sign-out.
    pub logout: fn(&mut dyn FnMut(String)),
    /// Say who is signed in.
    pub show: fn(&mut dyn FnMut(String)),
}

static ACCOUNT: OnceLock<Account> = OnceLock::new();

/// Register the binary's account. The first registration holds.
pub fn register(account: Account) {
    let _ = ACCOUNT.set(account);
}

pub(crate) fn registered() -> bool {
    ACCOUNT.get().is_some()
}

pub(crate) fn login(emit: Box<dyn Fn(String) + Send + Sync>) {
    if let Some(account) = ACCOUNT.get() {
        (account.login)(emit);
    }
}

pub(crate) fn logout(emit: &mut dyn FnMut(String)) {
    if let Some(account) = ACCOUNT.get() {
        (account.logout)(emit);
    }
}

pub(crate) fn show(emit: &mut dyn FnMut(String)) {
    if let Some(account) = ACCOUNT.get() {
        (account.show)(emit);
    }
}
