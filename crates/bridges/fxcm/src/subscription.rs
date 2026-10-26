//! FXCM's instrument SUBSCRIPTION: the bridge makes a symbol tradable ITSELF (owner, 2026-10-26: "the user has to
//! have the ability to trade ANY ticker of the respective venue"). PURE: what a status means, which rows need the
//! request, how the shim's answer reads and what a session remembers of its attempts. The request itself is
//! `fcshim.cpp`'s `fc_set_subscription` and the calls are `crate::sys::FxcmSession::subscribe_tradable` (the exec
//! session, on the first order for an instrument) and `crate::reads::AccountReads` (the reconcile session, when the
//! node admits a symbol or the price lane is asked for one).
//!
//! # What the venue does (MEASURED on the demo login, 2026-10-26, `docs/ops/fxcm-forexconnect.md`)
//!
//! * A login sees the whole Offers table (497 rows) and each row has a subscription status: `T` tradable, `V` view
//!   only, `D` disabled. FXCM itself keeps the conversion legs of the account currency at `V`.
//! * `SetSubscriptionStatus` (`Command`, `OfferID`, `SubscriptionStatus`) is answered in 0.2 to 0.5 s and the new
//!   status reaches every session of the login at once. A `D` forex pair, twelve of them one after another (31
//!   tradable rows), was never refused. Setting a row back to `D` can flip a `V` row to `D` (FXCM's own bookkeeping),
//!   which is why the bridge only ever subscribes and never un-subscribes.
//!
//! # Rules
//!
//! * **Once per instrument per session**, bounded, no retry storm: [`Attempts`]. A refusal is remembered
//!   ([`REFUSED_KEEP`], an unknown outcome [`UNKNOWN_KEEP`]) and returned in words without asking again.
//! * **Only a row the bridge would admit is subscribed** ([`needs_subscribing`]): a regular forex pair. A metal,
//!   an index or a stock CFD is refused by name further on (`crate::home_conversion::quote_currency_of`), and a
//!   venue-side write for an instrument that is refused anyway would change the account for nothing.
//! * **A bridge that subscribed a row does not fight a person who un-subscribed it afterwards**: `Done` is final for
//!   the session ([`Next::Done`]) and the row reads as not subscribed, in words ([`undone_words`]).
//! * A refusal is a NAMED reason, what FXCM said ([`answer_from_shim`]), never the bare "not subscribed".

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::home_conversion::{INSTRUMENT_FOREX, Offer, quote_currency_of};

/// The one status the bridge asks FXCM for.
pub(crate) const TRADABLE: &str = "T";

/// How long a REFUSAL (FXCM said no, the row does not exist, the request could not be built) is remembered: asking
/// again would be refused again.
pub(crate) const REFUSED_KEEP: Duration = Duration::from_secs(600);

/// How long an UNKNOWN outcome (no answer, a row that did not settle, a table that could not be read) is remembered
/// before the next attempt: the request may well have applied, and a retry storm is what this bounds.
pub(crate) const UNKNOWN_KEEP: Duration = Duration::from_secs(30);

/// The whole time the shim may spend on one subscription when it runs on the EXEC thread: a first order on an
/// unsubscribed pair still fits under the ack timeout (`fcshim.cpp`'s budget note: 20 s of placement plus this).
pub(crate) const EXEC_BUDGET_MS: i32 = 8_000;

/// The same on the RECONCILE session, which no order waits behind.
pub(crate) const RECON_BUDGET_MS: i32 = 25_000;

/// The meaning of a subscription status (`IO2GOfferRow::getSubscriptionStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// `T`: price updates AND trading.
    Tradable,
    /// `V`: price updates, not for trading (FXCM uses it for the conversion legs of the account currency).
    ViewOnly,
    /// `D`: no price updates.
    Disabled,
    /// Anything else the venue might one day say.
    Other,
}

impl Status {
    /// The status a row's `subscription` text states.
    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "T" => Self::Tradable,
            "V" => Self::ViewOnly,
            "D" => Self::Disabled,
            _ => Self::Other,
        }
    }

    /// The status in the operator's words, with the venue's letter.
    pub(crate) fn words(text: &str) -> String {
        match Self::parse(text) {
            Self::Tradable => "tradable (`T`)".to_string(),
            Self::ViewOnly => "view only (`V`)".to_string(),
            Self::Disabled => "disabled (`D`)".to_string(),
            Self::Other => format!("`{text}`, a status this bridge does not know"),
        }
    }
}

