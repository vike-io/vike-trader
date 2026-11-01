//! **IBKR's ONE session**: the product and the tier every IBKR connection on a box rides —
//! orders, data, reconcile and the gateway process alike
//! (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`).
//!
//! One IBKR account admits ONE session, and the two products evict each other
//! (`docs/ops/ibkr-socket-gateway.md` §5), so a box gets ONE choice, read from two kinds of rows in
//! the settings database and nothing else:
//!
//! * **the tier** — the tier of the box's ACTIVE non-paper ibkr `account` rows, over EVERY label
//!   (the venue-wide scope, `crates/vike-venues/src/venue_tier_scope.rs`'s `TierScope`);
//! * **the product** — that tier's `venue.ibkr.<tier>.backend` row ([`IbkrBackend`]; no row, or a
//!   blank one, is [`IbkrBackend::Socket`]).
//!
//! [`ibkr_session`] is the ONE function that turns those into an answer — [`IbkrSession::Off`],
//! [`IbkrSession::Pair`] or [`IbkrSession::Conflict`]. The mount and the two `vike-cli` login verbs
//! call it, so the rule is spelled nowhere else. It is pure: the caller reads the rows, this module
//! decides.
//!
//! [`IbkrBackend`] is the product WORD, vocabulary like [`AccountTier`] (decision 0129's move is the
//! precedent): it came down from `crates/bridges/vike-ibkr/src/config.rs`, so a caller that needs
//! the word links no bridge.

use std::fmt;

use vike_model::AccountTier;

/// **Which IBKR product a tier's connections speak** — the `venue.ibkr.<tier>.backend` row's word.
///
/// `Socket` is IB Gateway (or TWS) over the ibapi wire protocol; `Cpapi` is the Client Portal
/// Gateway's REST+WS. `Oauth` parses and is wired to nothing: every mount refuses it (the bridge's
/// `LiveRefusal::BackendUnwired`, and a stub connect on the demo tier).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum IbkrBackend {
    /// IB Gateway / TWS over ibapi. **The default**: no backend row means this.
    #[default]
    Socket,
    /// The Client Portal Gateway (REST + WS).
    Cpapi,
    /// IBKR's OAuth Web API. Parses; nothing connects through it.
    Oauth,
}

impl IbkrBackend {
    /// Every backend, in declaration order.
    pub const ALL: [IbkrBackend; 3] = [IbkrBackend::Socket, IbkrBackend::Cpapi, IbkrBackend::Oauth];

    /// **The ONE parser of the backend word**: `socket`, `cpapi` or `oauth`, trimmed and
    /// case-insensitive; an EMPTY word is `Socket` (a blank row reads as no row, as the gateway's
    /// other rows do). Anything else is `None` — the caller's refusal, never a guess.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "" | "socket" => Some(IbkrBackend::Socket),
            "cpapi" => Some(IbkrBackend::Cpapi),
            "oauth" => Some(IbkrBackend::Oauth),
            _ => None,
        }
    }

    /// The row spelling — what [`IbkrBackend::parse`] reads back.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IbkrBackend::Socket => "socket",
            IbkrBackend::Cpapi => "cpapi",
            IbkrBackend::Oauth => "oauth",
        }
    }
}

impl fmt::Display for IbkrBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// **The box's IBKR session** — what [`ibkr_session`] answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IbkrSession {
    /// No ACTIVE non-paper ibkr row, of any label: no session, no gateway, every ibkr account
    /// paper.
    Off,
    /// The ONE pair every IBKR connection rides. `tier` is `Demo` or `Live`, never `Paper`.
    Pair {
        /// The product: that tier's `venue.ibkr.<tier>.backend` row.
        backend: IbkrBackend,
        /// The tier the active rows state.
        tier: AccountTier,
    },
    /// Active rows at `demo` AND `live`, under any labels: every ibkr account mounts paper
    /// (`ArmingBlock::TierConflict`) and no gateway starts. No pick (decision 0119).
    Conflict,
}

/// **A backend row nobody can read** — the resolved tier's `venue.ibkr.<tier>.backend` row holds a
/// word [`IbkrBackend::parse`] refuses. The word is a setting, never a secret, so it is echoed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnIbkrBackend {
    /// The tier whose row was read.
    pub tier: AccountTier,
    /// The word the row holds, as written.
    pub word: String,
}

impl fmt::Display for NotAnIbkrBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the `venue.ibkr.{}.backend` row is `{}`, which is not an IBKR backend (socket, cpapi \
             or oauth)",
            self.tier, self.word
        )
    }
}

impl std::error::Error for NotAnIbkrBackend {}

/// **THE IBKR session resolver** (decision 0140, item 1).
///
/// * `active_tiers` — the `tier` of every ACTIVE ibkr `account` row, of EVERY label (an inactive
///   row states nothing and must not be passed). `paper` rows are ignored: a paper row never
///   conflicts.
/// * `backend_word` — the RESOLVED tier's `venue.ibkr.<tier>.backend` row, `None` when there is no
///   row. It is called at most once, and only for a [`IbkrSession::Pair`]: an `Off` or a `Conflict`
///   box has no tier whose row means anything, so neither reads one.
///
/// No non-paper tier → `Off`; exactly one (several rows of it are fine) → `Pair` with that tier's
/// backend; two → `Conflict`. A backend row that does not parse is `Err`, never a default.
///
/// # Errors
///
/// [`NotAnIbkrBackend`] when the resolved tier's backend row holds a word that is not a backend.
pub fn ibkr_session<'a>(
    active_tiers: impl IntoIterator<Item = AccountTier>,
    backend_word: impl FnOnce(AccountTier) -> Option<&'a str>,
) -> Result<IbkrSession, NotAnIbkrBackend> {
    let mut stated: Option<AccountTier> = None;
    for tier in active_tiers {
        if tier == AccountTier::Paper {
            continue;
        }
        match stated {
            None => stated = Some(tier),
            Some(seen) if seen == tier => {}
            Some(_) => return Ok(IbkrSession::Conflict),
        }
    }
    let Some(tier) = stated else { return Ok(IbkrSession::Off) };
    let word = backend_word(tier).unwrap_or("");
    let backend = IbkrBackend::parse(word)
        .ok_or_else(|| NotAnIbkrBackend { tier, word: word.to_string() })?;
    Ok(IbkrSession::Pair { backend, tier })
}

#[path = "ibkr_session_tests.rs"]
#[cfg(test)]
mod ibkr_session_tests;
