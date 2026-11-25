//! **The Polymarket cockpit follows the live up/down window**: the window's token is the live window's `Up` token of a
//! family the backend's datahub records, as its recorder holds it (`Request::LiveFamilyTokens`), so the cockpit shows
//! the current market's book and its orders name the current market's token. The desktop asks no venue for it.
//!
//! * **When it asks.** On a cockpit's first frame, and again when the window it shows ENDS (that token's own end, as
//!   the venue states it): driven by the window's end, not a clock. An answer with no live window yet (a recorder
//!   that has not resolved since it started) is asked again after [`RETRY_AFTER_MS`]; a failed ask (no datahub, an
//!   older datahub, no recorder) at the end of the rail's current window.
//! * **What it picks.** The family whose window length is the chain rail's ([`UPDOWN_WINDOW_SECS`]), else the first
//!   the datahub names; within it the live window (the earliest end still ahead) and its `Up` outcome, the YES leg the
//!   ladder and the ticket read. The answer lists the next window too, so at a window's end the cockpit moves to it
//!   at once and the ask only refreshes what comes after.
//! * **What it replaces.** A token the answer lists as live is kept (a seeded live token stays); any other (the
//!   placeholder, an ended window's, one restored from a saved layout) becomes the live `Up` token. Until an answer
//!   names one, the window keeps its symbol, so a placeholder is never subscribed.
//! * **What it leaves alone.** The book subscription, the old token's cleanup and the order path read the window's
//!   symbol as before (`crate::ui::feed_lifecycle`, the desktop's cockpit drain): this module only writes it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_datahub_client::DatahubClient;
use vike_datahub_client::family::LiveFamilyToken;
use vike_hub_protocol::auth::Scope;
use vike_model::fair::UPDOWN_WINDOW_SECS;

use crate::data::catalog_refresh::DatahubCatalogRefresh;
use crate::ui::window::{WindowKind, WindowState};
use crate::ui::windows::WindowView;
use vike_utility::mutex::lock;

/// How long after an answer with no live window the cockpit asks again: about one recorder tick, the time a freshly
/// started recorder takes to resolve its families.
pub(crate) const RETRY_AFTER_MS: i64 = 10_000;
/// The outcome whose book the cockpit shows: an up/down market's YES leg.
pub(crate) const UP_OUTCOME: &str = "Up";
/// How long one ask waits for the datahub's answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// What the cockpit says while its first ask is out.
pub(crate) const ASKING: &str = "asking the datahub which Polymarket market is live…";
/// What the cockpit says when the datahub's answer names no live window.
pub(crate) const NO_LIVE_WINDOW: &str =
    "the datahub's recorder holds no live window of a rolling Polymarket family yet";
/// What the cockpit says with no datahub to ask.
pub(crate) const COCKPIT_FOLLOW_NO_DATAHUB: &str =
    "no datahub is connected, so the cockpit cannot learn which Polymarket market is live";

/// One ask's answer, left by its thread for the next frame.
type CockpitFollowAnswerSlot = Arc<Mutex<Option<Result<Vec<LiveFamilyToken>, String>>>>;

/// One cockpit window's follower state (the window's [`WindowView`]). A clone (a cloned window) keeps the answer held and
/// asks on its own: it never shares an ask that is out.
#[derive(Default)]
pub struct CockpitFollow {
    /// The datahub's last answer.
    tokens: Vec<LiveFamilyToken>,
    /// The family this window follows, once picked.
    family: Option<String>,
    /// Ask once the frame's clock reaches this; `None` asks at once.
    ask_at_ms: Option<i64>,
    /// Whether an ask is out; its answer lands in `answer`.
    asking: bool,
    answer: CockpitFollowAnswerSlot,
    /// Why the window shows no live token, in words; `None` while it follows one.
    status: Option<String>,
}

impl Clone for CockpitFollow {
    fn clone(&self) -> Self {
        Self {
            tokens: self.tokens.clone(),
            family: self.family.clone(),
            status: self.status.clone(),
            ..Self::default()
        }
    }
}

impl CockpitFollow {
    /// Why the window shows no live token, in words, or `None`.
    #[must_use]
    pub(crate) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Whether an ask is out (the cockpit repaints until it lands).
    #[must_use]
    pub(crate) fn asking(&self) -> bool {
        self.asking
    }

