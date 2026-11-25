//! **An account's path to IG, as this bridge last heard it**: the bridge's words for the reading behind `trade
//! health`'s `gateway-link` row for an IG account. The reading itself is the account's ONE cell
//! (`vike_bridge_core::venue_link::venue_link` of its route key, which the gatewayhub's status verb reads for every
//! venue); this module writes its two named sub-paths. The lease to the gatewayhub stays up through a dead order
//! session or a dead trade stream, and the engine stays `RUNNING`, so nothing else can say so. It changes no order's
//! fate: both lanes log in, retry and give up exactly as they did without it.
//!
//! An IG account's path is two paths, and the reading needs both:
//!
//! * **The order session** (REST, the exec thread's `IgSession`): its login, then every authenticated call it makes
//!   (`IgSession::call_with_reauth` writes here, for the exec lane's session alone). Lost when its login failed, when
//!   IG did not answer its last call, when IG refused its tokens after a re-login, or when the re-login itself failed
//!   (said by the login error's kind).
//! * **The trade stream** (Lightstreamer, `crate::execution::user_data`): every line it reads, `PROBE` heartbeats included.
//!   Lost when a dial or a read failed, or when its login did.
//!
//! It answers in the shared vocabulary, `vike_bridge_core::venue_link`'s [`VenueLinkState`], and the cell's rules
//! read the two paths (the sub-paths [`ORDER_SESSION`], claimed first, and [`TRADE_STREAM`]); the words stay here.
//! [`VenueLinkState::Up`] is both answered AND the stream's last line younger than [`FRESH_FOR`] (the stream's "up"
//! vouches for that long). The order session's last answer is not aged: an idle exec thread asks IG nothing for hours,
//! and its tokens heal on the next call (the 401 ladder), so it is the stream's heartbeat, on the same gateway and key,
//! that vouches IG answers NOW. A lost path is [`VenueLinkState::Down`] in its own words, the order session's
//! outranking the stream's (the first claimed among equal losses), and always a failure
//! (`vike_bridge_core::venue_link::VenueLinkDown::failed`: `Error`, never a routine reopen). Anything else (nothing
//! said yet, or a stream whose last line is too old) is [`VenueLinkState::Unknown`], never up.
//!
//! [`SessionLink`], found by account LABEL ([`session_link`]), holds this client's two path handles and also lends the
//! order session itself to an operator's read of the account ([`SessionLink::order_session`],
//! `crate::reconcile::venue_read`), which then opens no IG login of its own: the shared cell carries no session, which
//! is why this type stays.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use vike_bridge_core::venue_link::{VenueLink, VenueLinkDown, VenueLinkPath, VenueLinkState};
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::venue_timings;

use crate::transport::IgSession;
use vike_bridge_core::transport::http_error::VenueApiHttpError;

/// How old the stream's last line may be and still vouch for the path: a read waits at most the trade stream's idle
/// bound before it fails (and says so here), so a line older than twice that means the reader is not reading at all.
const FRESH_FOR: Duration = venue_timings::IG.exec().idle_threshold().saturating_mul(2);

/// The longest reason kept: a venue's error text goes onto every observe frame while the path is down.
const WHY_LIMIT: usize = 240;

/// The order session's login failed: the exec thread has ended, and nothing logs it in again (`login_with_retry` is
/// bounded), so the wire's default "until its bridge has it back" would promise a return that never comes.
const LOGIN_LOST: &str = "every order is refused, and nothing logs this session in again until the account's \
                          `account.active` is set off and on";

/// IG did not answer the order session's last call: the next order is still sent.
const CALL_LOST: &str = "the next order is still sent, and IG did not answer this session's last call, so it may \
                         not answer that one either";

/// The order session's re-login after a 401 failed: the pair stays the refused one, so the next call meets the 401
/// again and its ladder tries the re-login once more.
const RELOGIN_LOST: &str =
    "the next order is still sent, and IG's 401 to it tries the re-login once more";

/// The trade stream is down and its driver redials it by itself.
const STREAM_REDIALING: &str = "orders still go out on the order session; working-order fills and venue-side \
                                cancels arrive when the bridge has the trade stream back (it redials by itself), \
                                and its snapshot re-delivers what the gap held";

/// The trade stream's thread has ended: nothing redials it until the adapter is started again.
const STREAM_GAVE_UP: &str = "orders still go out on the order session; working-order fills and venue-side \
                              cancels will NOT arrive: nothing redials the trade stream until the account's \
                              `account.active` is set off and on";

/// The name of the order session's sub-path of the account's cell, claimed first so its loss outranks the stream's.
pub(crate) const ORDER_SESSION: &str = "order session";

/// The name of the trade stream's sub-path of the account's cell.
pub(crate) const TRADE_STREAM: &str = "trade stream";

/// A lost path in its own words, the venue's text cut at [`WHY_LIMIT`]. Every loss here is a failure (a login, a call,
/// a re-login, a dial or a read failed), so `Error`, said at once; neither lane counts its retries or states its next
/// one.
fn lost(which: &str, why: &str, effect: &'static str) -> VenueLinkState {
    VenueLinkState::Down(VenueLinkDown::failed(
        format!("{which}: {}", why.chars().take(WHY_LIMIT).collect::<String>()),
        effect,
        0,
        Duration::ZERO,
    ))
}

#[derive(Debug, Default)]
struct Paths {
    /// The order session's sub-path of the account's cell, once this client claimed it ([`SessionLink::claim`]).
    rest: Option<VenueLinkPath>,
    /// The trade stream's sub-path.
    stream: Option<VenueLinkPath>,
    /// The order session itself, for an operator's read that opens no login of its own (`crate::reconcile::venue_read`). Weak:
    /// the session holds this cell (`IgSession::report_to`), so a strong one would be a cycle, and a stopped client's
    /// session is not kept alive by this registry.
    session: Weak<IgSession>,
}

