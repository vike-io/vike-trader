//! Blocking REST wrappers over the CP Gateway. Reuses vike-bridge-core's ureq agent (loopback
//! self-signed variant when the base URL is loopback; the verifying agent otherwise). Every method
//! returns the raw `serde_json::Value` (or a `VenueApiError`); the pure decoders in `decode.rs`
//! interpret the bodies.
//!
//! ## Wire shapes: LIVE-VERIFIED 2026-08-23, no longer guesses
//!
//! Every path in this module carried a `GUESSED (unverified against a live CP Gateway)` marker.
//! All of them were exercised against the CI box's live authenticated gateway (DUQ186573, Build
//! 10.46.2d) — the read-only ones by direct request, the order-mutating ones by placing, amending
//! and cancelling ONE real paper order and leaving the account flat. Observed responses:
//!
//! | wrapper | request | response |
//! |---|---|---|
//! | [`CpapiRest::place`] | `POST /v1/api/iserver/account/{acct}/orders` `{"orders":[order]}` | the reply-question array `[{"id":"…","message":[…],"messageIds":["o163"],"messageOptions":["Yes","No"]}]` |
//! | [`CpapiRest::reply`] | `POST /v1/api/iserver/reply/{id}` `{"confirmed":true}` | the accepted row `[{"order_id":"1491452840","local_order_id":"wire-probe-1","order_status":"PreSubmitted"}]` |
//! | [`CpapiRest::amend`] | `POST /v1/api/iserver/account/{acct}/order/{orderId}` + order body | the same accepted-row shape |
//! | [`CpapiRest::cancel`] | `DELETE /v1/api/iserver/account/{acct}/order/{orderId}` | `{"msg":"Request was submitted","order_id":1491452840,…}` |
//! | [`CpapiRest::open_orders`] | `GET /v1/api/iserver/account/orders` | `{"orders":[…],"snapshot":…}` |
//! | [`CpapiRest::executions`] | `GET /v1/api/iserver/account/trades` | `[]` (flat account) |
//! | [`CpapiRest::positions`] | `GET /v1/api/portfolio/{acct}/positions/0` | `[]` (flat account) |
//! | [`CpapiRest::ledger`] | `GET /v1/api/portfolio/{acct}/ledger` | `{"USD":{"cashbalance":…,"netliquidationvalue":…,"settledcash":…},"BASE":{…}}` |
//! | [`CpapiRest::secdef_search`] | `GET /v1/api/iserver/secdef/search?symbol=AAPL&secType=STK` | `[{"conid":"265598",…}]` — conid a STRING, as `decode::decode_conid` already handles |
//! | [`CpapiRest::tickle`] | `POST /v1/api/tickle` `{}` | `{"session":"…","iserver":{"authStatus":{…}}}` |
//! | [`CpapiRest::accounts`] | `GET /v1/api/iserver/accounts` | `{"accounts":["DUQ186573"],…,"isPaper":true}` |
//!
//! The `/v1/api` prefix, the `{"orders":[order]}` envelope, the `{"confirmed":true}` reply envelope
//! and the amend-by-POST shape are all confirmed by the rows above — and the reply CHAIN is not
//! hypothetical: a far-from-market limit trips IB's 3%-percentage-constraint question every time, so
//! [`CpapiRest::reply`] is on the normal path rather than an edge case.
//!
//! ⚠ **This retires the REST guesses ONLY.** The WS `sor` subscribe frame was measured in the same
//! session and does NOT work — see `transport/cpapi/mod.rs`'s module doc, which is where the
//! consequence (cpapi fills have no live path) is written down.

use serde_json::{Value, json};
use vike_bridge_core::http::{blocking_agent, blocking_agent_loopback_insecure, is_loopback_url};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};

pub struct CpapiRest {
    agent: ureq::Agent,
    base_url: String,
    account: String,
}