    /// One frame: take a landed answer, point `symbol` at the live window's `Up` token, and ask when due through
    /// `ask` (which starts the ask, or says why it cannot).
    pub(crate) fn step(
        &mut self,
        symbol: &mut String,
        now_ms: i64,
        ask: impl FnOnce(&CockpitFollowAnswerSlot) -> Result<(), String>,
    ) {
        let mut answered = false;
        if self.asking
            && let Some(answer) = lock(&self.answer).take()
        {
            self.asking = false;
            match answer {
                Ok(tokens) => {
                    self.tokens = tokens;
                    self.status = None;
                    answered = true;
                }
                Err(why) => {
                    self.status = Some(why);
                    self.ask_at_ms = rail_window_end(now_ms);
                }
            }
        }
        if !listed_live(&self.tokens, symbol, now_ms)
            && let Some(live) = live_up_token(&self.tokens, self.family.as_deref(), now_ms)
        {
            self.family = Some(live.family.clone());
            symbol.clone_from(&live.token);
        }
        if answered {
            // The next ask is due when the window shown ends; with none shown, after a retry.
            match end_of(&self.tokens, symbol).filter(|end| *end > now_ms) {
                Some(end) => self.ask_at_ms = Some(end),
                None => {
                    self.ask_at_ms = Some(now_ms + RETRY_AFTER_MS);
                    self.status = Some(NO_LIVE_WINDOW.to_string());
                }
            }
        }
        if !self.asking && self.ask_at_ms.is_none_or(|at| now_ms >= at) {
            match ask(&self.answer) {
                Ok(()) => {
                    self.asking = true;
                    if self.tokens.is_empty() && self.status.is_none() {
                        self.status = Some(ASKING.to_string());
                    }
                }
                Err(why) => {
                    self.status = Some(why);
                    self.ask_at_ms = rail_window_end(now_ms);
                }
            }
        }
    }
}

/// The end of the chain rail's current window: when a failed ask is tried again. Always there,
/// `UPDOWN_WINDOW_SECS` being a positive length; with no window, a failed ask waits for nothing.
fn rail_window_end(now_ms: i64) -> Option<i64> {
    vike_order_entry::cockpit::window_open_ms(now_ms, UPDOWN_WINDOW_SECS)
        .and_then(|open| vike_order_entry::cockpit::window_close_ms(open, UPDOWN_WINDOW_SECS))
}

/// The end of `symbol`'s window, when the answer lists it.
fn end_of(tokens: &[LiveFamilyToken], symbol: &str) -> Option<i64> {
    tokens.iter().find(|t| t.token == symbol).and_then(|t| t.end_ms)
}

/// Whether the answer lists `symbol` with its window still ahead.
fn listed_live(tokens: &[LiveFamilyToken], symbol: &str, now_ms: i64) -> bool {
    end_of(tokens, symbol).is_some_and(|end| end > now_ms)
}

/// The live window's `Up` token: of `family` when the answer still names it, else of the family whose window length is
/// the rail's, else of the first family named; the earliest end still ahead.
#[must_use]
pub(crate) fn live_up_token<'a>(
    tokens: &'a [LiveFamilyToken],
    family: Option<&str>,
    now_ms: i64,
) -> Option<&'a LiveFamilyToken> {
    let named = |f: &str| tokens.iter().any(|t| t.family == f);
    let rail_length = |f: &&LiveFamilyToken| {
        vike_catalog::polymarket_family_minutes(&f.family)
            .is_ok_and(|m| m * 60 == UPDOWN_WINDOW_SECS)
    };
    let family = family
        .filter(|f| named(f))
        .map(str::to_string)
        .or_else(|| tokens.iter().find(rail_length).map(|t| t.family.clone()))
        .or_else(|| tokens.iter().map(|t| t.family.clone()).min())?;
    tokens
        .iter()
        .filter(|t| t.family == family && t.outcome == UP_OUTCOME)
        .filter(|t| t.end_ms.is_some_and(|end| end > now_ms))
        .min_by_key(|t| t.end_ms)
}

/// **The cockpit window's symbol this frame**: a Polymarket window's follows the live window (`CockpitFollow::step`,
/// asking the datahub `catalog` dials on a thread of its own); every other window's is its own, untouched.
pub fn follow_cockpit(
    w: &mut WindowState,
    tv: &mut WindowView,
    catalog: &DatahubCatalogRefresh,
    now_ms: i64,
) -> String {
    if w.kind == WindowKind::Polymarket {
        tv.cockpit_follow.step(&mut w.symbol, now_ms, |slot| {
            let dial =
                catalog.dial_snapshot().ok_or_else(|| COCKPIT_FOLLOW_NO_DATAHUB.to_string())?;
            let slot = Arc::clone(slot);
            std::thread::Builder::new()
                .name("ui-cockpit".into())
                .spawn(move || {
                    let answer = DatahubClient::connect_with_keys(
                        &dial.address,
                        dial.keys.as_ref(),
                        Scope::Read,
                    )
                    .map_err(|e| {
                        format!("the datahub at {} could not be reached: {e}", dial.address)
                    })
                    .and_then(|mut client| {
                        client.set_reply_timeout(Some(REPLY_TIMEOUT));
                        client.live_family_tokens()
                    });
                    *lock(&slot) = Some(answer);
                })
                .map(|_| ())
                .map_err(|e| format!("the cockpit could not start its ask of the datahub: {e}"))
        });
    }
    w.symbol.clone()
}

#[path = "follow_tests.rs"]
#[cfg(test)]
mod follow_tests;