/// **One IG account's two path handles and its lent order session**: the exec thread's order session and the trade
/// stream write the account's cell through it. Before [`Self::claim`] it writes nothing.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionLink(Arc<Mutex<Paths>>);

impl SessionLink {
    fn lock(&self) -> std::sync::MutexGuard<'_, Paths> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// **A new exec client takes the account's cell** (once, at its start, before any lane writes): what the last
    /// client said is history, and a lane of an earlier client that outlived its teardown writes nothing from now on.
    /// The order session is claimed before the trade stream, so its loss is the one read when both are lost.
    pub(crate) fn claim(&self, cell: &VenueLink) {
        let cell = cell.claim();
        let rest = cell.claim_path(ORDER_SESSION);
        let stream = cell.claim_path(TRADE_STREAM);
        *self.lock() = Paths { rest: Some(rest), stream: Some(stream), session: Weak::new() };
    }

    fn rest(&self, state: VenueLinkState) {
        if let Some(path) = &self.lock().rest {
            path.set(state, None);
        }
    }

    fn stream(&self, state: VenueLinkState, fresh: Option<Duration>) {
        if let Some(path) = &self.lock().stream {
            path.set(state, fresh);
        }
    }

    /// The exec thread's logged-in order session, lent to an operator's read of the account.
    pub(crate) fn hold_session(&self, session: &Arc<IgSession>) {
        self.lock().session = Arc::downgrade(session);
    }

    /// The account's order session while its exec client runs; `None` before its login and after it stopped.
    pub(crate) fn order_session(&self) -> Option<Arc<IgSession>> {
        self.lock().session.upgrade()
    }

    /// The order session answered: up until the next word (not aged, see the module doc).
    pub(crate) fn rest_answered(&self) {
        self.rest(VenueLinkState::Up);
    }

    /// The order session's login failed: the exec thread has ended.
    pub(crate) fn rest_login_lost(&self, why: &str) {
        self.rest(lost("the order session's login failed", why, LOGIN_LOST));
    }

    /// The order session's word on one authenticated call, after the 401 ladder ran (`IgSession::call_with_reauth`):
    /// any status IG answers with is an answer, but a 401 the ladder could not cure and a gateway's ambiguous 5xx.
    pub(crate) fn rest_heard(&self, answer: &Result<(u16, String), VenueApiHttpError>) {
        let lost = |why: &str| self.rest(lost("the order session", why, CALL_LOST));
        match answer {
            Err(e) => lost(&format!("IG did not answer its last call ({})", e.message)),
            Ok((status, _)) if *status == crate::transport::REAUTH_STATUS => {
                lost("IG refused its tokens (HTTP 401) and a re-login did not cure it");
            }
            Ok((status, _))
                if vike_transport::send_outcome::status_is_gateway_ambiguous(*status) =>
            {
                lost(&format!("IG's gateway answered its last call with HTTP {status}"));
            }
            Ok(_) => self.rest_answered(),
        }
    }

    /// The order session's re-login after a 401 FAILED (`IgSession::call_with_reauth`), said by the login error's
    /// own kind: IG refusing it with a status, IG answering it without a usable session, its host never reached, or
    /// the login's own words (an answer that could not be read, a re-login landing on another account). Never "did not
    /// answer its last call": the call was answered, with the 401.
    pub(crate) fn rest_relogin_failed(&self, login: &VenueApiHttpError) {
        let words = &login.message;
        let why = match login.status {
            0 => format!("IG's login could not be reached for a re-login ({words})"),
            status if (200..300).contains(&status) => {
                format!(
                    "IG answered the re-login without a usable session (HTTP {status}: {words})"
                )
            }
            status if status >= 100 => format!("IG refused the re-login (HTTP {status}: {words})"),
            _ => format!("the re-login failed ({words})"),
        };
        self.rest(lost("the order session", &why, RELOGIN_LOST));
    }

    /// The trade stream read a line (a `PROBE` heartbeat included), or its subscription was answered: up for
    /// [`FRESH_FOR`].
    pub(crate) fn stream_answered(&self) {
        self.stream_answered_at(Instant::now());
    }

    /// [`Self::stream_answered`], heard at `at`.
    fn stream_answered_at(&self, at: Instant) {
        if let Some(path) = &self.lock().stream {
            path.set_at(VenueLinkState::Up, Some(FRESH_FOR), at);
        }
    }

    /// The trade stream is down and its driver redials it.
    pub(crate) fn stream_lost(&self, why: &str) {
        self.stream(lost("the trade stream", why, STREAM_REDIALING), None);
    }

    /// The trade stream's thread ended before it could stream (its login failed, no Lightstreamer endpoint). Said as an
    /// ordinary loss, not the cell's give-up: the order session's loss still outranks it, as it always did.
    pub(crate) fn stream_gave_up(&self, why: &str) {
        self.stream(lost("the trade stream", why, STREAM_GAVE_UP), None);
    }
}

/// The [`SessionLink`] of `account`: its exec client's path handles and lent order session, found by LABEL because an
/// operator's read of the account (`crate::reconcile::venue_read`) knows the label and no route key.
/// shortcut: one entry per account label for the life of the process, bounded by the directory; a new exec client
/// claims its cell again, so a stopped adapter's last word is history once no adapter reports it.
#[must_use]
pub(crate) fn session_link(account: &AccountLabel) -> SessionLink {
    static LINKS: LazyLock<Mutex<HashMap<String, SessionLink>>> = LazyLock::new(Mutex::default);
    let mut links = LINKS.lock().unwrap_or_else(PoisonError::into_inner);
    links.entry(account.to_string()).or_default().clone()
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
