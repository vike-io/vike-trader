//! Per-verb tables: which socket class a verb opens, which scope may send it, how long its reply may
//! stay silent and what it is called.
//!
//! They live in this crate rather than in the daemon because the daemon (`vike-gatewayhub`, layer
//! 65) and every client are separate binaries that must agree: a table kept in one would be a second
//! copy in the other, and the first divergence would be a verb BOTH refuse. Every classifier here is
//! an EXHAUSTIVE match with no `_` arm, so a new wire verb fails to COMPILE until somebody says
//! which class it opens, which scope may send it, who bounds its reply and what it is called; WHICH
//! arm was chosen is pinned by this module's tests (`proto_tests.rs`).

use vike_hub_proto::auth::Scope;

use super::{FetchWhat, GATEWAYHUB_PLANE_SENTINEL, Request};

/// Which SOCKET CLASS a request opens or belongs to — the split the module doc of `proto` draws.
///
/// A daemon reads the first post-auth frame, asks [`surface_of`], and either serves the one
/// positional verb (`Rpc`) or switches the connection into a push mode (`Stream`, `Feed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// A pre-auth handshake frame: `Hello`, `Auth`. Not a verb at all.
    Handshake,
    /// A positional request/reply verb, including the multi-frame [`Request::Fetch`] family.
    Rpc,
    /// Opens the duplex `stream` class ([`Request::DataOpen`]).
    Stream,
    /// Opens the server-push options `feed` class ([`Request::OptionsFeedOpen`]).
    Feed,
    /// Opens the duplex `trade` lease class ([`Request::TradeOpen`]).
    Trade,
}

/// Classify one request onto its socket class. **EXHAUSTIVE — no `_` arm, deliberately.**
pub fn surface_of(request: &Request) -> Surface {
    match request {
        Request::Hello { .. } | Request::Auth { .. } => Surface::Handshake,
        Request::DataOpen { .. } => Surface::Stream,
        Request::OptionsFeedOpen { .. } => Surface::Feed,
        Request::TradeOpen { .. } => Surface::Trade,
        Request::Ping
        | Request::Fetch { .. }
        | Request::FetchCredentialed { .. }
        | Request::FetchCancel { .. }
        | Request::CatalogDescribe { .. }
        | Request::OptionsRest { .. }
        | Request::ResolveFamily { .. }
        | Request::CredentialPresence { .. }
        | Request::GwStatus
        | Request::ArmingQuery
        | Request::PreflightQuery { .. } => Surface::Rpc,
    }
}

/// A request's variant name, for a log line or a refusal — never its PAYLOAD. Both ends log by it;
/// exhaustive for the same reason [`surface_of`] is.
pub fn request_kind(request: &Request) -> &'static str {
    match request {
        Request::Hello { .. } => "Hello",
        Request::Auth { .. } => "Auth",
        Request::Ping => "Ping",
        Request::DataOpen { .. } => "DataOpen",
        Request::OptionsFeedOpen { .. } => "OptionsFeedOpen",
        Request::Fetch { .. } => "Fetch",
        Request::FetchCredentialed { .. } => "FetchCredentialed",
        Request::FetchCancel { .. } => "FetchCancel",
        Request::CatalogDescribe { .. } => "CatalogDescribe",
        Request::OptionsRest { .. } => "OptionsRest",
        Request::ResolveFamily { .. } => "ResolveFamily",
        Request::CredentialPresence { .. } => "CredentialPresence",
        Request::GwStatus => "GwStatus",
        Request::ArmingQuery => "ArmingQuery",
        Request::PreflightQuery { .. } => "PreflightQuery",
        Request::TradeOpen { .. } => "TradeOpen",
    }
}

/// How long a verb's reply may legitimately stay SILENT — the class the client's `exchange` turns into
/// the socket's idle read bound for that one request. A CLIENT policy, not a wire fact: no frame
/// carries it and the daemon reads none, so it moves without a version bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyWait {
    /// A pre-auth handshake frame: nothing is computed, so the handshake's own short deadline.
    Handshake,
    /// A pure LOOKUP: a table read, a presence word, a counter snapshot, an open acknowledgement. The
    /// daemon touches no venue to answer, so the idle bound is short
    /// ([`crate::proto::LOOKUP_REPLY_TIMEOUT`]) and a wedged daemon is called dead in seconds.
    Lookup,
    /// The SERVER bounds the work before the reply, and the work calls a venue: one REST call at the
    /// bridges' own HTTP timeout (an options chain, a family resolve). A finite idle bound is safe
    /// ([`crate::proto::REPLY_IDLE_TIMEOUT`]).
    Bounded,
    /// A PROGRESS-framed reply (the fetch family): the idle bound is per frame and per request,
    /// `max(FETCH_STALL_FLOOR, 3 × FetchProgress.next_in_ms)`, so a fetch that keeps emitting frames
    /// is never clipped (drift register R8) and a vanished server is still called dead.
    Progress,
}

