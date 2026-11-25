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
//!
//! ⚠ **The two market-data wrappers are NOT in the table above.** [`CpapiRest::marketdata_snapshot`]
//! and [`CpapiRest::marketdata_history`] (decision 0140's cpapi data client) are transcribed from
//! IBKR's public docs and have never answered this crate; the read-only
//! `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_cpapi_market_data_smoke.rs` is what moves them into it.

use serde_json::{Value, json};
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::reply::{
    RestReplyGatewayFirst, RestReplySpec, RestReplyWord, classify_rest_reply,
};
use vike_transport::http::{blocking_agent, blocking_agent_loopback_insecure};
use vike_utility::url::is_loopback_url;

/// The CP Gateway's reply rules (`vike_bridge_core::transport::reply`): its error body carries no code (the status is
/// the code, the body's `body_summary` the message: its `{"error": …}` sentence, never the body itself); a WRITE's 5xx
/// (owner ruling B-1: nothing in the body says IBKR did not act) or 3xx is "outcome unknown" before the body is read
/// (the loopback agent follows no redirect: it used to replay the write as a GET, which read as unknown too), with the
/// gateway's own sentence out of its first 512 bytes for the warning only (what a 5xx body proves about the order is
/// not documented anywhere in this repo); a read keeps its status.
/// Every message keeps its `cpapi ` words ([`cpapi_words`]).
const CPAPI_REPLY: RestReplySpec = RestReplySpec {
    code: None,
    no_verdict_on_5xx: &[],
    gateway_first: RestReplyGatewayFirst::OnWrites,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
    words: Some(cpapi_words),
    unknown_body_head: 512,
    plain_text_errors: false,
    verdict_on_5xx: &[],
};

