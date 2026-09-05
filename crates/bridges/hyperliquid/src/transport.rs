//! Hyperliquid REST transport — `ureq` blocking `POST /info` (unsigned) + `POST /exchange` (signed
//! body). One thin client over the network host pair ([`crate::config::Network::urls`]).
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
//! [`crate::ratelimit::ip_weight_gate`] from `vike_model::venue_rate_limits::HYPERLIQUID` — the
//! budget is a venue FACT and lives in that table, never as a local const here. This module owns
//! only the per-endpoint weight SCHEDULE that charges it (a cost map, not a budget): exchange
//! actions cost `1 + floor(batch_len / 40)`; info reads cost 2
//! (`l2Book`/`clearinghouseState`/`orderStatus`/`spotClearinghouseState`/`allMids`), 60 (`userRole`),
//! or 20 (everything else). The gate BLOCKS on throttle (venue thread, never the core fold — matches
//! [`vike_bridge_core::ratelimit`]) with a single warn. rustls only (the shared
//! [`vike_bridge_core::http`] agent). HL has no numeric `{code,msg}` envelope: a 200 body is returned
//! verbatim (the caller's `event_mapper`/`recon` reads its `{status:"ok"|"err", …}`); a non-2xx maps
//! to a [`VenueApiError`] carrying the HTTP status as `code` so
//! [`vike_bridge_core::transport::ErrorKind`] classifies it (429 → `RateLimited`, 5xx → `ServerError`).

use std::time::Duration;

#[cfg(feature = "exec")]
use serde::Serialize;

use vike_bridge_core::http;
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::transport::{
    ErrorKind, VenueApiError, classify_send_error, read_body_ambiguous,
};

use crate::config::Network;
use crate::consts::VENUE;
// The `/exchange` (signed) half of this transport is exec-plane: a feeds-only build
// (`default-features = false`) keeps the keyless `/info` reads and compiles none of it, so the
// signer types below — and the k256 stack behind them — never enter that binary.
#[cfg(feature = "exec")]
use crate::signing::action::Action;
#[cfg(feature = "exec")]
use crate::signing::{Signature, Signer};

/// Total attempts an idempotent `/info` read makes on a transient (rate-limit / server) failure —
/// one initial + up to two retries. `/exchange` is ALWAYS one attempt (never here).
const INFO_MAX_ATTEMPTS: u32 = 3;

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
#[cfg(feature = "exec")]
fn exchange_weight(batch_len: usize) -> usize {
    1 + batch_len / 40
}

/// The number of items an [`Action`] batches — the `batch_len` [`exchange_weight`] meters. A `modify`
/// is a single cancel-replace (1); the plural actions carry their vector length.
#[cfg(feature = "exec")]
fn action_batch_len(action: &Action) -> usize {
    match action {
        Action::Order(a) => a.orders.len(),
        Action::Cancel(a) => a.cancels.len(),
        Action::CancelByCloid(a) => a.cancels.len(),
        Action::Modify(_) => 1,
        Action::BatchModify(a) => a.modifies.len(),
    }
}