/// Classify one request's [`ReplyWait`]. **EXHAUSTIVE — no `_` arm, deliberately**: a new verb fails
/// to COMPILE until somebody says who bounds its work, so it cannot silently inherit a bound that
/// clips it or none that would park its caller.
pub fn reply_wait(request: &Request) -> ReplyWait {
    match request {
        Request::Hello { .. } | Request::Auth { .. } => ReplyWait::Handshake,
        Request::Fetch { .. } | Request::FetchCredentialed { .. } => ReplyWait::Progress,
        Request::Ping
        | Request::DataOpen { .. }
        | Request::OptionsFeedOpen { .. }
        | Request::FetchCancel { .. }
        | Request::CatalogDescribe { .. }
        | Request::CredentialPresence { .. }
        | Request::GwStatus
        | Request::ArmingQuery
        | Request::TradeOpen { .. } => ReplyWait::Lookup,

        // A preflight leg reads a venue clock and makes an authenticated read: seconds, bounded by
        // the daemon, never a table read.
        Request::OptionsRest { .. }
        | Request::ResolveFamily { .. }
        | Request::PreflightQuery { .. } => ReplyWait::Bounded,
    }
}

/// The scope a verb requires — the ONE authority for the read/write split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbScope {
    /// A pre-auth handshake frame — how a connection GETS a scope, so it cannot require one.
    Handshake,
    /// Servable by a connection authenticated under [`Scope::Read`] or [`Scope::Write`].
    Read,
    /// Servable ONLY by a connection authenticated under [`Scope::Write`].
    Write,
    /// Servable ONLY by a connection authenticated under [`Scope::Trade`]: the trade surface (a lease
    /// per `(client, account)`, submits, cancels, reconcile, and the arming and preflight queries that
    /// read the daemon's account table). No `_` arm decides who may send an order.
    Trade,
}

/// Classify one request's required scope. **EXHAUSTIVE — no `_` arm, deliberately**: a default of
/// `Read` would leak a credential, and a default of `Write` would silently break a read for
/// read-scope clients. Neither is a decision a wildcard should make.
///
/// - **Cheap verbs are [`VerbScope::Read`]**, the datahub's market-data rule
///   (`docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`) carried over the hop:
///   a `stream` open, a catalog describe, the catalog and option-index fetches, the options calls, a
///   family resolve, a presence word and the status read PUBLIC venue endpoints or the daemon's own
///   state, no reply carries a credential, and what each costs the venue is bounded by something the
///   request does not move: a `stream` and an options feed by the session, subscription, key, feed
///   and queue caps plus (a feed) the name length, charset and count of `OptionsFeedSpec::refusal`;
///   a catalog or option-index fetch by the venue's own universe (one call or a fixed pager); a
///   family resolve by the per-connection planner cap.
/// - **The fetches that walk a CLIENT-NAMED WINDOW are [`VerbScope::Write`]** — [`FetchWhat::Klines`],
///   [`FetchWhat::Funding`] and [`FetchWhat::Ticks`], however keyless the venue: the venue's pager
///   (or, for ticks, one CDN request per hour file) walks as much history as the window names, from
///   this box's address, and the daemon holds the answer whole before it pages it. The datahub's
///   `Backfill` was `Scope::Write` for exactly this work (`crates/vike-datahub/src/backfill.rs`'s
///   module doc; decisions 0097 and 0101) and only its bounded chart seed was Read, so the trust
///   boundary is carried over the hop and not widened: a `Scope::Read` key (a desktop's) must not be able to make this daemon spend a
///   venue's request budget, or the Bybit ten-minute ban decision 0164 cites, on a window it names.
///   The datahub holds both keys, so its chart seed (Read at the datahub's own door, a bounded
///   window it chooses) fetches here under its Control key like the rest of its history. A window
///   cap is still not a daemon constant for klines and funding (a tick fetch has
///   `GW_TICKS_MAX_SPAN_MS`); the datahub cuts its backfill into chunks before it asks, and a cap
///   belongs with the chunk sizes it asks for.
/// - **[`Request::FetchCredentialed`] is [`VerbScope::Write`]**: the daemon spends a credential it
///   holds on the client's timing and terms. A `Scope::Read` key (a desktop's) must never be able to
///   make this daemon authenticate as its operator — decision 0062's rule 3 and 0097's verdict 3,
///   carried over the hop.
/// - **[`Request::FetchCancel`] is [`VerbScope::Read`]** because it can name only a fetch THIS
///   connection started; a connection that started a window fetch is `Write` already. A
///   cross-connection cancel (decision 0101's posture: it ends work another connection started) is
///   a later verb and `Write`.
/// - **[`Request::CredentialPresence`] is [`VerbScope::Read`]**: it reports a presence WORD and never
///   uses or returns a value (decision 0102).
pub fn required_scope(request: &Request) -> VerbScope {
    match request {
        Request::Hello { .. } | Request::Auth { .. } => VerbScope::Handshake,

        Request::Ping
        | Request::DataOpen { .. }
        | Request::OptionsFeedOpen { .. }
        | Request::FetchCancel { .. }
        | Request::CatalogDescribe { .. }
        | Request::OptionsRest { .. }
        | Request::ResolveFamily { .. }
        | Request::CredentialPresence { .. }
        | Request::GwStatus => VerbScope::Read,

        Request::Fetch { what, .. } => fetch_scope(what),

        Request::FetchCredentialed { .. } => VerbScope::Write,

        // The trade class: a lease, and the two queries that read the daemon's account rows. A
        // `Scope::Read`/`Write` key (a desktop's, the datahub's) must never reach them.
        Request::ArmingQuery | Request::PreflightQuery { .. } | Request::TradeOpen { .. } => {
            VerbScope::Trade
        }
    }
}

