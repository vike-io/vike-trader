//! **The token plumbing the bearer-token venues share** (alpaca, ctrader, ig): when a token is due for a PROACTIVE
//! refresh ([`token_refresh_due`], the venue's margin as a parameter), and the ONE re-authentication ladder
//! ([`TokenCell::reauth_once`], ig's): on the venue's "token dead" status, one re-login, then exactly one retry, never a
//! loop. A generation counter makes a SHARED token safe: two threads that both see the dead status produce ONE login,
//! because the second finds the generation already moved and retries with the token the first fetched.
//!
//! What a token IS (an access token, ig's `CST` and `X-SECURITY-TOKEN` pair), how a venue logs in, which status means
//! "dead" (`401`) and the words a venue logs are the venue's: parameters here, and the log lines are the caller's
//! ([`TokenReauthNote`]), so each keeps its own target. What a failed re-login answers is the venue's too
//! ([`TokenReauthFailedLogin`]).
//!
//! A mint that produced no token is ONE error, [`TokenMintError`], by what it says about the credentials: refused by
//! the venue, answered with no token, or never reached (ctrader's OAuth). Its words are one wording for every venue
//! (ruling U-7).

use std::sync::Mutex;

/// A token mint (an OAuth code exchange, a refresh, a client-credentials grant) that produced no token, by what it
/// says about the credentials. It never carries a secret: a venue builds it from the answer's words, and an
/// unreachable mint's words are the transport's (a venue whose mint URL embeds a secret keeps those out of its lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenMintError {
    /// The mint answered and refused: the venue's own code and words (ctrader's `{errorCode, description}`). This one
    /// IS about the credentials.
    Refused { code: String, words: String },
    /// The mint answered something that is no token: a body that does not parse, or one missing a field a token needs.
    Malformed(String),
    /// The mint was never reached, or its answer never came: it says NOTHING about the credentials.
    Unreachable(String),
}

impl std::fmt::Display for TokenMintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { code, words } => write!(f, "token mint refused {code}: {words}"),
            Self::Malformed(why) => write!(f, "malformed token response: {why}"),
            Self::Unreachable(why) => write!(f, "token mint unreachable: {why}"),
        }
    }
}

impl std::error::Error for TokenMintError {}

/// Is a token expiring at `expires_at_ms` due for a PROACTIVE refresh at `now_ms`, `margin_ms` ahead of its expiry?
/// `None` (an expiry the venue never stated) is NOT due: it leaves the reactive path (the dead status) alone rather
/// than refreshing on every call for ever. ctrader's margin is 5 min, alpaca's 60 s.
pub fn token_refresh_due(expires_at_ms: Option<i64>, now_ms: i64, margin_ms: i64) -> bool {
    expires_at_ms.is_some_and(|at| now_ms >= at.saturating_sub(margin_ms))
}

/// A token that expires, and the generation each successful (re-)login bumps (`1` after the first login).
pub struct TokenCell<T> {
    held: Mutex<(T, u64)>,
}

/// What the ladder has to say, for the caller's own log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenReauthNote {
    /// The venue answered the dead status: re-logging in, then retrying ONCE.
    Reauthenticating,
    /// The venue answered the dead status again on a FRESH token: the credentials are refused, not expiring, and
    /// nothing retries further.
    RefusedAgain,
}

/// What [`TokenCell::reauth_once`] answers when the re-login itself fails. Nothing is retried either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenReauthFailedLogin {
    /// The FIRST answer: the venue's own words on the dead token.
    AnswerFirst,
    /// The login's own error (alpaca, ig): a mint that was never reached is a transport failure, not the dead status
    /// it would otherwise wear.
    AnswerLoginError,
}

impl<T: Clone> TokenCell<T> {
    /// A cell holding the first login's `token`, at generation `1`.
    pub fn new(token: T) -> Self {
        TokenCell { held: Mutex::new((token, 1)) }
    }

    /// The token in force and its generation.
    pub fn current(&self) -> (T, u64) {
        self.held.lock().expect("token cell poisoned").clone()
    }

    /// Log in again with `login` unless another caller already did since the caller saw generation `seen`, and answer
    /// the generation now in force. The lock is held ACROSS the login on purpose: that is what collapses a concurrent
    /// stampede into one login. A failed login leaves the old token in place, so a network failure does not also
    /// destroy it.
    ///
    /// # Errors
    /// What `login` answered.
    pub fn relogin<E>(&self, seen: u64, login: impl FnOnce() -> Result<T, E>) -> Result<u64, E> {
        let mut held = self.held.lock().expect("token cell poisoned");
        if held.1 != seen {
            return Ok(held.1);
        }
        held.0 = login()?;
        held.1 += 1;
        Ok(held.1)
    }

    /// Run one call through the ladder: `run` with the token in force; on `dead_status`, `relogin` (handed the
    /// generation the call used; typically [`Self::relogin`] behind the venue's own login and log lines), then `run`
    /// ONCE more with the token now in force. Returns the raw `(status, body)` of whichever run was last, for the caller
    /// to parse: the status must be seen before a parse turns it into an error and loses it.
    ///
    /// A failed re-login answers by `failed_login` ([`TokenReauthFailedLogin`]) and runs nothing again; a transport
    /// error from `run` returns at once and is NEVER a re-login trigger (a dead socket says nothing about the token,
    /// and re-logging-in on one would turn an outage into a login storm).
    ///
    /// # Errors
    /// What `run` answered as an error, and the login's under [`TokenReauthFailedLogin::AnswerLoginError`].
    pub fn reauth_once<E>(
        &self,
        dead_status: u16,
        failed_login: TokenReauthFailedLogin,
        run: impl Fn(&T) -> Result<(u16, String), E>,
        relogin: impl FnOnce(u64) -> Result<u64, E>,
        mut note: impl FnMut(TokenReauthNote),
    ) -> Result<(u16, String), E> {
        let (token, generation) = self.current();
        let first = run(&token)?;
        if first.0 != dead_status {
            return Ok(first);
        }
        note(TokenReauthNote::Reauthenticating);
        if let Err(e) = relogin(generation) {
            return match failed_login {
                TokenReauthFailedLogin::AnswerFirst => Ok(first),
                TokenReauthFailedLogin::AnswerLoginError => Err(e),
            };
        }
        let second = run(&self.current().0)?;
        if second.0 == dead_status {
            note(TokenReauthNote::RefusedAgain);
        }
        Ok(second)
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod auth_tests;
