//! `quoted` — judging the quote currency and the rate a gatewayhub's facts answer carries, before the node
//! lets a symbol quoted in another currency than the account's anywhere near an engine (decision 0183,
//! phase 1). PURE: the clock is a parameter, so every verdict is a table row.
//!
//! The rule is **fail closed, never par**: a symbol the facts say is quoted in another currency is admitted
//! only with a rate that is finite and positive, stated with the moment it was asked, and fresh by this
//! node's own clock. A rate that is missing, unusable, older than the engine's window or from the future is
//! the symbol NOT admitted, in words that say which. An answer that names no currency is the symbol quoted
//! in the account's own (which the daemon's own fence guarantees), and it must carry no rate either: a rate
//! beside no currency is a daemon that disagrees with itself, and is refused, never read as the account's.
//!
//! Two kinds of refusal, because the node remembers one and not the other: a [`Verdict::Refused`] is about
//! the answer (it can never be a rate: remembered like any definitive refusal), a [`Verdict::Unavailable`] is
//! about the moment (an old rate is replaced by the next ask).

use vike_gatewayhub_client::proto::WireSymbolFacts;

/// How old a rate may be when the node takes it: the engine's own staleness window, because a rate the
/// engine would already call stale is no rate to send it.
pub const RATE_MAX_AGE_MS: i64 = vike_exec::DEFAULT_QUOTE_RATE_TTL_MS;

/// How far ahead of this node's clock a rate may claim to have been asked: the two processes share a host
/// (or a tunnel's few milliseconds), so more than this is a daemon on another clock, and its ages mean nothing.
pub const RATE_MAX_AHEAD_MS: i64 = 5_000;

/// The conversion a facts answer carries, judged usable.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    /// The currency the symbol is quoted in (upper case, three letters).
    pub ccy: String,
    /// Account-currency units per ONE unit of [`Self::ccy`]: finite and positive.
    pub rate: f64,
    /// When the rate was asked, epoch ms.
    pub as_of_ms: i64,
}

/// Why a facts answer's conversion cannot be used.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Definitive: this answer can never be admitted.
    Refused(String),
    /// Not now: the answer is stale or the clocks disagree; the next ask may be fine.
    Unavailable(String),
}

/// `Ok(None)`: the symbol is quoted in the account's own currency. `Ok(Some(quote))`: it is quoted in
/// another and `quote` converts it. `Err`: it is not admitted, and why.
pub fn judge(
    venue: &str,
    symbol: &str,
    facts: &WireSymbolFacts,
    now_ms: i64,
) -> Result<Option<Quote>, Verdict> {
    let Some(ccy) = facts.quote_ccy.as_deref() else {
        return if facts.quote_rate.is_some() || facts.rate_as_of_ms.is_some() {
            Err(Verdict::Refused(format!(
                "{venue}: the gatewayhub answered `{symbol}` with a conversion rate and no currency \
                 to apply it to, so the answer disagrees with itself and the symbol is NOT admitted"
            )))
        } else {
            Ok(None)
        };
    };
    if ccy.len() != 3 || !ccy.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(Verdict::Refused(format!(
            "{venue}: the gatewayhub names `{ccy}` as the currency `{symbol}` is quoted in, which is \
             not a currency code, so the symbol is NOT admitted"
        )));
    }
    let not_admitted = |why: String| {
        format!(
            "{venue}: `{symbol}` is quoted in {ccy} and this account's money is another currency, so \
             it is admitted only with a rate that converts {ccy}; {why}. The symbol is NOT admitted \
             (no rate, no admission: it is never valued at par)"
        )
    };
    let Some(rate) = facts.quote_rate else {
        return Err(Verdict::Refused(not_admitted("the gatewayhub gave none".to_string())));
    };
    if !(rate.is_finite() && rate > 0.0) {
        return Err(Verdict::Refused(not_admitted(format!(
            "the gatewayhub's rate ({rate}) is not a positive number"
        ))));
    }
    let Some(as_of_ms) = facts.rate_as_of_ms else {
        return Err(Verdict::Refused(not_admitted(
            "the gatewayhub did not say when it asked the venue for it".to_string(),
        )));
    };
    let age = now_ms.saturating_sub(as_of_ms);
    if age < -RATE_MAX_AHEAD_MS {
        return Err(Verdict::Unavailable(not_admitted(format!(
            "the rate claims to have been asked {} s in this node's future (the two clocks disagree)",
            -age / 1_000
        ))));
    }
    if age > RATE_MAX_AGE_MS {
        return Err(Verdict::Unavailable(not_admitted(format!(
            "the rate is {} s old, past the {} s the engine accepts",
            age / 1_000,
            RATE_MAX_AGE_MS / 1_000
        ))));
    }
    Ok(Some(Quote { ccy: ccy.to_string(), rate, as_of_ms }))
}

#[path = "quoted_tests.rs"]
#[cfg(test)]
mod quoted_tests;