impl CpapiRest {
    /// Picks the insecure loopback agent ONLY when `base_url` is a loopback host (the CP Gateway's
    /// self-signed cert); any other host gets the normal verifying agent, so a misconfigured
    /// non-loopback gateway is never served insecurely.
    pub fn new(base_url: &str, account: &str) -> Self {
        let agent = if is_loopback_url(base_url) {
            blocking_agent_loopback_insecure()
        } else {
            blocking_agent()
        };
        CpapiRest {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            account: account.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn read(
        &self,
        sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Value, VenueApiError> {
        let mut resp = sent.map_err(|e| VenueApiError {
            code: if matches!(e, ureq::Error::Timeout(_)) { E_TIMEOUT_AMBIGUOUS } else { 0 },
            msg: format!("cpapi network error: {e}"),
        })?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| VenueApiError {
            code: E_TIMEOUT_AMBIGUOUS,
            msg: format!("cpapi read: {e}"),
        })?;
        let parsed: Result<Value, _> = serde_json::from_str(&text);
        if (200..300).contains(&status) {
            return parsed.map_err(|e| VenueApiError {
                code: 0,
                msg: format!("cpapi bad json ({status}): {e}"),
            });
        }
        Err(VenueApiError { code: i64::from(status), msg: text })
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value, VenueApiError> {
        let bytes = serde_json::to_vec(body).unwrap_or_default();
        let sent = self
            .agent
            .post(&self.url(path))
            .header("Content-Type", "application/json")
            .send(&bytes[..]);
        self.read(sent)
    }

    pub fn get(&self, path: &str) -> Result<Value, VenueApiError> {
        self.read(self.agent.get(&self.url(path)).call())
    }

    pub fn delete(&self, path: &str) -> Result<Value, VenueApiError> {
        self.read(self.agent.delete(&self.url(path)).call())
    }

    // --- typed wrappers (CP Gateway v1 paths — LIVE-VERIFIED, see the module doc table) ---

    /// POST /iserver/account/{acct}/orders  body {"orders":[order]}. Response is EITHER a
    /// reply-question array [{id, message:[...]}] OR an accepted array [{order_id, order_status}].
    pub fn place(&self, order: &Value) -> Result<Value, VenueApiError> {
        self.post_json(
            &format!("/v1/api/iserver/account/{}/orders", self.account),
            &json!({ "orders": [order] }),
        )
    }

    /// POST /iserver/reply/{id}  {"confirmed":true}.
    pub fn reply(&self, reply_id: &str, confirmed: bool) -> Result<Value, VenueApiError> {
        self.post_json(
            &format!("/v1/api/iserver/reply/{reply_id}"),
            &json!({ "confirmed": confirmed }),
        )
    }

    /// POST /iserver/account/{acct}/order/{orderId}  (amend) body = order fields to change.
    pub fn amend(&self, order_id: &str, order: &Value) -> Result<Value, VenueApiError> {
        self.post_json(&format!("/v1/api/iserver/account/{}/order/{order_id}", self.account), order)
    }

    /// DELETE /iserver/account/{acct}/order/{orderId}.
    pub fn cancel(&self, order_id: &str) -> Result<Value, VenueApiError> {
        self.delete(&format!("/v1/api/iserver/account/{}/order/{order_id}", self.account))
    }

    /// GET /iserver/account/orders (resync snapshot).
    pub fn open_orders(&self) -> Result<Value, VenueApiError> {
        self.get("/v1/api/iserver/account/orders")
    }

    /// The account-FREE cpapi fills/executions path. Embedding the account segment
    /// (`/iserver/account/{acct}/trades`) 404s on the CP Gateway — see [`Self::executions`].
    const EXECUTIONS_PATH: &'static str = "/v1/api/iserver/account/trades";

    /// GET /iserver/account/trades (recent executions). Account-FREE, exactly like the `orders`
    /// snapshot above — the CP Gateway 404s an `/iserver/account/{acct}/trades` variant
    /// (live-verified on the latency box's Gateway, 2026-07-21); the fills seam scopes to a conId itself.
    pub fn executions(&self) -> Result<Value, VenueApiError> {
        self.get(Self::EXECUTIONS_PATH)
    }

    /// GET /portfolio/{acct}/positions/{page} — the account's current positions, ONE page (30 rows)
    /// at a time (the CP Gateway paginates portfolio positions; page 0 is the first). The reconcile
    /// read seam (`recon_client`) fetches page 0 — a single-symbol reconcile never needs deep pages.
    /// LIVE-VERIFIED 2026-08-23: `GET /v1/api/portfolio/DUQ186573/positions/0` answered `200 []` on
    /// a flat account. See the module doc table.
    pub fn positions(&self, page: u32) -> Result<Value, VenueApiError> {
        self.get(&format!("/v1/api/portfolio/{}/positions/{page}", self.account))
    }

    /// GET /portfolio/{acct}/ledger — the account cash ledger keyed by currency (`{"BASE":{..},
    /// "USD":{..}}`), each carrying `cashbalance`/`netliquidationvalue`/`settledcash`. The reconcile
    /// balance read (`recon_client::parse_balance`) pulls the quote currency's `cashbalance`.
    /// LIVE-VERIFIED 2026-08-23: answered `200` with `USD`/`BASE` keys carrying `cashbalance`,
    /// `netliquidationvalue` and `settledcash` — exactly the fields `parse_balance` reads.
    pub fn ledger(&self) -> Result<Value, VenueApiError> {
        self.get(&format!("/v1/api/portfolio/{}/ledger", self.account))
    }

    /// GET /iserver/secdef/search?symbol=..&secType=.. (conId resolution).
    pub fn secdef_search(&self, symbol: &str, sec_type: &str) -> Result<Value, VenueApiError> {
        self.get(&format!("/v1/api/iserver/secdef/search?symbol={symbol}&secType={sec_type}"))
    }

    /// POST /tickle (session keep-alive; also used as the connect-time readiness probe).
    pub fn tickle(&self) -> Result<Value, VenueApiError> {
        self.post_json("/v1/api/tickle", &json!({}))
    }

    /// GET /iserver/accounts → the accounts the AUTHENTICATED SESSION can trade
    /// (`{"accounts":["DUQ186573"],..}`). This is the account cpapi orders actually route to — NOT
    /// `cfg.account` — so it is the authoritative paper/live gate for the live smoke.
    pub fn accounts(&self) -> Result<Value, VenueApiError> {
        self.get("/v1/api/iserver/accounts")
    }
}

#[cfg(test)]
mod tests {
    use super::CpapiRest;

    // Regression guard for the live-caught 404: the cpapi fills endpoint is account-FREE
    // (`/iserver/account/trades`); an earlier `/iserver/account/{acct}/trades` variant 404'd on the
    // CP Gateway. Asserts the exact path `executions()` sends (via the shared const) never embeds the
    // account id — mirroring the account-free `orders` snapshot.
    #[test]
    fn executions_path_is_account_free() {
        let acct = "DU7654321";
        let rest = CpapiRest::new("https://127.0.0.1:5000", acct);
        assert_eq!(CpapiRest::EXECUTIONS_PATH, "/v1/api/iserver/account/trades");
        let url = rest.url(CpapiRest::EXECUTIONS_PATH);
        assert_eq!(url, "https://127.0.0.1:5000/v1/api/iserver/account/trades");
        assert!(!url.contains(acct), "executions path must not embed the account id: {url}");
    }
}