/// The scope a keyless [`Request::Fetch`] of `what` requires. **EXHAUSTIVE — no `_` arm**: a new lane
/// fails to COMPILE until somebody says whether its cost is bounded by the venue's own universe
/// (`Read`) or by a window the client names (`Write`). See [`required_scope`].
fn fetch_scope(what: &FetchWhat) -> VerbScope {
    match what {
        FetchWhat::Klines { .. } | FetchWhat::Funding { .. } | FetchWhat::Ticks { .. } => {
            VerbScope::Write
        }
        FetchWhat::CatalogList { .. }
        | FetchWhat::CatalogSearch { .. }
        | FetchWhat::OptionIndex { .. } => VerbScope::Read,
    }
}

/// Whether a connection authenticated under `authed` may send a verb requiring `needed`.
///
/// [`VerbScope::Handshake`] is `false` on purpose: those frames belong to the pre-auth phase, and a
/// SECOND `Hello`/`Auth` on an established connection is a protocol error (a scope that can be
/// re-negotiated after the fact is not a ceiling).
///
/// ⚠ **[`Scope::Account`] admits NOTHING here, deliberately**: the gatewayhub's handshake refuses a
/// scope it holds no key for before any verb. Listing Account beside `Write` would grant this
/// service's credentialed fetch to a capability it cannot authenticate — the enum's own *not a
/// superset*.
///
/// ⚠ **[`Scope::Trade`] is not a superset either, in BOTH directions**: it admits [`VerbScope::Trade`]
/// and nothing else (not a read, not the credentialed fetch, not a window fetch), and neither
/// [`Scope::Read`] nor [`Scope::Write`] admits [`VerbScope::Trade`] — a control key places no order
/// through the gatewayhub.
pub fn scope_admits(authed: Scope, needed: VerbScope) -> bool {
    match needed {
        VerbScope::Handshake => false,
        VerbScope::Read => matches!(authed, Scope::Read | Scope::Write),
        VerbScope::Write => matches!(authed, Scope::Write),
        VerbScope::Trade => matches!(authed, Scope::Trade),
    }
}

/// Whether a `Welcome`'s feature list names the gatewayhub — the check a client makes BEFORE it signs.
pub fn welcome_is_gatewayhub(features: &[String]) -> bool {
    features.iter().any(|f| f == GATEWAYHUB_PLANE_SENTINEL)
}

/// The refusal a client prints when the daemon it dialled is not the gatewayhub: it names the
/// address, the missing sentinel and what the peer DID advertise (bounded), so a wrong port reads as
/// a wrong port rather than as an authentication failure.
pub fn plane_mismatch_message(addr: &str, features: &[String]) -> String {
    const SHOWN: usize = 8;
    let shown: Vec<&str> = features.iter().take(SHOWN).map(String::as_str).collect();
    let more = features.len().saturating_sub(SHOWN);
    let tail = if more > 0 { format!(", … {more} more") } else { String::new() };
    format!(
        "{addr} is not a vike-gatewayhub: its Welcome does not advertise \
         {GATEWAYHUB_PLANE_SENTINEL:?} (it advertised [{}{tail}]). Point the client at the \
         gatewayhub (`config.gatewayhub_addr`); nothing was signed or sent beyond the Hello.",
        shown.join(", ")
    )
}
