//! **The bounds and the ANSWER SHAPE for [`Request::OptionsChain`](crate::proto::Request::OptionsChain)
//! that BOTH ends need** — the request validator, the expiry cap, and the outcome enum that keeps
//! "this server cannot answer for that underlying" from ever looking like "that underlying has no
//! chain". Declared ungated: a default `vike-datahub` build must DECODE the verb in order to answer
//! it cleanly. [`crate::catalog`] is the model, copied shape for shape.
//!
//! # What the verb is
//!
//! One underlying's option-chain SNAPSHOT, as the data daemon holds it: the daemon polls the venue's
//! public chain REST, folds the venue's two public option feeds onto it, and a client asks for the
//! current value. The request names a VENUE, an UNDERLYING and up to [`MAX_OPTIONS_EXPIRIES`] expiry
//! dates, nothing else: which chains are polled, which instruments are streamed and how often is the
//! SERVER's (`crates/vike-datahub/src/options/mod.rs`'s module doc), so no client input reaches the
//! venue. It writes nothing anywhere, so it is an Observe verb for 0062's decision-1 reason rather
//! than by 0058's four-part rule — `crate::proto::required_scope`'s arm carries the argument.
//!
//! # Why the constants live HERE and not in `vike-datahub`
//!
//! The rule [`crate::catalog`] states: a client cannot guard a rule the server does not know.
//! `crates/vike-datahub/src/server/options_chain.rs`'s `options_chain_verb` and
//! [`crate::DatahubClient::options_chain`] both call [`validate_options_request`], so a refusal seen
//! locally is the one the server would give; [`MAX_OPTIONS_EXPIRIES`] is enforced by the SERVER (no
//! request moves it, the `docs/decisions/0052` sense of a server constant) and read by a client that
//! wants to stay under it.
//!
//! # ⚠ The outcome is an ENUM because an empty chain map is a LIE for three states
//!
//! A server without the lane, a server whose first venue pass has not landed, and an underlying the
//! venue does not list would all render as "no chains" through a bare map. They are different
//! variants of [`OptionsChainOutcome`], so a client cannot collapse them by accident; an
//! [`OptionsChainOutcome::Chains`] whose `chains` is EMPTY still means what it says (none of the
//! asked expiries is listed).

use serde::{Deserialize, Serialize};
use vike_marketdata::UnderlyingChains;

/// The most expiries ONE request may name. **The server's bound, no request moves it**: a request
/// naming more is answered [`OptionsRefusal::TooManyExpiries`] having read nothing.
///
/// Derived from what the server HOLDS, not picked: its poll keeps the nearest 8 expiries per
/// underlying (`crates/vike-datahub/src/options/mod.rs`'s `POLLED_EXPIRIES`), so a request for more
/// could only ever name expiries the server does not have. The server asserts the two agree at
/// compile time.
pub const MAX_OPTIONS_EXPIRIES: usize = 8;

/// The longest venue slug this verb will carry — [`crate::catalog::CATALOG_MAX_VENUE_BYTES`]'s
/// bound and argument: it bounds the ALLOCATION a hostile frame can ask for, while the server's own
/// table lookup is the real bound.
pub const OPTIONS_MAX_VENUE_BYTES: usize = 16;

/// The longest underlying this verb will carry. Deribit's option underlyings are 3-letter coin codes
/// (`BTC`, `ETH`, `SOL`); 16 leaves a venue with longer codes its whole range.
pub const OPTIONS_MAX_UNDERLYING_BYTES: usize = 16;

