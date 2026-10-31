//! `assumed_ccy` — the owner's USD default on the gatewayhub side, for the venues whose own read of the account's
//! currency can come back with nothing (decision 0183, amended 2026-10-19): **the account currency is what the
//! venue API states; if the API does not state it, or the read fails, USD.** Never a refusal because a currency
//! is unknown, never silent.
//!
//! This is the generalisation of what FXCM's lane has done since v0.1.81 (`fxcm_rate`, `vike_fxcm::reads`): the
//! currency a start tells the node travels with a flag, `AdapterReply::Live::account_ccy_assumed`, and the
//! assumption is said at `warn` in FXCM's words. Two readers use it: the START (`TradeLane::account_currency_stated`,
//! [`start_currency`]) and each venue's RATE SOURCE, which reads the currency for itself ([`source_currency`]). A
//! venue that STATES a currency is passed through untouched, byte for byte: only silence is filled in.
//!
//! **Two rules keep the readers from contradicting each other** (second review, HIGH-1):
//!
//! * **Only a START writes what the engine was told** ([`AssumedSaid::is_assumed`], which the lane's reconcile
//!   balance reads): the engine's label is fixed at its start, so a rate source that reads the currency again
//!   later and gets one must not clear "the engine is on an assumed USD", or the next pass would adopt the real
//!   currency's cash as dollars. The facts it answers STATE the ledger they are in instead
//!   (`WireSymbolFacts::account_ccy`), and the node refuses a ledger that is not the engine's. The one thing a
//!   later statement DOES end is a statement of the assumed currency itself: the venue then confirms the label the
//!   engine was started with, the cash is in that currency, and the belt follows the node's own lift
//!   ([`AssumedSaid::is_assumed`]). ⚠ This gate is a BELT: it follows the last start THIS process saw, and the node's
//!   re-teach after a gatewayhub restart is a start no engine consumes. The authority is the node's own gate over
//!   the reconcile client it binds (`vike-tradehub`'s `remote_backend::balance_gate`), which follows what the
//!   engine was bound with.
//! * **A currency the venue STATED is never downgraded** to an assumption by a later silent read, whoever asks
//!   and whoever stated it: a transient failure on an account the venue already named answers the named
//!   currency, unflagged. The memory is the process's and is keyed `(venue, account label)` AND bound to the
//!   credentials it was made under ([`AssumedSaid::bind`], a one-way [`CredentialFingerprint`] the lane takes at
//!   every ask): keys re-pointed under one label to an account in another currency start the memory afresh, so
//!   neither a stated currency nor an assumption of the OLD account answers for the new one. A restart forgets it
//!   too, and the next read decides afresh.
//!
//! ⚠ **A rate stamp alone never proves the venue stated the currency, and the answer says it separately.** The
//! lanes stamp their answers with the ledger the rates are in (`WireSymbolFacts::account_ccy`), and that ledger is
//! the lister's currency, which [`source_currency`] fills in with this module's USD, unflagged, whenever its own
//! read failed: an assumed USD stamps `USD` exactly as a stated one does (fxcm's `assumed.account_ccy ==
//! Some("USD")` is pinned). So the lane adds the proof beside the stamp: `WireSymbolFacts::account_ccy_stated`
//! ([`AssumedSaid::has_stated`], and fxcm's session for its own), true only when the VENUE named that currency to
//! some reader. The node lifts the hold on an assumed engine's balance (`vike-tradehub`'s
//! `remote_backend::balance_gate`) only on a stamp that is stated AND equals the anchor, i.e. when the venue now
//! confirms the very USD the engine only assumed; a stamp that merely equals its anchor still proves nothing.
//!
//! The node uses an assumed currency only for a `demo` account with `policy.max_account_exposure` armed (the
//! conversion's own gates); a `live` or `paper` account, or one with no ceiling, is told nothing it acts on.
//! ⚠ The gateway cannot know the ceiling, so its own assumption (a lister or a listing that read nothing) applies
//! to every `demo` account of these venues; the node's door still refuses what it cannot anchor.
//!
//! One `warn` per account while an assumption stands ([`AssumedSaid`]): a read that keeps coming back silent says
//! so once; once the venue has stated the currency there is nothing left to assume.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use vike_secrets::account_credentials::CredentialFingerprint;

/// The currency the owner's ruling assumes when the venue states none.
pub(crate) const ASSUMED_CCY: &str = "USD";

