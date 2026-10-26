//! `RemoteRollingFamily` — a `vike_data::live::SymbolResolver` answered by the gatewayhub: the
//! recorder's polymarket rotating-family row without the bridge.
//!
//! * **The offline grammar check runs FIRST and locally.** [`RemoteRollingFamily::new`] calls
//!   `vike_catalog::polymarket_family_minutes` before anything else, so a malformed family is refused
//!   at startup with the SAME words the bridge's `window_spec_for` returns — with the gatewayhub down,
//!   and without a socket.
//! * **A transport failure is `Err`, never an empty set** (drift register R12). The trait's contract
//!   is that `Err` means UNKNOWN and the caller leaves its streams exactly as they are; an empty set is
//!   a statement ("nothing should be streaming") and DOES unsubscribe every live book. A dead link,
//!   a daemon without `gw_family`, an unreadable reply and the resolver's own failure are all `Err`.
//!   The resolver's own failure text arrives unchanged.
//! * **One connection per resolver.** The daemon keeps one `RollingFamily` per `(connection, family)`
//!   and the planner behind it is stateful, so this value holds its connection across `desired` calls.
//!   A call on a HELD connection that fails at the transport is retried ONCE on a fresh one (the
//!   daemon may have reaped an idle connection; a resolve is read-only, so the repeat is harmless); a
//!   failure on a fresh connection is the answer.
//! * **A resolve is waited for at most [`RESOLVE_REPLY_BOUND`]**, the 30 s the in-process resolver's
//!   Gamma agent had as its global timeout, and NOT the 300 s idle bound a REST-backed verb gets
//!   (`proto::ReplyWait::Bounded`). The caller is the recorder's tick, which reads its stop flag only
//!   AFTER the tick returns, so a wedged daemon (one that accepted and stopped answering) must cost
//!   the stop path what the Gamma call used to, not ten times that. A timeout is `Err` like every
//!   other transport failure (the caller leaves its streams exactly as they are and asks again next
//!   tick), the connection is dropped, and the next call dials a fresh one.

use std::collections::BTreeSet;
use std::time::Duration;

use vike_data::live::SymbolResolver;
use vike_hub_proto::auth::Scope;

use crate::client::{GatewayClient, GatewayLink, gateway_text, unexpected};
use crate::proto::{FEATURE_GW_FAMILY, REPLY_IDLE_TIMEOUT, Request, Response};

/// The venue whose recorder rows rotate their symbols through a family.
pub const FAMILY_VENUE: &str = "polymarket";

/// The longest [`RemoteRollingFamily::desired`] waits for the daemon's reply: the global timeout of
/// the Gamma agent the in-process resolver ran on (`crates/bridges/polymarket/src/egress.rs`'s
/// `agent`), which is what `crates/vike-datahub/src/recorder.rs`'s `TOTAL_STOP_BUDGET_SECS` states as
/// the tick prefix a stop signal waits out. A resolve that is slow but alive past it is an `Err` for
/// this tick (benign: nothing is unsubscribed) and is asked again on the next; narrower than the
/// link's own `reply_idle` only ever helps, never widens it.
pub const RESOLVE_REPLY_BOUND: Duration = Duration::from_secs(30);

const _: () = assert!(
    RESOLVE_REPLY_BOUND.as_secs() < REPLY_IDLE_TIMEOUT.as_secs(),
    "the family resolve is bounded tighter than a REST-backed verb in general"
);

/// A rotating polymarket family, resolved by the gatewayhub.
#[derive(Debug)]
pub struct RemoteRollingFamily {
    link: GatewayLink,
    family: String,
    client: Option<GatewayClient>,
    reply_bound: Duration,
}

impl RemoteRollingFamily {
    /// A resolver for `family` (e.g. `btc-updown-5m`). `Err` is the offline grammar refusal, byte for
    /// byte what the in-process `RollingFamily::new` returns for it. Dials nothing. The reply wait
    /// is [`RESOLVE_REPLY_BOUND`].
    pub fn new(link: GatewayLink, family: &str) -> Result<Self, String> {
        vike_catalog::polymarket_family_minutes(family)?;
        Ok(Self {
            link,
            family: family.to_string(),
            client: None,
            reply_bound: RESOLVE_REPLY_BOUND,
        })
    }

    /// The same resolver waiting at most `bound` for a reply instead of [`RESOLVE_REPLY_BOUND`] - a
    /// test's tool, so a wedged daemon is watched in milliseconds. (The link's own `reply_idle` still
    /// applies when it is shorter.)
    #[must_use]
    pub fn with_reply_bound(mut self, bound: Duration) -> Self {
        self.reply_bound = bound;
        self
    }

    /// A connection ready to ask, with the family feature checked.
    fn fresh(&self) -> Result<GatewayClient, String> {
        let client = GatewayClient::connect(&self.link, Scope::Read)
            .map_err(|e| gateway_text(self.link.addr(), &e))?;
        client
            .require_feature(FEATURE_GW_FAMILY)
            .map_err(|e| gateway_text(self.link.addr(), &e))?;
        Ok(client)
    }
}

impl SymbolResolver for RemoteRollingFamily {
    fn group(&self) -> Option<&str> {
        Some(&self.family)
    }

    fn desired(&mut self, now_ms: i64) -> Result<BTreeSet<String>, String> {
        let request = Request::ResolveFamily {
            venue: FAMILY_VENUE.to_string(),
            family: self.family.clone(),
            now_ms,
        };
        let held = self.client.take();
        let reused = held.is_some();
        let mut client = match held {
            Some(c) => c,
            None => self.fresh()?,
        };
        let mut result = client.exchange_within(&request, self.reply_bound);
        // A timeout is not a stale connection (that fails at once): repeating it would double the wait on
        // a wedged daemon.
        let stale = |r: &std::io::Result<Response>| {
            r.as_ref().is_err_and(|e| e.kind() != std::io::ErrorKind::TimedOut)
        };
        if reused && stale(&result) {
            // The held connection was stale. One repeat on a fresh one decides.
            client = self.fresh()?;
            result = client.exchange_within(&request, self.reply_bound);
        }
        match result {
            Ok(Response::FamilyResolved { symbols }) => {
                self.client = Some(client);
                Ok(symbols.into_iter().collect())
            }
            Ok(Response::Error(text)) => {
                // The resolver's own failure: forwarded unchanged, and the connection is still good.
                self.client = Some(client);
                Err(text)
            }
            Ok(other) => Err(gateway_text(
                self.link.addr(),
                &unexpected(client.addr(), "FamilyResolved", &other),
            )),
            Err(e) => Err(gateway_text(self.link.addr(), &e)),
        }
    }
}