/// The `/exchange` request body: `{action, nonce, signature, vaultAddress?}`. `action` is borrowed
/// and serialized inline (its own `#[serde(tag="type")]` map); `vaultAddress` is omitted (not `null`)
/// when absent. Field order here is cosmetic — HL reconstructs the signed hash from the msgpack of
/// `action` (see [`crate::signing`]), not from this JSON's key order.
#[cfg(feature = "exec")]
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
#[cfg(feature = "exec")]
fn exchange_body(
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
        vault_address: vault.map(|v| format!("0x{}", hex::encode(v))),
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
/// paths therefore resolves [`crate::ratelimit::ip_weight_gate`] once and clones it into each
/// transport with [`Self::with_rate_gate`] — for a live mount that is the exec thread, the funding
/// poller and the reconcile client (`vike_mount::hyperliquid`'s `hl_ip_gate_and_transport`). Not
/// the WS feeds: they hold no transport of this type and draw no REST weight at all.
pub struct HyperliquidTransport {
    network: Network,
    info_url: String,
    // Read only by the `exec`-gated `/exchange` half; still RESOLVED unconditionally (it falls out
    // of the same `Network::urls` triple as `info_url`, and a cfg'd field would fracture `new`).
    #[cfg_attr(not(feature = "exec"), allow(dead_code))]
    exchange_url: String,
    agent: ureq::Agent,
    rate_gate: RateGate,
}

impl HyperliquidTransport {
    /// A transport for `network` with a fresh IP-weight gate — a WHOLE budget of its own, sized by
    /// [`crate::ratelimit::ip_weight_gate`] from the venue table. Correct for a one-shot caller;
    /// a second REST path in the same process must take the FIRST one's gate through
    /// [`Self::with_rate_gate`] rather than mint another (the budget is per-IP, so two windows
    /// spend two caps).
    pub fn new(network: Network) -> Self {
        let (info_url, exchange_url, _ws) = network.urls();
        HyperliquidTransport {
            network,
            info_url: info_url.to_string(),
            exchange_url: exchange_url.to_string(),
            agent: http::blocking_agent(),
            rate_gate: crate::ratelimit::ip_weight_gate(),
        }
    }

    /// Replace this transport's own IP-weight gate with a shared one, DISCARDING the fresh window
    /// [`Self::new`] built. Pass a clone of the handle the unit resolved once from
    /// [`crate::ratelimit::ip_weight_gate`], so every REST path in the process charges the one
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
    /// what builds it, and NOT the one a live mount shares across exec/funding/recon. Today's caller
    /// is `vike_mount::server_time`'s hyperliquid clock read, which must not park a mount for 30 s
    /// on a keyless canary: one `exchangeStatus` read at startup, so its unshared window is a
    /// bounded one-shot rather than a second sustained lane. Chain [`Self::with_rate_gate`] onto
    /// this if it ever becomes a repeating caller. ⚠ An ORDER path must NOT use this: a short timeout there converts a definite
    /// pre-send failure into [`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`], the "may have
    /// landed, must re-query" case [`Self::post_json`] exists to keep rare.
    pub fn with_agent(network: Network, agent: ureq::Agent) -> Self {
        HyperliquidTransport { agent, ..Self::new(network) }
    }

    /// The network this transport targets (mainnet/testnet) — the source of the phantom-agent
    /// `source` byte the signer stamps.
    pub fn network(&self) -> Network {
        self.network
    }

    /// This transport's IP-weight gate — whichever window it is riding, which is a fresh one of its
    /// own unless [`Self::with_rate_gate`] replaced it. Clone it into a sibling transport
    /// (`.with_rate_gate(t.rate_gate().clone())`) so both ride one budget, and read it (as
    /// [`crate::transfer`] and [`crate::builder_fee`] do) to charge a `/exchange` call this
    /// transport does not itself make.
    pub fn rate_gate(&self) -> &RateGate {
        &self.rate_gate
    }

    /// Keyless `POST /info` (research §8): the `body` is a plain `{type, …}` object (reads that scope
    /// to an account carry a `user` address field, never a signature). Charges the gate by the §9
    /// weight of `body["type"]`, then posts. As an idempotent read it MAY retry a transient throttle /
    /// server error (bounded by [`INFO_MAX_ATTEMPTS`], with a short backoff); every other outcome —
    /// including an ambiguous timeout — returns to the caller on the first attempt.
    pub fn info(&self, body: &serde_json::Value) -> Result<serde_json::Value, VenueApiError> {
        let request_type = body.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        let weight = info_weight(request_type);
        let payload = serde_json::to_vec(body)
            .map_err(|e| VenueApiError { code: 0, msg: format!("bad info body: {e}") })?;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            self.rate_gate.proceed_cost_logged(VENUE, request_type, weight);
            match self.post(&self.info_url, &payload) {
                Ok(v) => return Ok(v),
                Err(e)
                    if attempt < INFO_MAX_ATTEMPTS
                        && matches!(e.kind(), ErrorKind::RateLimited | ErrorKind::ServerError) =>
                {
                    tracing::warn!(
                        venue = VENUE, request_type, attempt, error = %e,
                        "info request transient failure; retrying"
                    );
                    // Short escalating backoff (venue thread, off the core fold): 200ms, then 400ms.
                    std::thread::sleep(Duration::from_millis(200 * u64::from(attempt)));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Signed `POST /exchange`: build `{action, nonce, signature, vaultAddress?}` (the signature
    /// covers the same nonce/vault), charge the gate `1 + floor(batch_len/40)`, then post **exactly
    /// once**. NEVER auto-retries — a post-send timeout (the ambiguous
    /// [`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`]) or a 429 returns as a
    /// [`VenueApiError`] the caller must resolve by re-querying order status, so a resend can't
    /// double-submit. A 200 body is returned verbatim (the caller reads `{status, response}`).
    #[cfg(feature = "exec")]
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
    /// [`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`] (a timeout AFTER send may have landed);
    /// a non-2xx maps to `code: <http status>` + the raw body text, so [`ErrorKind`] classifies it.
    ///
    /// `pub(crate)` so the crate's OTHER `/exchange` caller — [`crate::transfer`]'s user-signed
    /// `usdClassTransfer`, which can't route through [`Self::exchange`] (that one bakes in L1
    /// phantom-agent signing and only accepts the L1 `Action` enum) — reaches the SAME post+classify
    /// path instead of mirroring it. It previously hand-copied this body purely because this method
    /// was private; the audit-T1 classification is far too safety-critical to keep in two copies
    /// inside one crate.
    pub(crate) fn post(&self, url: &str, body: &[u8]) -> Result<serde_json::Value, VenueApiError> {
        let sent = self.agent.post(url).header("Content-Type", "application/json").send(body);
        let mut resp = sent.map_err(|e| {
            tracing::warn!(venue = VENUE, error = %e, "REST request failed");
            classify_send_error(&e)
        })?;
        let status = resp.status().as_u16();
        let text = read_body_ambiguous(&mut resp)?;
        if (200..300).contains(&status) {
            return serde_json::from_str(&text)
                .map_err(|e| VenueApiError { code: 0, msg: format!("bad json: {e}") });
        }
        // HL error bodies are plain text or a bare `{status:"err",…}` — no numeric code. Carry the
        // HTTP status as `code` (so `ErrorKind` maps 429/5xx) and the raw body as the message.
        tracing::warn!(venue = VENUE, status, "REST request failed");
        Err(VenueApiError { code: i64::from(status), msg: text })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Everything below that names the signing types is `exec`-gated with its half of the
    // transport; the `info_weight` schedule test at the bottom is the feeds-plane residue and
    // stays feature-less. A default `cargo test` compiles and runs all of it unchanged.
    #[cfg(feature = "exec")]
    use crate::signing::action::{
        BatchModifyAction, CancelAction, CancelByCloidAction, CancelCloidWire, CancelWire,
        LimitParams, ModifyWire, OrderAction, OrderKind, OrderWire,
    };

    /// The official Rust SDK's test wallet (shared with `tests/signing_vectors.rs`) — a valid
    /// secp256k1 key, so the `Signer` constructs and signing is exercised end-to-end.
    #[cfg(feature = "exec")]
    const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";
    #[cfg(feature = "exec")]
    const NONCE: u64 = 1583838;

    #[cfg(feature = "exec")]
    fn signer() -> Signer {
        Signer::from_private_key(KEY, Network::Mainnet).expect("SDK test key is valid")
    }

    /// A single-order `order` action (asset 1, buy 3.5 @ 2000, Ioc) — the shape exec submits.
    #[cfg(feature = "exec")]
    fn order_action() -> Action {
        Action::Order(OrderAction {
            orders: vec![OrderWire {
                asset: 1,
                is_buy: true,
                limit_px: "2000".to_string(),
                sz: "3.5".to_string(),
                reduce_only: false,
                order_type: OrderKind::Limit(LimitParams { tif: "Ioc".to_string() }),
                cloid: None,
            }],
            grouping: "na".to_string(),
            builder: None,
        })
    }

    #[cfg(feature = "exec")]
    #[test]
    fn exchange_body_has_action_nonce_signature_and_omits_vault() {
        let body = exchange_body(&order_action(), &signer(), NONCE, None);

        // action serializes inline as the tagged order envelope.
        assert_eq!(body["action"]["type"], "order");
        assert_eq!(body["action"]["orders"][0]["a"], 1);
        assert_eq!(body["action"]["orders"][0]["p"], "2000");

        // nonce is the numeric u64 we passed.
        assert_eq!(body["nonce"].as_u64(), Some(NONCE));

        // signature is the `{r,s,v}` object: r/s are 0x + 64 hex, v ∈ {27,28}.
        let sig = &body["signature"];
        let r = sig["r"].as_str().expect("r present");
        let s = sig["s"].as_str().expect("s present");
        assert!(r.starts_with("0x") && r.len() == 66, "malformed r: {r}");
        assert!(s.starts_with("0x") && s.len() == 66, "malformed s: {s}");
        assert!(matches!(sig["v"].as_u64(), Some(27) | Some(28)), "v: {}", sig["v"]);

        // no vault → the key is ABSENT (not null), so the venue's hash marker matches.
        assert!(body.get("vaultAddress").is_none(), "vaultAddress must be omitted when absent");
    }

    #[cfg(feature = "exec")]
    #[test]
    fn exchange_body_includes_lowercased_vault_address_when_present() {
        let vault = [0xABu8; 20];
        let body = exchange_body(&order_action(), &signer(), NONCE, Some(vault));
        assert_eq!(
            body["vaultAddress"].as_str(),
            Some("0xabababababababababababababababababababab"),
            "vaultAddress is 0x + lowercased 20-byte hex"
        );
    }

    #[cfg(feature = "exec")]
    #[test]
    fn signature_is_deterministic_for_a_fixed_action_and_nonce() {
        // RFC-6979 ECDSA ⇒ the same body is produced twice (guards against nondeterministic assembly).
        let a = exchange_body(&order_action(), &signer(), NONCE, None);
        let b = exchange_body(&order_action(), &signer(), NONCE, None);
        assert_eq!(a, b);
    }

    #[test]
    fn info_weight_schedule_matches_research_9() {
        for t in
            ["l2Book", "clearinghouseState", "orderStatus", "spotClearinghouseState", "allMids"]
        {
            assert_eq!(info_weight(t), 2, "{t} is a light read");
        }
        assert_eq!(info_weight("userRole"), 60);
        assert_eq!(info_weight("meta"), 20);
        assert_eq!(info_weight("spotMeta"), 20);
        assert_eq!(info_weight(""), 20, "an absent type defaults to the heavy bucket");
    }

    #[cfg(feature = "exec")]
    #[test]
    fn exchange_weight_is_one_plus_batch_over_forty() {
        assert_eq!(exchange_weight(0), 1);
        assert_eq!(exchange_weight(1), 1);
        assert_eq!(exchange_weight(39), 1);
        assert_eq!(exchange_weight(40), 2);
        assert_eq!(exchange_weight(79), 2);
        assert_eq!(exchange_weight(80), 3);
    }

    #[cfg(feature = "exec")]
    #[test]
    fn action_batch_len_reads_each_variant() {
        let order = OrderWire {
            asset: 1,
            is_buy: true,
            limit_px: "2000".to_string(),
            sz: "3.5".to_string(),
            reduce_only: false,
            order_type: OrderKind::Limit(LimitParams { tif: "Gtc".to_string() }),
            cloid: None,
        };
        assert_eq!(action_batch_len(&order_action()), 1);
        assert_eq!(
            action_batch_len(&Action::Cancel(CancelAction {
                cancels: vec![CancelWire { asset: 1, oid: 1 }, CancelWire { asset: 1, oid: 2 }],
            })),
            2
        );
        assert_eq!(
            action_batch_len(&Action::CancelByCloid(CancelByCloidAction {
                cancels: vec![CancelCloidWire { asset: 1, cloid: "0x00".to_string() }],
            })),
            1
        );
        assert_eq!(
            action_batch_len(&Action::Modify(ModifyWire { oid: 1, order: order.clone() })),
            1
        );
        assert_eq!(
            action_batch_len(&Action::BatchModify(BatchModifyAction {
                modifies: vec![
                    ModifyWire { oid: 1, order: order.clone() },
                    ModifyWire { oid: 2, order },
                ],
            })),
            2
        );
    }
}
