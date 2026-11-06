//! `preflight` — READ the wallet's balance and allowance before an order is signed, and REFUSE IN
//! WORDS what is missing, instead of letting the venue answer "not enough balance / allowance".
//!
//! ## What the operator got before this
//! A wallet that was never funded, or never approved the exchange, signed the order, sent it, and
//! read back the venue's own sentence as the `OrderRejected` reason (`polymarket 400: {"error":
//! "not enough balance / allowance"}`). The sentence names neither the asset, nor the amounts, nor
//! WHICH contract needs an approval, so the operator's only road was the venue's app. The order had
//! also been signed and spent a submit token.
//!
//! ## What this does
//! [`Preflight::check`] runs in the exec thread on every order, between building the order and
//! signing it:
//! * a BUY reads `GET /balance-allowance?asset_type=COLLATERAL` and needs `order.maker_amount` pUSD
//!   of BOTH balance and allowance (for the exchange this order settles through);
//! * a SELL reads `asset_type=CONDITIONAL&token_id=…` and needs `order.maker_amount` shares.
//!
//! A positive shortfall is refused in words ([`refusal_words`]) before anything is signed. The
//! answer is cached for [`PREFLIGHT_TTL`] only while it shows NO shortfall, so a burst of orders
//! costs one read, while a refusal is never served from cache: a deposit made a second ago is seen
//! by the next order.
//!
//! ## It only ever refuses on a POSITIVE, PARSED shortfall
//! A read that fails (a 5xx, a 401, a proxy down) or outlasts [`READ_TIMEOUT`] (it runs ON the exec
//! thread in front of the order, so it is not given the order path's 30 s), a body that is not an
//! object, a missing figure,
//! and an `allowances` map that does not list the order's exchange are all "unknown", and an unknown
//! lets the order through for the venue to judge — the geoblock pre-flight's rule
//! (`crate::exec_plane::adapter::geoblock_action`): a probe that cannot read is no evidence, and a
//! check that could fail an order on a misparse would be worse than the reject it replaces. The
//! balance includes funds already committed to the wallet's resting orders, so this check is a
//! NECESSARY condition only: it can let through an order the venue refuses, never the reverse.
//!
//! ⚠ **The wire shape of `/balance-allowance` has not been read live in this repo.** Two shapes are
//! understood: `{"balance": "<base units>", "allowances": {"<spender>": "<base units>", …}}` (one
//! figure per contract) and the older `{"balance": …, "allowance": …}` (one figure). Amounts are
//! decimal strings in 6-decimal base units and an unlimited allowance (`2^256-1`) saturates. The
//! first live read confirms or corrects this parser; until then a wrong guess degrades to "unknown",
//! not to a wrong refusal.
//!
//! ## The allowance repair
//! When the shortfall is an ALLOWANCE one and the account is a deposit wallet (`POLY_1271`) with
//! relayer credentials, the bridge sends the four trading approvals once per process
//! ([`crate::exec_plane::onboarding`]) and still refuses THIS order, saying so: the approvals take
//! effect when mined, and an order that waited for them would hold the exec thread. Once the batch is
//! out, a still-short allowance first asks the CLOB to re-read the chain (`GET
//! /balance-allowance/update`, which the venue's own client calls after an approval: the CLOB caches
//! the allowance) and reads again, so a later order is refused only if the venue still says so. Any
//! other account type, or no relayer credentials, is told in words what to do by hand.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::PolymarketCreds;
use crate::exec_plane::exec::get_signed_within;
use crate::exec_plane::onboarding::send_trading_approvals;
use crate::exec_plane::order::{Order, Side, SignatureType, exchange_contract};
use crate::market_ref::is_token_id;

/// How long a snapshot that showed no shortfall may stand in for a fresh read.
pub(crate) const PREFLIGHT_TTL: Duration = Duration::from_secs(10);

/// How long ONE read may hold the exec thread. The read runs in front of an order, and a read that
/// takes longer than this is no evidence either way (the order goes to the venue), so it is not given
/// the order path's [`crate::egress::REST_TIMEOUT`]: a stalled `/balance-allowance` would otherwise
/// hold every order and cancel behind it for 30 s, about the core's own ack timeout.
const READ_TIMEOUT: Duration = Duration::from_secs(4);

/// `GET` path of the read.
const PATH: &str = "/balance-allowance";

