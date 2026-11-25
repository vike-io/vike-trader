//! Hyperliquid REST transport — `ureq` blocking `POST /info` (unsigned) + `POST /exchange` (signed
//! body). One thin client over one network's REST row ([`crate::config::base_url`]:
//! `/info` and `/exchange` hang off it).
//!
//! - `/info` posts a plain `{type, …}` body — **keyless** (reads take a `user` address, never a
//!   signature) and idempotent, so a transient throttle / 5xx MAY be retried in-transport (bounded).
//! - `/exchange` posts `{action, nonce, signature, vaultAddress?}` built by [`crate::signing`] and is
//!   sent **exactly once — never auto-retried**, so a post-send timeout can't double-submit an order.
//!   The caller re-queries instead, keying off
//!   [`vike_bridge_core::transport::ErrorKind::must_requery`] (the ambiguous-timeout sentinel
//!   [`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`] survives to the caller untouched).
//!
//! Both paths share the venue's **IP weight budget** through one [`RateGate`] built by
//! [`crate::transport::ratelimit::ip_weight_gate`] from `vike_venues::venue_rate_limits::HYPERLIQUID` — the
//! budget is a venue FACT and lives in that table, never as a local const here. This module owns
//! only the per-endpoint weight SCHEDULE that charges it (a cost map, not a budget): exchange
//! actions cost `1 + floor(batch_len / 40)`; info reads cost 2
//! (`l2Book`/`clearinghouseState`/`orderStatus`/`spotClearinghouseState`/`allMids`), 60 (`userRole`),
//! or 20 (everything else). The gate BLOCKS on throttle (venue thread, never the core fold — matches
//! [`vike_bridge_core::ratelimit`]) with a single warn. rustls only (the shared
//! [`vike_transport::http`] agent). HL has no numeric `{code,msg}` envelope: a 200 body is returned
//! verbatim (the caller's `event_mapper`/`recon` reads its `{status:"ok"|"err", …}`); a non-2xx maps
//! to a [`VenueApiError`] carrying the HTTP status as `code` so
//! [`vike_bridge_core::transport::ErrorKind`] classifies it (429 → `RateLimited`, 5xx → `ServerError`).

pub mod consts;
pub mod ratelimit;

use std::time::Duration;

#[cfg(feature = "execution")]
use serde::Serialize;

use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::transport::reply::{
    RestReplyGatewayFirst, RestReplyNote, RestReplySpec, classify_rest_reply,
};
use vike_bridge_core::transport::{ErrorKind, VenueApiError};

use crate::transport::consts::VENUE;
use vike_model::VenueNetwork;
// The `/exchange` (signed) half of this transport is exec-plane: a feeds-only build
// (`default-features = false`) keeps the keyless `/info` reads and compiles none of it, so the
// signer types below — and the k256 stack behind them — never enter that binary.
#[cfg(feature = "execution")]
use crate::signing::action::Action;
#[cfg(feature = "execution")]
use crate::signing::{Signature, Signer};

/// The pauses before an idempotent `/info` read's retries on a transient (rate-limit / server) failure: a
/// short escalating backoff (venue thread, off the core fold), 200 ms, then 400 ms.
/// So one initial attempt and up to two retries; `/exchange` is ALWAYS one attempt (never here).
const INFO_RETRY_PAUSES: [Duration; 2] = [Duration::from_millis(200), Duration::from_millis(400)];

/// IP weight of one `/info` request, from its `type` (research §9). The light reads are 2, `userRole`
/// is 60, everything else is 20. Pure — driven off the request body's `type` string.
fn info_weight(request_type: &str) -> usize {
    match request_type {
        "l2Book" | "clearinghouseState" | "orderStatus" | "spotClearinghouseState" | "allMids" => 2,
        "userRole" => 60,
        _ => 20,
    }
}

