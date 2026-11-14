//! Per-verb tables: which socket class a verb opens, which scope may send it, how long its reply may
//! stay silent and what it is called.
//!
//! They live in this crate rather than in the daemon because the daemon (`vike-gatewayhub`, layer
//! 65) and every client are separate binaries that must agree: a table kept in one would be a second
//! copy in the other, and the first divergence would be a verb BOTH refuse. Every classifier here is
//! an EXHAUSTIVE match with no `_` arm, so a new wire verb fails to COMPILE until somebody says
//! which class it opens, which scope may send it, who bounds its reply and what it is called; WHICH
//! arm was chosen is pinned by this module's tests (`proto_tests.rs`).

#[cfg(doc)]
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::auth::VerbScope;
use vike_hub_protocol::conn::ReplyWait;

use super::{FetchWhat, GATEWAYHUB_PLANE_SENTINEL, Request};

/// Which SOCKET CLASS a request opens or belongs to — the split the module doc of `proto` draws.
///
/// A daemon reads the first post-auth frame, asks [`surface_of`], and either serves the one
/// positional verb (`Rpc`) or switches the connection into a push mode (`Stream`, `FeedOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// A pre-auth handshake frame: `Hello`, `Auth`. Not a verb at all.
    Handshake,
    /// A positional request/reply verb, including the multi-frame [`Request::Fetch`] family.
    Rpc,
    /// Opens the duplex `stream` class ([`Request::DataOpen`]).
    Stream,
    /// Opens the server-push options `feed` class ([`Request::OptionsFeedOpen`]).
    FeedOptions,
    /// Opens the duplex `trade` lease class ([`Request::TradeOpen`]).
    Trade,
}

/// Classify one request onto its socket class. **EXHAUSTIVE — no `_` arm, deliberately.**
pub fn surface_of(request: &Request) -> Surface {
    match request {
        Request::Hello { .. } | Request::Auth { .. } => Surface::Handshake,
        Request::DataOpen { .. } => Surface::Stream,
        Request::OptionsFeedOpen { .. } => Surface::FeedOptions,
        Request::TradeOpen { .. } => Surface::Trade,
        Request::Ping
        | Request::Fetch { .. }
        | Request::FetchCredentialed { .. }
        | Request::FetchCancel { .. }
        | Request::CatalogDescribe { .. }
        | Request::OptionsRest { .. }
        | Request::ResolveFamily { .. }
        | Request::CredentialPresence { .. }
        | Request::GatewayStatus
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
        Request::GatewayStatus => "GatewayStatus",
        Request::ArmingQuery => "ArmingQuery",
        Request::PreflightQuery { .. } => "PreflightQuery",
        Request::TradeOpen { .. } => "TradeOpen",
    }
}

/// Classify one request's [`ReplyWait`] (the one class every hub client shares; the client's
/// `exchange` turns it into the socket's idle read bound for that one request: a `Lookup` under
/// [`crate::proto::LOOKUP_REPLY_TIMEOUT`], a `Bounded` under [`crate::proto::REPLY_IDLE_TIMEOUT`], a
/// `Progress` per frame). No verb here is `Unbounded`. **EXHAUSTIVE — no `_` arm, deliberately**: a
/// new verb fails to COMPILE until somebody says who bounds its work, so it cannot silently inherit a
/// bound that clips it or none that would park its caller.
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
        | Request::GatewayStatus
        | Request::ArmingQuery
        | Request::TradeOpen { .. } => ReplyWait::Lookup,

        // A preflight leg reads a venue clock and makes an authenticated read: seconds, bounded by
        // the daemon, never a table read.
        Request::OptionsRest { .. }
        | Request::ResolveFamily { .. }
        | Request::PreflightQuery { .. } => ReplyWait::Bounded,
    }
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
///   `GATEWAY_TICKS_MAX_SPAN_MS`); the datahub cuts its backfill into chunks before it asks, and a cap
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
        | Request::GatewayStatus => VerbScope::Read,

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