/// Whether the bridge should ask FXCM to subscribe this row: it is not tradable AND it is a regular forex pair the
/// bridge would admit ([`quote_currency_of`]).
pub(crate) fn needs_subscribing(row: &Offer) -> bool {
    Status::parse(&row.subscription) != Status::Tradable && quote_currency_of(row).is_ok()
}

/// **The instruments to subscribe so that `symbol` can be admitted** for an account whose money is `account_ccy`:
/// the symbol's own row, and, when it is quoted in another currency, the ONE listed conversion pair (`Q/A` or
/// `A/Q`) -- if both orientations are listed and neither is tradable the choice is not made here and the
/// conversion refusal names both. Names only, symbol first, no duplicates; empty when everything is tradable or
/// the symbol is not in the table (the facts then say so).
pub(crate) fn wanted(offers: &[Offer], symbol: &str, account_ccy: &str) -> Vec<String> {
    let name = crate::exec::to_fxcm_instrument(symbol);
    let Some(row) = offers.iter().find(|o| o.instrument == name) else { return Vec::new() };
    let mut names = Vec::new();
    if needs_subscribing(row) {
        names.push(name);
    }
    let Ok(quote) = quote_currency_of(row) else { return names };
    if quote == account_ccy {
        return names;
    }
    let pair = |a: &str, b: &str| format!("{a}/{b}");
    let listed: Vec<&Offer> = [pair(&quote, account_ccy), pair(account_ccy, &quote)]
        .iter()
        .filter_map(|n| {
            offers.iter().find(|o| &o.instrument == n && o.instrument_type == INSTRUMENT_FOREX)
        })
        .collect();
    if let [only] = listed.as_slice()
        && needs_subscribing(only)
        && !names.contains(&only.instrument)
    {
        names.push(only.instrument.clone());
    }
    names
}

/// Why a subscription did not come to pass, and whether a later try may do better.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The reason, in words: what FXCM (or the SDK) said.
    pub why: String,
    /// `true` when this is NOT a verdict on the symbol: the outcome is UNKNOWN (no answer, a row that did not
    /// settle, an unreadable table) or nothing could be sent at all (the installed shim is older than the binary
    /// and has no `fc_set_subscription`). The node must not remember it as a refusal of the symbol, an order is
    /// still placed (the shim's own offer lookup judges the row), and the next attempt comes sooner
    /// ([`UNKNOWN_KEEP`]).
    pub transient: bool,
}

/// **The shim's answer to `fc_set_subscription`**, as words. `code` and `text` are what it returned (`text` already
/// passed through the crate's redaction); `status` is the one asked for.
///
/// # Errors
/// Every code but `0`: the instrument is not listed (`-3`), the request could not be built (`-4`, `-5`: the shim
/// returns these from its SEND stage only, so they are a verdict), the venue refused it (`-6`, its words verbatim),
/// the lookup before the send failed or there was no answer after it (`-8`: never a verdict, the shim returns it for
/// EVERY lookup failure), the venue answered and the row still reads another status (`-9`), or the instrument is not a
/// regular forex instrument and the bridge does not subscribe it (`-14`).
pub(crate) fn answer_from_shim(instrument: &str, code: i32, text: &str) -> Result<(), Failure> {
    let said = text.trim();
    let definite = |why: String| Err(Failure { why, transient: false });
    let unknown = |why: String| Err(Failure { why, transient: true });
    match code {
        0 => Ok(()),
        -3 => definite(format!(
            "fxcm: `{instrument}` is not in the venue's Offers table, so it cannot be subscribed"
        )),
        -4 | -5 => definite(format!(
            "fxcm: ForexConnect could not build the request that subscribes `{instrument}`: {said}"
        )),
        -6 => definite(format!(
            "fxcm: FXCM refused to subscribe `{instrument}` for trading; the venue said: {said}"
        )),
        FC_NOT_REGULAR_FOREX => definite(format!(
            "fxcm: `{instrument}` is not subscribed for trading and is not a regular forex instrument ({said}); the \
             bridge subscribes regular forex pairs only, so subscribe it in the FXCM platform"
        )),
        -8 => unknown(format!(
            "fxcm: the subscription of `{instrument}` is not known to have applied: {said}"
        )),
        -9 => unknown(format!(
            "fxcm: FXCM answered the request that subscribes `{instrument}` but its Offers row still reads {} \
             after the wait",
            Status::words(said)
        )),
        other => unknown(format!(
            "fxcm: the subscription of `{instrument}` failed with code {other}: {said}"
        )),
    }
}