/// What one read of an account's currency came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CcyRead {
    /// The venue stated it.
    Stated(String),
    /// The venue stated none, or the read failed (`why`): the ruling's USD fills in.
    Silent(String),
    /// The venue stated a currency the lane cannot hand the node (the engine's mounted pair counts its money in
    /// another one), so there is nothing to assume over: the old answer stands.
    Declined,
}

/// What is known of one account's currency.
#[derive(Default)]
struct Entry {
    /// The currency the venue last STATED to any reader. Once there, a silent read is answered with it.
    stated: Option<String>,
    /// Whether the last START told the node an assumed USD: the only thing the balance gate reads.
    told_assumed: bool,
    /// Whether an assumption was already said at `warn`.
    warned: bool,
    /// The key set everything above was learned under ([`AssumedSaid::bind`]): `None` = not bound yet,
    /// `Some(None)` = bound to an account that holds no credentials.
    credentials: Option<Option<CredentialFingerprint>>,
}

/// What was said per `(venue, account)`, so a silent read is warned about once and a stated one is not undone.
#[derive(Default)]
pub(crate) struct AssumedSaid(Mutex<HashMap<(String, String), Entry>>);

/// The process's one memory of what it has said ([`start_currency`] and [`source_currency`] of production).
pub(crate) static CCY_SAID: LazyLock<AssumedSaid> = LazyLock::new(AssumedSaid::default);

impl AssumedSaid {
    fn with<R>(&self, venue: &str, account: &str, f: impl FnOnce(&mut Entry) -> R) -> R {
        let mut said = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        f(said.entry((venue.to_string(), account.to_string())).or_default())
    }

    /// Whether `(venue, account)`'s engine was last TOLD an assumed currency AND the venue has not since STATED that
    /// very currency: what the lane's reconcile balance reads to withhold a figure it cannot show to be in the
    /// ledger's currency. Written only by a start ([`start_currency`], [`Self::note_told`]); a currency stated
    /// later by a rate source ends it only when it IS the assumed one ([`ASSUMED_CCY`]: the venue confirms the
    /// engine's label, so its cash is in that currency after all, which is what the node's own gate lifts on),
    /// and a statement of any OTHER currency leaves it standing (the cash is in a currency the label is not).
    pub(crate) fn is_assumed(&self, venue: &str, account: &str) -> bool {
        let said = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        said.get(&(venue.to_string(), account.to_string())).is_some_and(|entry| {
            entry.told_assumed
                && !entry.stated.as_deref().is_some_and(|ccy| ccy.eq_ignore_ascii_case(ASSUMED_CCY))
        })
    }

    /// A start told the node `assumed` about `(venue, account)`'s currency, by a path that does its own saying
    /// (fxcm's session decides and warns for itself, `vike_fxcm::reads`).
    pub(crate) fn note_told(&self, venue: &str, account: &str, assumed: bool) {
        self.with(venue, account, |entry| entry.told_assumed = assumed);
    }

    /// **Bind `(venue, account)`'s memory to the credentials it is read with NOW** (`credentials`: the one-way
    /// fingerprint of the key set the lane holds for the account; `None` = it holds none). The first bind only
    /// records them. A LATER bind to a different key set means the keys were re-pointed under this label (an
    /// account in another currency, maybe): everything remembered under the old keys (a stated currency, an
    /// assumption, what the engine was told, that a warning was said) is forgotten, so the next read decides
    /// afresh and the new account's silence is warned about once again. Called by the lane before every use of
    /// the memory; a test that drives the memory directly never binds and sees no change.
    pub(crate) fn bind(
        &self,
        venue: &str,
        account: &str,
        credentials: Option<CredentialFingerprint>,
    ) {
        let forgot = self.with(venue, account, |entry| match entry.credentials {
            Some(held) if held == credentials => false,
            Some(_) => {
                let had = entry.stated.is_some() || entry.told_assumed || entry.warned;
                *entry = Entry { credentials: Some(credentials), ..Entry::default() };
                had
            }
            None => {
                entry.credentials = Some(credentials);
                false
            }
        });
        if forgot {
            tracing::warn!(
                venue,
                account,
                "{venue}: this account's credentials changed under its label: what the gatewayhub remembered of \
                 its currency (a stated one, an assumed USD) belonged to the old keys and is forgotten; the \
                 next read of the currency decides afresh"
            );
        }
    }

    /// Whether the VENUE has STATED `ccy` as `(venue, account)`'s currency to some reader since the credentials
    /// were bound: the proof a rate stamp in `ccy` lacks on its own (`WireSymbolFacts::account_ccy_stated`).
    pub(crate) fn has_stated(&self, venue: &str, account: &str, ccy: &str) -> bool {
        let said = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        said.get(&(venue.to_string(), account.to_string()))
            .and_then(|entry| entry.stated.as_deref())
            .is_some_and(|stated| stated.eq_ignore_ascii_case(ccy.trim()))
    }
}