/// The request rules, cheapest first, each naming what was wrong.
///
/// - the venue: non-empty, at most [`OPTIONS_MAX_VENUE_BYTES`], ASCII lowercase letters and digits
///   (every `vike_model::VENUES` slug);
/// - the underlying: non-empty, at most [`OPTIONS_MAX_UNDERLYING_BYTES`], ASCII uppercase letters
///   and digits (the venue's own currency code, exactly as the server's provider lists it);
/// - each expiry: an ISO calendar date `YYYY-MM-DD` by SHAPE (ten bytes, digits, dashes at offsets
///   4 and 7) — a well-shaped date the server does not list is OMITTED from the answer, never
///   refused.
///
/// The expiry COUNT is not a shape rule: over [`MAX_OPTIONS_EXPIRIES`] is a typed
/// [`OptionsRefusal::TooManyExpiries`] in a successful answer, so it is not checked here.
///
/// ⚠ **The message never ECHOES the input** (`crate::seed::validate_seed_symbol`'s rule): it names
/// the field, the length, the cap and the offset, never the untrusted string, which would ride into
/// a log whose file layer defaults to `trace`. ⚠ **Nothing is lower-cased or trimmed**: the server
/// looks each value up by exact match, so coercing one would make the request and the answer
/// disagree about what was asked for.
pub fn validate_options_request(
    venue: &str,
    underlying: &str,
    expiries: &[String],
) -> Result<(), String> {
    check_token(
        "venue",
        venue,
        OPTIONS_MAX_VENUE_BYTES,
        |b| b.is_ascii_lowercase() || b.is_ascii_digit(),
        "ASCII lowercase letters and digits (`deribit`)",
    )?;
    check_token(
        "underlying",
        underlying,
        OPTIONS_MAX_UNDERLYING_BYTES,
        |b| b.is_ascii_uppercase() || b.is_ascii_digit(),
        "ASCII uppercase letters and digits (`BTC`, `ETH`, `SOL`)",
    )?;
    for (i, e) in expiries.iter().enumerate() {
        if !is_iso_date_shape(e) {
            return Err(format!(
                "options expiry #{i} is not an ISO date: an expiry is `YYYY-MM-DD` — ten bytes, \
                 digits with dashes at offsets 4 and 7 (`2026-06-26`)."
            ));
        }
    }
    Ok(())
}

/// One token's rules: empty, too long, then the first byte outside `allowed` — never echoing it.
fn check_token(
    field: &str,
    value: &str,
    max: usize,
    allowed: impl Fn(u8) -> bool,
    charset: &str,
) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("an options {field} is EMPTY, so it names nothing."));
    }
    if value.len() > max {
        return Err(format!(
            "an options {field} of {} bytes exceeds its cap of {max} bytes.",
            value.len()
        ));
    }
    if let Some(at) = value.bytes().position(|b| !allowed(b)) {
        return Err(format!(
            "an options {field} carries a byte outside the permitted set at offset {at}. It may hold \
             {charset} only."
        ));
    }
    Ok(())
}

/// `YYYY-MM-DD` by shape: ten ASCII bytes, digits everywhere but the dashes at offsets 4 and 7.
/// A calendar check is the server's lookup: a date it does not list is omitted, not refused.
fn is_iso_date_shape(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// Why the server answered no chains — the REFUSALS, each a different fact about the world.
///
/// Separate variants rather than one string because a client renders them differently: the two
/// `Unknown*` name what IS served (a misspelling is told apart from a missing venue), the expiry cap
/// names its number, and `NotReady` is the one that clears by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionsRefusal {
    /// **This server's options lane does not serve the venue** — a fact about the BUILD's table,
    /// naming what it does serve.
    UnknownVenue { supported: Vec<String> },
    /// **The venue's provider does not list this underlying** — naming the ones it does.
    UnknownUnderlying { supported: Vec<String> },
    /// **The request named more than [`MAX_OPTIONS_EXPIRIES`] expiries** — the server's bound,
    /// carried so a client can say which number it hit.
    TooManyExpiries { max: usize },
    /// **The lane is armed but holds no snapshot for the underlying yet** — the first venue pass
    /// has not landed, or every pass so far failed. `why` says which. The one refusal that clears by
    /// itself: ask again.
    NotReady { why: String },
}

impl OptionsRefusal {
    /// A one-line operator-facing sentence. The client renders this rather than assembling its own,
    /// so the daemon and the desktop cannot describe the same refusal differently.
    pub fn describe(&self, venue: &str, underlying: &str) -> String {
        match self {
            Self::UnknownVenue { supported } => format!(
                "`{venue}` has no options chain in this server's build. Supported: [{}].",
                supported.join(", ")
            ),
            Self::UnknownUnderlying { supported } => format!(
                "`{venue}` lists no options on `{underlying}` in this server. Supported: [{}].",
                supported.join(", ")
            ),
            Self::TooManyExpiries { max } => format!(
                "a request for `{venue}` `{underlying}` named more expiries than this server \
                 answers at once (MAX_OPTIONS_EXPIRIES = {max}). Ask for fewer, or for none to get \
                 the front expiry."
            ),
            Self::NotReady { why } => format!(
                "`{venue}` `{underlying}`: this server holds no chain snapshot yet — {why}. Ask \
                 again; this clears by itself once a venue pass lands."
            ),
        }
    }
}