/// The part of a failed read's error that may be logged. `get_signed`'s errors END in the response
/// BODY (`polymarket 502: <body>`, `bad json (200): <serde error>: <body>`), and an error page from a
/// proxy or CDN can echo the request, `POLY_*` credential headers included, so a body never reaches a
/// log line (the rule `crate::exec_plane::l1`'s `parse_trio` is written under). A transport error
/// (`network: …`) carries no body and is kept whole.
pub(crate) fn loggable(error: &str) -> String {
    let head = if error.starts_with("network:") {
        error
    } else {
        error.split_once(':').map_or(error, |(status, _)| status)
    };
    head.chars().take(160).collect()
}

/// A base-unit figure: a JSON number, or a decimal string (the venue's spelling). A string too big
/// for `u128` is an unlimited allowance (`2^256-1`) and SATURATES rather than failing the parse.
/// Anything else (a decimal point, a sign, text, `null`) is unknown.
fn base_units(v: &Value) -> Option<u128> {
    match v {
        Value::Number(n) => n.as_u64().map(u128::from),
        Value::String(s) => {
            let s = s.trim();
            // digits only, so the one way `parse` can fail is overflow
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u128>().unwrap_or(u128::MAX))
        }
        _ => None,
    }
}

/// One `/balance-allowance` answer, as far as it could be read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct BalanceAllowance {
    pub balance: Option<u128>,
    /// `allowances`: spender address (as sent) → allowance.
    pub per_spender: Vec<(String, u128)>,
    /// The single `allowance` figure of the older shape.
    pub single: Option<u128>,
}

impl BalanceAllowance {
    /// `None` when the body is not an object (nothing to read).
    pub(crate) fn parse(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        Some(BalanceAllowance {
            balance: o.get("balance").and_then(base_units),
            per_spender: o
                .get("allowances")
                .and_then(Value::as_object)
                .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), base_units(v)?))).collect())
                .unwrap_or_default(),
            single: o.get("allowance").and_then(base_units),
        })
    }

    /// The allowance granted to `spender`: its own entry when the venue lists one per contract (an
    /// unlisted contract is UNKNOWN, not zero), else the single figure of the older shape.
    pub(crate) fn allowance_for(&self, spender: &str) -> Option<u128> {
        if self.per_spender.is_empty() {
            return self.single;
        }
        self.per_spender.iter().find(|(a, _)| a.eq_ignore_ascii_case(spender)).map(|(_, v)| *v)
    }
}

/// What an order is short of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shortfall {
    /// The wallet holds `have` of the asset the order gives; it needs `need`.
    Balance { have: u128, need: u128 },
    /// The wallet holds enough, but has approved the order's exchange for only `have`.
    Allowance { have: u128, need: u128 },
}

/// The shortfall of `order` against `snap`, balance first. `None` when the snapshot shows enough or
/// does not say.
pub(crate) fn shortfall(order: &Order, snap: &BalanceAllowance) -> Option<Shortfall> {
    let need = order.maker_amount;
    if let Some(have) = snap.balance
        && have < need
    {
        return Some(Shortfall::Balance { have, need });
    }
    if let Some(have) = snap.allowance_for(exchange_contract(order.neg_risk))
        && have < need
    {
        return Some(Shortfall::Allowance { have, need });
    }
    None
}

/// 6-decimal base units → whole units, trailing zeros trimmed (`5000000` → `5`, `12500000` → `12.5`).
fn units(v: u128) -> String {
    let (whole, frac) = (v / 1_000_000, v % 1_000_000);
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{frac:06}").trim_end_matches('0').to_string()
    }
}

pub(crate) fn exchange_name(neg_risk: bool) -> &'static str {
    if neg_risk { "neg-risk CTF exchange" } else { "CTF exchange" }
}

/// The refusal an operator reads, naming the asset, both amounts, the wallet and — for an allowance —
/// the contract. `repair` is the sentence about what the bridge did or what is left to do by hand.
pub(crate) fn refusal_words(order: &Order, s: &Shortfall, repair: &str) -> String {
    let (verb, wallet) = (if order.side == Side::Buy { "BUY" } else { "SELL" }, &order.maker);
    match (s, order.side) {
        (Shortfall::Balance { have, need }, Side::Buy) => format!(
            "polymarket: this BUY was NOT sent. It needs {} pUSD (the venue's collateral token) and \
             the funder wallet {wallet} holds {}. Put pUSD into that wallet (a wallet holding only \
             USDC cannot buy until it has been converted in the Polymarket app), then send the \
             order again.",
            units(*need),
            units(*have)
        ),
        (Shortfall::Balance { have, need }, Side::Sell) => format!(
            "polymarket: this SELL was NOT sent. It sells {} shares of token {} and the funder \
             wallet {wallet} holds {} of them, {}. Check the token id (a Yes share and a No share \
             are different tokens) and which wallet holds the position.",
            units(*need),
            order.token_id,
            units(*have),
            // the words state what is true of the figures: a wallet holding some is not empty
            if *have == 0 {
                "so there is nothing to sell"
            } else {
                "which is less than this order sells"
            }
        ),
        (Shortfall::Allowance { have, need }, side) => {
            let (what, spends) = match side {
                Side::Buy => ("pUSD", "spend its pUSD"),
                Side::Sell => ("outcome tokens", "move its outcome tokens"),
            };
            format!(
                "polymarket: this {verb} was NOT sent. The funder wallet {wallet} has not approved \
                 the {} {} to {spends}: the approved amount of {what} is {} and the order needs {}. \
                 That one-time approval is the venue's \"Enable trading\". {repair}",
                exchange_name(order.neg_risk),
                exchange_contract(order.neg_risk),
                units(*have),
                units(*need)
            )
        }
    }
}