/// The warn line of an assumption, in FXCM's words (`vike_fxcm::reads`' `assumed_words`), pure so it is pinned.
pub(crate) fn assumed_words(venue: &str, why: &str) -> String {
    format!(
        "{venue} account currency not reported by the venue: assuming {ASSUMED_CCY}, verify with the drill \
         ({why}). The node uses this only for a `demo` account with `policy.max_account_exposure` set; a \
         live or paper account, or one with no ceiling, keeps refusing a symbol quoted in another currency. \
         Until a start reads the currency from the venue, oanda, ig and fxcm hand the engine no reconcile \
         balance (a figure in the account's real currency would be labelled {ASSUMED_CCY})"
    )
}

/// The venue stated `ccy`: remembered for every reader, and said at `info` only when it replaces an assumption.
fn stated(said: &AssumedSaid, venue: &str, account: &str, ccy: &str) {
    let replaced = said.with(venue, account, |entry| {
        let replaced = entry.warned && entry.stated.is_none();
        entry.stated = Some(ccy.to_string());
        replaced
    });
    if replaced {
        tracing::info!(
            venue,
            account,
            "{venue}: the venue now states the account's currency ({ccy}); the assumed {ASSUMED_CCY} is \
             replaced by it for the next engine"
        );
    }
}

/// The venue stated none (`why`): the currency it stated before, if it ever did (never downgraded, nothing said);
/// else the ruling's USD, flagged, and said at `warn` once. `(currency, assumed)`.
fn fill_in(said: &AssumedSaid, venue: &str, account: &str, why: &str) -> (String, bool) {
    let (out, first) = said.with(venue, account, |entry| match &entry.stated {
        Some(ccy) => ((ccy.clone(), false), false),
        None => {
            let first = !entry.warned;
            entry.warned = true;
            ((ASSUMED_CCY.to_string(), true), first)
        }
    });
    if first {
        tracing::warn!(venue, account, "{}", assumed_words(venue, why));
    }
    out
}

/// **The account's currency for a start's reply**: `(currency, assumed)`. A stated currency is `(Some, false)`;
/// a silent read is the currency the venue stated before when it ever did (`(Some, false)`), else the ruling's USD,
/// `(Some("USD"), true)`, said at `warn` once; a declined one is `(None, false)` and writes nothing. The start is
/// the one writer of what the engine was told ([`AssumedSaid::is_assumed`]).
pub(crate) fn start_currency(
    said: &AssumedSaid,
    venue: &str,
    account: &str,
    read: CcyRead,
) -> (Option<String>, bool) {
    let (told, assumed) = match read {
        CcyRead::Stated(ccy) => {
            stated(said, venue, account, &ccy);
            (ccy, false)
        }
        CcyRead::Silent(why) => fill_in(said, venue, account, &why),
        CcyRead::Declined => return (None, false),
    };
    said.note_told(venue, account, assumed);
    (Some(told), assumed)
}

/// **The account's currency for a venue's rate source**, which reads it for itself: `(currency, assumed)`. A stated
/// currency passes through (and is remembered, so a later silence cannot undo it); a failed or silent read is the
/// currency the venue stated before, else the ruling's USD (flagged, said once). Never a refusal where the ruling
/// fills in. It writes nothing the balance gate reads: only a start does ([`start_currency`]).
pub(crate) fn source_currency(
    said: &AssumedSaid,
    venue: &str,
    account: &str,
    read: Result<String, String>,
) -> (String, bool) {
    match read {
        Ok(ccy) => {
            stated(said, venue, account, &ccy);
            (ccy, false)
        }
        Err(why) => fill_in(said, venue, account, &why),
    }
}

/// **The currency a venue's FACTS lister reads for itself** (oanda's `GET /summary` beside its instrument list):
/// with credentials it is [`source_currency`]; with none nothing was asked, so there is no silent API to assume
/// over and the read's own refusal ("no usable credentials") stands.
///
/// # Errors
/// The read's own words, when the account has no credentials to ask with.
pub(crate) fn lister_currency(
    said: &AssumedSaid,
    venue: &str,
    account: &str,
    credentialed: bool,
    read: Result<String, String>,
) -> Result<String, String> {
    if credentialed { Ok(source_currency(said, venue, account, read).0) } else { read }
}

#[path = "assumed_ccy_tests.rs"]
#[cfg(test)]
mod assumed_ccy_tests;