/// What the server DID about one [`Request::OptionsChain`](crate::proto::Request::OptionsChain).
///
/// The states are mutually exclusive by construction: `Chains` with an empty map and every refusal
/// are different variants, so no client can collapse them by forgetting to check a flag.
/// ⚠ `PartialEq` but NOT `Eq`, by force: an `OptionChain` carries `f64` prices and greeks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OptionsChainOutcome {
    /// The underlying's snapshot: `chains` restricted to the asked expiries (the front expiry,
    /// `default_expiry`, when none were asked; an asked expiry the server does not list is OMITTED),
    /// with the FULL `expiries` strip and `default_expiry` kept, so a client can offer every expiry
    /// the server holds.
    Chains(UnderlyingChains),
    /// **This server serves no options chains** — its build lacks the lane
    /// (`vike-datahub`'s `options-serve` feature). A SUCCESS, not an error, exactly as
    /// [`crate::catalog::CatalogOutcome::NotArmed`] is: nothing was polled for this request.
    NotArmed,
    /// Nothing was answered for the request. See [`OptionsRefusal`].
    Refused(OptionsRefusal),
}

/// The [`Response::OptionsChain`](crate::proto::Response::OptionsChain) payload.
/// ⚠ `PartialEq` but not `Eq` — see [`OptionsChainOutcome`], which it contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionsChainReply {
    /// The venue this answers for, echoed VERBATIM as the client spelled it.
    pub venue: String,
    /// The underlying this answers for, echoed VERBATIM — so a client holding several in flight
    /// can match them up without relying on ordering.
    pub underlying: String,
    /// **Every underlying this server serves for `venue`**, in canonical order (BTC, ETH, SOL, then
    /// the rest — `vike_options::underlying_rank`): exactly what the venue's provider lists, which
    /// is what the lane polls and may answer for. Present on EVERY outcome so a client's selector
    /// learns the set from any answer, a refusal included; EMPTY when the server serves the venue
    /// nothing (`NotArmed`, `UnknownVenue`). It is the server's set, not the held one: an underlying
    /// listed here may still answer `NotReady` until its first pass lands.
    pub served_underlyings: Vec<String>,
    /// What happened.
    pub outcome: OptionsChainOutcome,
}

impl OptionsChainReply {
    /// The snapshot, or `None` for every non-`Chains` outcome.
    ///
    /// ⚠ A convenience for a caller that has ALREADY branched on [`Self::outcome`] — never a
    /// substitute for doing so, which is the collapse this module's whole shape exists to prevent.
    pub fn chains(&self) -> Option<&UnderlyingChains> {
        match &self.outcome {
            OptionsChainOutcome::Chains(c) => Some(c),
            _ => None,
        }
    }