/// What has been done about a missing allowance in this process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Approvals {
    NotTried,
    Sent { tx: Option<String> },
    Failed(String),
}

/// The exec thread's pre-order read. One per exec thread; it owns its cache and the one-shot
/// approval state, so it needs no lock.
pub(crate) struct Preflight {
    clob_base: String,
    relayer_base: String,
    signature_type: SignatureType,
    ttl: Duration,
    cache: HashMap<String, (Instant, BalanceAllowance)>,
    /// After an unreadable answer, reads are skipped until this instant, so a venue that keeps
    /// failing costs one failed GET per [`PREFLIGHT_TTL`], not one per order.
    unreadable_until: Option<Instant>,
    approvals: Approvals,
}

impl Preflight {
    pub(crate) fn new(clob_base: &str, relayer_base: &str, signature_type: SignatureType) -> Self {
        Preflight {
            clob_base: clob_base.to_string(),
            relayer_base: relayer_base.to_string(),
            signature_type,
            ttl: PREFLIGHT_TTL,
            cache: HashMap::new(),
            unreadable_until: None,
            approvals: Approvals::NotTried,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    fn key(order: &Order) -> String {
        match order.side {
            Side::Buy => "COLLATERAL".to_string(),
            Side::Sell => format!("CONDITIONAL:{}", order.token_id),
        }
    }

    /// `Ok(())` = sign and send; `Err(words)` = refuse, with the sentence that becomes the
    /// `OrderRejected` reason. See the module doc for when it refuses.
    pub(crate) fn check(&mut self, creds: &PolymarketCreds, order: &Order) -> Result<(), String> {
        // A symbol that is not a token id never reached a resolved signing domain in production; there
        // is no balance to read for it, and the venue refuses it by its own words.
        if !is_token_id(&order.token_id) {
            return Ok(());
        }
        let key = Self::key(order);
        if let Some((at, snap)) = self.cache.get(&key)
            && at.elapsed() < self.ttl
            && shortfall(order, snap).is_none()
        {
            return Ok(());
        }
        if self.unreadable_until.is_some_and(|until| Instant::now() < until) {
            return Ok(());
        }
        let mut snap = match self.read(creds, order) {
            Ok(Some(snap)) => snap,
            // A 200 whose body is not an object is as unreadable as a failed request: it backs off
            // too, or a venue that keeps answering it costs one read in front of EVERY order.
            Ok(None) => {
                tracing::warn!(
                    target: "vike_polymarket::preflight",
                    "the balance/allowance pre-check got a body that is not an object — the order \
                     goes to the venue, which will judge it"
                );
                self.unreadable_until = Some(Instant::now() + self.ttl);
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    target: "vike_polymarket::preflight",
                    error = %loggable(&e),
                    "the balance/allowance pre-check could not read the wallet — the order goes to \
                     the venue, which will judge it"
                );
                self.unreadable_until = Some(Instant::now() + self.ttl);
                return Ok(());
            }
        };
        self.unreadable_until = None;
        // The CLOB keeps its own cache of the chain's allowance. Once the bridge has sent the
        // approvals, a still-short allowance may only be that cache, so ask the venue to re-read the
        // chain (`GET /balance-allowance/update`) and read once more before refusing.
        if matches!(shortfall(order, &snap), Some(Shortfall::Allowance { .. }))
            && matches!(self.approvals, Approvals::Sent { .. })
        {
            self.refresh(creds, order);
            if let Ok(Some(again)) = self.read(creds, order) {
                snap = again;
            }
        }
        self.cache.insert(key, (Instant::now(), snap.clone()));
        let Some(short) = shortfall(order, &snap) else { return Ok(()) };
        let repair = match short {
            Shortfall::Allowance { .. } => self.repair_allowance(creds, order),
            Shortfall::Balance { .. } => String::new(),
        };
        Err(refusal_words(order, &short, &repair))
    }

    /// The query naming the asset this order gives and the wallet type that holds it.
    fn query(&self, order: &Order) -> String {
        let sig = self.signature_type.code();
        match order.side {
            Side::Buy => format!("asset_type=COLLATERAL&signature_type={sig}"),
            Side::Sell => {
                format!("asset_type=CONDITIONAL&token_id={}&signature_type={sig}", order.token_id)
            }
        }
    }

    /// One L2-signed `GET /balance-allowance` for the asset this order gives. `Ok(None)` = the body
    /// was not an object.
    fn read(
        &self,
        creds: &PolymarketCreds,
        order: &Order,
    ) -> Result<Option<BalanceAllowance>, String> {
        let v = get_signed_within(&self.clob_base, PATH, &self.query(order), creds, READ_TIMEOUT)?;
        Ok(BalanceAllowance::parse(&v))
    }

    /// Ask the CLOB to re-read this asset's balance and allowance from the chain
    /// (`GET /balance-allowance/update`, the same query). Best effort: its answer is not used, only
    /// the read that follows is, and a failure leaves that read to say what it says.
    ///
    /// ⚠ Unverified live: the path and its query are the documented ones (py-clob-client's
    /// `update_balance_allowance`); this repo has never called it.
    fn refresh(&self, creds: &PolymarketCreds, order: &Order) {
        if let Err(e) = get_signed_within(
            &self.clob_base,
            "/balance-allowance/update",
            &self.query(order),
            creds,
            READ_TIMEOUT,
        ) {
            tracing::warn!(
                target: "vike_polymarket::preflight",
                error = %loggable(&e),
                "the CLOB would not refresh its balance/allowance cache"
            );
        }
    }

    /// Send the trading approvals (at most once per process) and say what happened, or say why they
    /// cannot be sent and what to do by hand. The returned sentence ends the refusal.
    fn repair_allowance(&mut self, creds: &PolymarketCreds, order: &Order) -> String {
        const BY_HAND: &str = "Approve once in the Polymarket app (\"Enable trading\") and send the \
                               order again.";
        match &self.approvals {
            Approvals::Sent { tx } => {
                return format!(
                    "The bridge sent the approvals earlier in this run (transaction {}) and the \
                     venue still reports this allowance: wait for that transaction to be mined, or \
                     approve in the Polymarket app (\"Enable trading\").",
                    tx.as_deref().unwrap_or("hash not reported")
                );
            }
            Approvals::Failed(why) => {
                return format!(
                    "The bridge tried to send the approvals through the relayer earlier in this run \
                     and it failed ({why}). {BY_HAND}"
                );
            }
            Approvals::NotTried => {}
        }
        if self.signature_type != SignatureType::Poly1271 {
            return format!(
                "This wallet is not a deposit wallet, so its approvals are Polygon transactions \
                 paid in gas, which this bridge does not send. {BY_HAND}"
            );
        }
        if creds.relayer_key.is_empty() || creds.relayer_address.is_empty() {
            return format!(
                "The account has no relayer credentials (RELAYER_API_KEY / RELAYER_API_KEY_ADDRESS), \
                 without which the bridge cannot send the approvals itself. Add them to the \
                 account, or: {BY_HAND}"
            );
        }
        tracing::warn!(
            target: "vike_polymarket::preflight",
            wallet = %order.maker,
            "polymarket: the wallet has not approved the exchange — sending the four trading \
             approvals (pUSD + outcome tokens, both exchanges) through the relayer"
        );
        // Whatever happens next, the snapshot is stale: the next order re-reads.
        self.cache.clear();
        match send_trading_approvals(&self.relayer_base, creds, &order.maker) {
            Ok(sent) => {
                let sentence = format!(
                    "The bridge has just sent the approvals through the relayer (transaction {}); \
                     they take effect when mined, usually within a minute. Send the order again \
                     then.",
                    sent.tx_hash.as_deref().unwrap_or("hash not reported")
                );
                self.approvals = Approvals::Sent { tx: sent.tx_hash };
                sentence
            }
            Err(why) => {
                let why: String = why.chars().take(200).collect();
                let sentence = format!(
                    "The bridge tried to send the approvals through the relayer and it failed \
                     ({why}). {BY_HAND}"
                );
                self.approvals = Approvals::Failed(why);
                sentence
            }
        }
    }
}

#[path = "preflight_tests.rs"]
#[cfg(test)]
mod preflight_tests;