/// `fcshim.cpp`'s `FC_NOT_REGULAR_FOREX`.
const FC_NOT_REGULAR_FOREX: i32 = -14;

/// The one connection a subscription is written on. `venue.fxcm.<tier>.connection` is an unvalidated setting, so a
/// wrongly set `Real` must never write on a real account: whether a REAL account may change its own subscription (the
/// SDK's `canChangeOfferSubscription` permission exists for it) is not measured, and a real account is not where
/// this is first tried.
pub(crate) const WRITE_CONNECTION: &str = "Demo";

/// **Why a session cannot make an instrument tradable itself**, or `None`: it is not on the `Demo` connection, or
/// the shim it opened (`without_export` is its described path) is older than the binary and has no
/// `fc_set_subscription`. Nothing is sent in either case, so neither is a verdict on any symbol ([`Failure`]'s
/// `transient`), and an order on an already tradable instrument is placed exactly as before.
pub(crate) fn subscribe_gap(connection: &str, without_export: Option<&str>) -> Option<String> {
    if connection != WRITE_CONNECTION {
        return Some(format!(
            "the session is on the `{connection}` connection and the bridge writes a subscription on `{WRITE_CONNECTION}` \
             only (whether a real account may change its own subscription is not measured), so a symbol that is not \
             tradable must be subscribed in the FXCM platform"
        ));
    }
    without_export.map(|shim| {
        format!(
            "the shim this process opened — {shim} — is older than this binary and exports no `fc_set_subscription`; the \
             deploy installs the release's shim once the root-owned deploy helper is reinstalled on the box \
             (`docs/ops/fxcm-forexconnect.md`, \"How the shim reaches a box\"), and until then a symbol that is not \
             tradable must be subscribed in the FXCM platform"
        )
    })
}

/// The words for a row this bridge subscribed earlier in the session and that reads as not tradable again.
pub(crate) fn undone_words(instrument: &str, now: &str) -> Failure {
    Failure {
        why: format!(
            "fxcm: `{instrument}` was subscribed by this bridge earlier in this session and its Offers row reads {} \
             again, so it was changed at the venue since; the bridge does not subscribe it a second time",
            Status::words(now)
        ),
        transient: false,
    }
}

/// What a session did about one instrument.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// Subscribed earlier in this session: nothing to send.
    Done,
    /// Refused or unknown earlier, and remembered still: the reason, nothing to send.
    Refused(Failure),
    /// Not asked yet (or the memory of a failure has run out): ask now.
    Ask,
}

/// One session's memory of its subscription attempts. A new session is a new `Attempts`.
#[derive(Debug, Default)]
pub(crate) struct Attempts {
    done: HashSet<String>,
    failed: HashMap<String, (Failure, Instant)>,
}

impl Attempts {
    /// What to do about `name` at `now`.
    pub(crate) fn next(&self, name: &str, now: Instant) -> Next {
        if self.done.contains(name) {
            return Next::Done;
        }
        match self.failed.get(name) {
            Some((failure, at)) => {
                let keep = if failure.transient { UNKNOWN_KEEP } else { REFUSED_KEEP };
                if now.saturating_duration_since(*at) < keep {
                    Next::Refused(failure.clone())
                } else {
                    Next::Ask
                }
            }
            None => Next::Ask,
        }
    }

    /// Remember what an attempt came to.
    pub(crate) fn record(&mut self, name: &str, result: &Result<(), Failure>, now: Instant) {
        match result {
            Ok(()) => {
                self.failed.remove(name);
                self.done.insert(name.to_string());
            }
            Err(failure) => {
                self.failed.insert(name.to_string(), (failure.clone(), now));
            }
        }
    }
}

/// **Make `name` tradable once per session**: nothing to send when this session did it already, the remembered
/// reason when it failed lately, else `ask` (the shim call) and remember how it went.
///
/// # Errors
/// The remembered or fresh [`Failure`].
pub(crate) fn ensure(
    attempts: &mut Attempts,
    name: &str,
    now: Instant,
    ask: impl FnOnce() -> Result<(), Failure>,
) -> Result<(), Failure> {
    match attempts.next(name, now) {
        Next::Done => Ok(()),
        Next::Refused(failure) => Err(failure),
        Next::Ask => {
            let result = ask();
            attempts.record(name, &result, Instant::now());
            result
        }
    }
}

#[path = "subscription_tests.rs"]
#[cfg(test)]
mod subscription_tests;