    /// A one-line operator-facing sentence for every outcome, including the successful one.
    ///
    /// It does not spell out [`Self::served_underlyings`]: a selector reads that field directly, and
    /// a refusal naming its own set already does so (`UnknownUnderlying`).
    pub fn describe(&self) -> String {
        match &self.outcome {
            OptionsChainOutcome::Chains(c) => format!(
                "`{}` `{}`: {} of {} expiries served.",
                self.venue,
                self.underlying,
                c.chains.len(),
                c.expiries.len()
            ),
            OptionsChainOutcome::NotArmed => format!(
                "this datahub serves no options chains: its build carries no options lane (the \
                 `options-serve` feature of vike-datahub). Rebuild it with that feature — the \
                 shipped image carries it. Nothing was polled for `{}` `{}`.",
                self.venue, self.underlying
            ),
            OptionsChainOutcome::Refused(r) => r.describe(&self.venue, &self.underlying),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exp(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn a_well_formed_request_is_accepted() {
        validate_options_request("deribit", "BTC", &[]).expect("no expiries = the front one");
        validate_options_request("deribit", "SOL", &exp("2026-06-26")).expect("an ISO date");
        for v in vike_model::VENUES {
            assert!(v.len() <= OPTIONS_MAX_VENUE_BYTES, "{v} exceeds the venue cap");
            validate_options_request(v, "BTC", &[]).unwrap_or_else(|e| panic!("{v}: {e}"));
        }
    }

    #[test]
    fn a_malformed_field_is_refused_by_name_and_never_echoed() {
        for (venue, underlying, expiries, field) in [
            ("", "BTC", vec![], "venue"),
            ("Deribit", "BTC", vec![], "venue"),
            ("deribit/../x", "BTC", vec![], "venue"),
            ("deribit", "", vec![], "underlying"),
            ("deribit", "btc", vec![], "underlying"),
            ("deribit", "BTC USD", vec![], "underlying"),
            ("deribit", "BTC", exp("26JUN26"), "expiry"),
            ("deribit", "BTC", exp("2026-6-26x"), "expiry"),
            ("deribit", "BTC", exp("2026/06/26"), "expiry"),
        ] {
            let err = validate_options_request(venue, underlying, &expiries)
                .expect_err("must be refused");
            assert!(err.contains(field), "{venue}/{underlying}/{expiries:?}: {err}");
            for bad in [venue, underlying].into_iter().chain(expiries.iter().map(String::as_str)) {
                if bad.len() > 3 && !["deribit", "BTC"].contains(&bad) {
                    assert!(!err.contains(bad), "the refusal echoed the input back: {err}");
                }
            }
        }
        let long = "X".repeat(OPTIONS_MAX_UNDERLYING_BYTES + 1);
        let err = validate_options_request("deribit", &long, &[]).expect_err("over-long");
        assert!(err.contains("exceeds its cap"), "{err}");
        assert!(!err.contains(&long), "the refusal echoed the underlying back");
    }

    #[test]
    fn the_expiry_count_is_not_a_shape_rule() {
        // Over the cap is a typed refusal in a SUCCESSFUL answer (the server's), never a shape error.
        let many: Vec<String> =
            (1..=MAX_OPTIONS_EXPIRIES + 3).map(|d| format!("2026-07-{d:02}")).collect();
        validate_options_request("deribit", "BTC", &many).expect("well-shaped, however many");
    }

    #[test]
    fn every_outcome_says_something_different() {
        let reply = |outcome| OptionsChainReply {
            venue: "deribit".into(),
            underlying: "BTC".into(),
            served_underlyings: vec!["BTC".into(), "ETH".into(), "SOL".into()],
            outcome,
        };
        let empty = reply(OptionsChainOutcome::Chains(UnderlyingChains::default()));
        let unarmed = reply(OptionsChainOutcome::NotArmed);
        let not_ready = reply(OptionsChainOutcome::Refused(OptionsRefusal::NotReady {
            why: "the first pass has not landed".into(),
        }));
        assert_ne!(empty.outcome, unarmed.outcome);
        assert!(empty.chains().is_some() && unarmed.chains().is_none());
        assert!(empty.describe().contains("0 of 0 expiries"), "{}", empty.describe());
        assert!(unarmed.describe().contains("options-serve"), "{}", unarmed.describe());
        assert!(not_ready.describe().contains("Ask again"), "{}", not_ready.describe());
        let s = OptionsRefusal::UnknownUnderlying { supported: vec!["BTC".into(), "ETH".into()] }
            .describe("deribit", "DOGE");
        assert!(s.contains("BTC, ETH") && s.contains("DOGE"), "{s}");
        let s = OptionsRefusal::UnknownVenue { supported: vec!["deribit".into()] }
            .describe("okx", "BTC");
        assert!(s.contains("[deribit]"), "{s}");
        let s = OptionsRefusal::TooManyExpiries { max: MAX_OPTIONS_EXPIRIES }
            .describe("deribit", "BTC");
        assert!(s.contains("MAX_OPTIONS_EXPIRIES = 8"), "{s}");
    }
}