/// IP weight of one `/exchange` action: `1 + floor(batch_len / 40)` (research §9) — a single order is
/// weight 1; batching amortizes (a 40-item batch is one extra weight).
#[cfg(feature = "execution")]
fn exchange_weight(batch_len: usize) -> usize {
    1 + batch_len / 40
}

/// The number of items an [`Action`] batches — the `batch_len` [`exchange_weight`] meters. A `modify`
/// is a single cancel-replace and an `updateLeverage` one asset (1 each); the plural actions carry
/// their vector length.
#[cfg(feature = "execution")]
fn action_batch_len(action: &Action) -> usize {
    match action {
        Action::Order(a) => a.orders.len(),
        Action::Cancel(a) => a.cancels.len(),
        Action::CancelByCloid(a) => a.cancels.len(),
        Action::Modify(_) | Action::UpdateLeverage(_) => 1,
        Action::BatchModify(a) => a.modifies.len(),
    }
}

/// The `/exchange` request body: `{action, nonce, signature, vaultAddress?}`. `action` is borrowed
/// and serialized inline (its own `#[serde(tag="type")]` map); `vaultAddress` is omitted (not `null`)
/// when absent. Field order here is cosmetic — HL reconstructs the signed hash from the msgpack of
/// `action` (see [`crate::signing`]), not from this JSON's key order.
#[cfg(feature = "execution")]
#[derive(Serialize)]
struct ExchangeBody<'a> {
    action: &'a Action,
    nonce: u64,
    signature: Signature,
    #[serde(rename = "vaultAddress", skip_serializing_if = "Option::is_none")]
    vault_address: Option<String>,
}

/// Build (and sign) the `/exchange` request body. Split out so the exact wire shape is unit-testable
/// with a fixed signer + nonce and NO network. `vault`, when present, is lowercased `0x`-hex (HL
/// requires lowercased address fields) and passed to the signer so the signature covers the same
/// vault marker the body advertises.
///
/// ⚠ `pub` so the shape is observable from OUTSIDE this file. `vaultAddress` is the key that names
/// WHICH BOOK an order lands on: misspell it and the venue sees no vault field at all, the request
/// stays well-formed, and the order is placed on the signer's own account instead of the vault's.
/// The in-file test asserting that string is rewritten by the same pass that would break it, so
/// `tests/signed_payload_fixtures.rs` freezes these bytes under `fixtures/` instead.
#[cfg(feature = "execution")]
pub fn exchange_body(
    action: &Action,
    signer: &Signer,
    nonce: u64,
    vault: Option<[u8; 20]>,
) -> serde_json::Value {
    let signature = signer.sign_l1_action(action, nonce, vault, None);
    let body = ExchangeBody {
        action,
        nonce,
        signature,
        vault_address: vault.map(|v| format!("0x{}", vike_utility::hex::hex_lower(&v))),
    };
    // Infallible: every field is a plain JSON-representable type (strings / ints / bool / vec).
    serde_json::to_value(body).expect("exchange body serializes to JSON")
}

/// Blocking Hyperliquid REST client over one network's `(info, exchange)` host pair, metering both
/// halves through one IP-weight [`RateGate`].
///
/// ⚠ **That gate is this transport's OWN unless a caller replaces it.** [`Self::new`] mints a fresh
/// window every time and the venue's budget is per-IP, so two plainly-constructed transports in one
/// process spend two full budgets while each reports itself inside quota. A unit with several REST
/// paths therefore resolves [`crate::transport::ratelimit::ip_weight_gate`] once and clones it into each
/// transport with [`Self::with_rate_gate`] — for a live start that is the exec thread, the funding
/// poller and the reconcile client (`crate::adapter`'s `ip_gate_and_transport`). Not
/// the WS feeds: they hold no transport of this type and draw no REST weight at all.
pub struct HyperliquidTransport {
    network: VenueNetwork,
    info_url: String,
    // Read only by the `exec`-gated `/exchange` half; still RESOLVED unconditionally (it hangs off
    // the same REST row as `info_url`, and a cfg'd field would fracture `new`).
    #[cfg_attr(not(feature = "execution"), allow(dead_code, clippy::allow_attributes))]
    exchange_url: String,
    agent: ureq::Agent,
    rate_gate: RateGate,
}