/// cpapi's words for each verdict the reply engine writes, as this transport always wrote them with the body's
/// summary for the body: a 3xx on a write reads as the gateway answer it always did.
fn cpapi_words(word: &RestReplyWord<'_>) -> String {
    match word {
        RestReplyWord::SendFailed(e) => format!("cpapi network error: {e}"),
        RestReplyWord::Redirect { status, summary }
        | RestReplyWord::Gateway { status, summary } => {
            format!("cpapi network error: gateway http {status}, outcome unknown: {summary}")
        }
        RestReplyWord::BodyUnread(e) => format!("cpapi read: {e}"),
        RestReplyWord::BadJson { status, error, .. } => {
            format!("cpapi bad json ({status}): {error}")
        }
        RestReplyWord::Refused { summary, .. } => (*summary).to_string(),
    }
}

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

    /// The account the order paths are addressed to (the order list's rows of another account are not ours).
    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    /// `write` = the call mutates (POST/DELETE): only then can a 5xx leave the outcome unknown, and any 5xx does
    /// (owner ruling B-1: the gateway's error body carries no code of its own, so nothing says IBKR did not act).
    /// A send failure is `code: 0` (definite, the request never left) ONLY per the shared allowlist
    /// (`classify_send_error`); everything else, a 2xx write reply that is not JSON included, is
    /// `E_TIMEOUT_AMBIGUOUS`, which `place_order` never turns into a terminal reject ([`CPAPI_REPLY`]).
    fn read(
        &self,
        sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
        write: bool,
    ) -> Result<Value, VenueApiError> {
        // The CP Gateway holds the brokerage session itself: a request here carries no credential to redact.
        classify_rest_reply(sent, write, &CPAPI_REPLY, &[], |_| {})
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value, VenueApiError> {
        let bytes = serde_json::to_vec(body).unwrap_or_default();
        let sent = self
            .agent
            .post(&self.url(path))
            .header("Content-Type", "application/json")
            .send(&bytes[..]);
        self.read(sent, true)
    }

    pub fn get(&self, path: &str) -> Result<Value, VenueApiError> {
        self.read(self.agent.get(&self.url(path)).call(), false)
    }

    pub fn delete(&self, path: &str) -> Result<Value, VenueApiError> {
        self.read(self.agent.delete(&self.url(path)).call(), true)
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

    /// GET /iserver/account/orders (resync snapshot). Each row names the order client id `order_ref`
    /// (`decode::row_order_client_id`).
    pub fn open_orders(&self) -> Result<Value, VenueApiError> {
        self.get("/v1/api/iserver/account/orders")
    }

    /// GET /iserver/account/orders?force=true: the documented "clear the cache and obtain an updated
    /// view from the backend" call, whose own reply is an EMPTY array; the plain read after it is the
    /// real one (secondary sources, not a capture). Only `IbkrTransportCpapi::read_open_orders` sends it.
    pub fn open_orders_forced(&self) -> Result<Value, VenueApiError> {
        self.get("/v1/api/iserver/account/orders?force=true")
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
    /// read seam (`reconcile::cpapi::collect_position_pages`) pages until a short page: a symbol past page 0
    /// must never read as flat.
    /// LIVE-VERIFIED 2026-08-23: `GET /v1/api/portfolio/DUQ186573/positions/0` answered `200 []` on
    /// a flat account. See the module doc table.
    pub fn positions(&self, page: u32) -> Result<Value, VenueApiError> {
        self.get(&format!("/v1/api/portfolio/{}/positions/{page}", self.account))
    }

    /// GET /portfolio/{acct}/ledger — the account cash ledger keyed by currency (`{"BASE":{..},
    /// "USD":{..}}`), each carrying `cashbalance`/`netliquidationvalue`/`settledcash`. The reconcile
    /// balance read (`reconcile::cpapi::parse_balance`) pulls the quote currency's `cashbalance`.
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
    /// `config.account` — so it is the authoritative paper/live gate for the live smoke.
    pub fn accounts(&self) -> Result<Value, VenueApiError> {
        self.get("/v1/api/iserver/accounts")
    }

    // --- market data (decision 0140) — ⚠ UNVERIFIED: IBKR's public docs, no live capture yet ---

    /// GET /iserver/marketdata/snapshot?conids=..&fields=.. → an array of rows keyed by the same
    /// field codes as the `smd` stream (`crate::feed::cpapi::decode`'s module doc). IBKR documents
    /// two preconditions: `/iserver/accounts` must have been called in the session first (which
    /// `crate::feed::cpapi`'s `DataClientBusSpecBackendIbkrCpapi::connect` does), and the FIRST request for a conId only
    /// primes it and answers no fields. UNVERIFIED against a live gateway.
    pub fn marketdata_snapshot(&self, conid: i64, fields: &str) -> Result<Value, VenueApiError> {
        self.get(&format!("/v1/api/iserver/marketdata/snapshot?conids={conid}&fields={fields}"))
    }

    /// GET /iserver/marketdata/history?conid=..&period=..&bar=.. → `{"data":[{t,o,h,l,c,v}],
    /// "mdAvailability":..,"mktDataDelay":..,...}` (`crate::feed::cpapi::decode`'s
    /// `decode_history`). UNVERIFIED against a live gateway.
    pub fn marketdata_history(
        &self,
        conid: i64,
        period: &str,
        bar: &str,
    ) -> Result<Value, VenueApiError> {
        self.get(&format!(
            "/v1/api/iserver/marketdata/history?conid={conid}&period={period}&bar={bar}"
        ))
    }

    /// GET /portfolio/accounts — the portfolio accounts list, which IBKR documents as a call to make
    /// before any `/portfolio/{acct}/…` read. The reconcile client calls it ONCE at connect, the way
    /// it calls [`Self::accounts`]. ⚠ UNVERIFIED that this gateway needs it: positions and the
    /// ledger answered on 2026-08-23 without it. Its body is not read.
    pub fn portfolio_accounts(&self) -> Result<Value, VenueApiError> {
        self.get("/v1/api/portfolio/accounts")
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
        let account = "DU7654321";
        let rest = CpapiRest::new("https://127.0.0.1:5000", account);
        assert_eq!(CpapiRest::EXECUTIONS_PATH, "/v1/api/iserver/account/trades");
        let url = rest.url(CpapiRest::EXECUTIONS_PATH);
        assert_eq!(url, "https://127.0.0.1:5000/v1/api/iserver/account/trades");
        assert!(!url.contains(account), "executions path must not embed the account id: {url}");
    }
}