impl HyperliquidTransport {
    /// A transport for `network` at the REST base `rest` (that network's `venue_url` row,
    /// `crate::config::base_url`: `/info` and `/exchange` hang off it) with a fresh IP-weight gate — a WHOLE
    /// budget of its own, sized by [`crate::transport::ratelimit::ip_weight_gate`] from the venue table.
    /// Correct for a one-shot caller; a second REST path in the same process must take the FIRST
    /// one's gate through [`Self::with_rate_gate`] rather than mint another (the budget is per-IP, so
    /// two windows spend two caps). `network` signs ([`Self::network`]); the row was looked up at the
    /// same network's type, so the two cannot disagree.
    pub fn new(network: VenueNetwork, rest: &str) -> Self {
        use crate::transport::consts::{EXCHANGE_PATH, INFO_PATH};
        HyperliquidTransport {
            network,
            info_url: format!("{rest}{INFO_PATH}"),
            exchange_url: format!("{rest}{EXCHANGE_PATH}"),
            agent: vike_transport::http::blocking_agent(),
            rate_gate: crate::transport::ratelimit::ip_weight_gate(),
        }
    }

    /// The `/exchange` url this transport signs to (a user-signed action posts here too).
    #[cfg(feature = "execution")]
    pub(crate) fn exchange_url(&self) -> &str {
        &self.exchange_url
    }

    /// Replace this transport's own IP-weight gate with a shared one, DISCARDING the fresh window
    /// [`Self::new`] built. Pass a clone of the handle the unit resolved once from
    /// [`crate::transport::ratelimit::ip_weight_gate`], so every REST path in the process charges the one
    /// window the venue actually meters — the budget is per-IP, not per-client.
    pub fn with_rate_gate(mut self, gate: RateGate) -> Self {
        self.rate_gate = gate;
        self
    }

    /// [`Self::new`], but on a caller-supplied agent — the same seam
    /// `vike_bridge_core::transport::UreqTransport::with_agent` offers, and for the same reason: a
    /// caller may need a timeout other than the shared 30 s one.
    ///
    /// ⚠ The IP-weight gate is this transport's OWN — a fresh full window, since [`Self::new`] is
    /// what builds it, and NOT the one a live start shares across exec/funding/recon. Today's caller
    /// is `crates/vike-gatewayhub/src/trade/adapters/server_time.rs`'s hyperliquid clock read, which
    /// must not park a start for 30 s
    /// on a keyless canary: one `exchangeStatus` read at startup, so its unshared window is a
    /// bounded one-shot rather than a second sustained lane. Chain [`Self::with_rate_gate`] onto
    /// this if it ever becomes a repeating caller. ⚠ An ORDER path must NOT use this: a short timeout there converts a definite
    /// pre-send failure into [`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`], the "may have
    /// landed, must re-query" case `Self::post` (once `post_json`) exists to keep rare.
    pub fn with_agent(network: VenueNetwork, rest: &str, agent: ureq::Agent) -> Self {
        HyperliquidTransport { agent, ..Self::new(network, rest) }
    }

    /// The network this transport targets (mainnet/testnet) — the source of the phantom-agent
    /// `source` byte the signer stamps.
    pub fn network(&self) -> VenueNetwork {
        self.network
    }

    /// This transport's IP-weight gate — whichever window it is riding, which is a fresh one of its
    /// own unless [`Self::with_rate_gate`] replaced it. Clone it into a sibling transport
    /// (`.with_rate_gate(t.rate_gate().clone())`) so both ride one budget, and read it (as
    /// [`crate::account::user_signed::transfer`] and [`crate::account::user_signed::builder_fee`] do) to charge a `/exchange` call this
    /// transport does not itself make.
    pub fn rate_gate(&self) -> &RateGate {
        &self.rate_gate
    }

    /// Keyless `POST /info` (research §8): the `body` is a plain `{type, …}` object (reads that scope
    /// to an account carry a `user` address field, never a signature). Charges the gate by the §9
    /// weight of `body["type"]`, then posts. As an idempotent read it MAY retry a transient throttle /
    /// server error (once more after each of [`INFO_RETRY_PAUSES`]); every other outcome —
    /// including an ambiguous timeout — returns to the caller on the first attempt.
    pub fn info(&self, body: &serde_json::Value) -> Result<serde_json::Value, VenueApiError> {
        self.info_asked(body, &INFO_RETRY_PAUSES)
    }

    /// [`Self::info`] asked ONCE, its answer whatever it is: for a caller that retries the read itself, so the
    /// call is retried in one layer (the feed's background symbology, `crate::feed`'s `with_background_symbology`),
    /// never three times inside each of its own tries.
    pub fn info_once(&self, body: &serde_json::Value) -> Result<serde_json::Value, VenueApiError> {
        self.info_asked(body, &[])
    }

    /// `POST /info`, asked again after each of `pauses` while the answer is a throttle or a server error.
    fn info_asked(
        &self,
        body: &serde_json::Value,
        pauses: &[Duration],
    ) -> Result<serde_json::Value, VenueApiError> {
        let request_type = body.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        let weight = info_weight(request_type);
        let payload = serde_json::to_vec(body)
            .map_err(|e| VenueApiError { code: 0, msg: format!("bad info body: {e}") })?;
        let transient =
            |e: &VenueApiError| matches!(e.kind(), ErrorKind::RateLimited | ErrorKind::ServerError);
        let attempts = pauses.len() as u32 + 1;
        vike_utility::retry::retry_on_pauses(
            pauses,
            |attempt| {
                self.rate_gate.proceed_cost_logged(VENUE, request_type, weight);
                let answer = self.post_with(&self.info_url, &payload, false);
                if let Err(e) = &answer
                    && attempt < attempts
                    && transient(e)
                {
                    tracing::warn!(
                        venue = VENUE, request_type, attempt, error = %e,
                        "info request transient failure; retrying"
                    );
                }
                answer
            },
            |answer| !matches!(answer, Err(e) if transient(e)),
            std::thread::sleep,
        )
    }

    /// Signed `POST /exchange`: build `{action, nonce, signature, vaultAddress?}` (the signature
    /// covers the same nonce/vault), charge the gate `1 + floor(batch_len/40)`, then post **exactly
    /// once**. NEVER auto-retries — a post-send timeout (the ambiguous
    /// [`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`]) or a 429 returns as a
    /// [`VenueApiError`] the caller must resolve by re-querying order status, so a resend can't
    /// double-submit. A 200 body is returned verbatim (the caller reads `{status, response}`).
    #[cfg(feature = "execution")]
    pub fn exchange(
        &self,
        action: &Action,
        signer: &Signer,
        nonce: u64,
        vault: Option<[u8; 20]>,
    ) -> Result<serde_json::Value, VenueApiError> {
        let weight = exchange_weight(action_batch_len(action));
        // Gate BEFORE signing (matches OKX): a throttle wait shouldn't sit between signing and send.
        self.rate_gate.proceed_cost_logged(VENUE, "exchange", weight);
        let body = exchange_body(action, signer, nonce, vault);
        // Infallible (a `serde_json::Value` always re-serializes); mapped to code 0 for total safety.
        let payload = serde_json::to_vec(&body)
            .map_err(|e| VenueApiError { code: 0, msg: format!("bad exchange body: {e}") })?;
        self.post(&self.exchange_url, &payload)
    }

    /// One POST + parse. `2xx` → the parsed JSON body (HL's `{status,response}` envelope is left for
    /// the caller to interpret — the seam split matching Bybit/OKX). A network failure maps to `code:
    /// 0` (definite pre-send) or the ambiguous
    /// [`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`] (a timeout AFTER send may have landed), and
    /// so does any 5xx ([`Self::post_with`]); another non-2xx maps to `code: <http status>` + the raw
    /// body text, so [`ErrorKind`] classifies it.
    ///
    /// `pub(crate)` so the crate's OTHER `/exchange` caller — [`crate::account::user_signed::transfer`]'s user-signed
    /// `usdClassTransfer`, which can't route through [`Self::exchange`] (that one bakes in L1
    /// phantom-agent signing and only accepts the L1 `Action` enum) — reaches the SAME post+classify
    /// path instead of mirroring it. It previously hand-copied this body purely because this method
    /// was private; the audit-T1 classification is far too safety-critical to keep in two copies
    /// inside one crate.
    #[cfg(feature = "execution")]
    pub(crate) fn post(&self, url: &str, body: &[u8]) -> Result<serde_json::Value, VenueApiError> {
        self.post_with(url, body, true)
    }

    /// [`Self::post`], told whether it is a WRITE. `/exchange` calls may leave an order behind any 5xx
    /// (this venue states no code of its own) or behind a 2xx whose body cannot be read, so both are the
    /// ambiguous sentinel; the idempotent `/info` reads have no outcome to be unsure of, and their bounded
    /// 5xx retry in [`Self::info`] is unchanged. `code: 0` on a write's unreadable 2xx said "it never left
    /// this host": a terminal reject of the whole batch, and on a modify no retirement of the old oid, so
    /// its `canceled` ended the live replacement.
    fn post_with(
        &self,
        url: &str,
        body: &[u8],
        write: bool,
    ) -> Result<serde_json::Value, VenueApiError> {
        let sent = self.agent.post(url).header("Content-Type", "application/json").send(body);
        // No key travels to this venue: what a signed `/exchange` request carries is its signature's halves.
        let signed: Option<serde_json::Value> =
            if write { serde_json::from_slice(body).ok() } else { None };
        let signature = signed.as_ref().and_then(|b| b.get("signature"));
        let credentials: Vec<&str> =
            ["r", "s"].into_iter().filter_map(|half| signature?.get(half)?.as_str()).collect();
        classify_rest_reply(sent, write, &REPLY, &credentials, |note| match note {
            RestReplyNote::SendFailed(e) => {
                tracing::warn!(venue = VENUE, error = %e, "REST request failed");
            }
            RestReplyNote::Refused { status, .. } => {
                tracing::warn!(venue = VENUE, status, "REST request failed");
            }
        })
    }
}

/// Hyperliquid's reply rules for the shared engine (`vike_bridge_core::transport::reply`'s
/// `classify_rest_reply`): an error body is plain text or a bare `{status:"err",…}` with no numeric
/// code, so the HTTP status is the `code` (`ErrorKind` maps 429/5xx) and the body's summary the message
/// (`vike_bridge_core::transport::body_summary`): the venue DECLARES plain-text errors (measured live:
/// `HTTP 422 Failed to deserialize the JSON body into the target type`, the user-signed and permissioning
/// smokes' tables), so a short single line is its words and anything longer or a page is its size alone;
/// a write's 5xx (owner ruling B-1) and 3xx (B-3: no agent follows a redirect, so it says nothing about
/// the order) are outcome unknown before the body is read, and so is a write's unreadable 2xx; a read
/// keeps its status.
const REPLY: RestReplySpec = RestReplySpec {
    code: None,
    no_verdict_on_5xx: &[],
    gateway_first: RestReplyGatewayFirst::Never,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
    words: None,
    unknown_body_head: 0,
    plain_text_errors: true,
    verdict_on_5xx: &[],
};

#[path = "transport_tests.rs"]
#[cfg(test)]
mod transport_tests;
