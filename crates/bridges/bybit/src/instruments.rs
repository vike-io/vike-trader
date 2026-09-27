//! `instruments` — **the venue's own answer to which book a symbol names**, read once per process
//! off the keyless public instrument lists.
//!
//! Two questions live here, and they are different questions:
//!
//!   1. *"Can this bare symbol mean another book?"* — `spot ∩ inverse`, the AMBIGUITY predicate that
//!      lets `crates/bridges/bybit/src/data.rs`'s `route_target` REFUSE an unclaimed bare symbol
//!      instead of guessing spot. Phase 0 (*the interim*) of
//!      `docs/decisions/0061-an-instrument-names-its-kind.md`.
//!   2. *"Which derivative book does a PERPETUAL claim on this symbol name?"* — [`perp_book`], phase
//!      4's half: the fact that turns `rest_category`'s two values into three and picks
//!      `crates/bridges/bybit/src/market_feed.rs`'s WS host. A perpetual claim
//!      ([`vike_catalog::PERP_SUFFIX`], or `AssetClass::CryptoPerp`) names the PRODUCT; it does not
//!      name the book, and until this existed every site in the crate answered "linear" by
//!      construction.
//!
//! # ⚠ THE AMBIGUITY PREDICATE: spot ∩ INVERSE. Not spot ∩ "a derivative category".
//!
//! **MEASURED against the live keyless lists, 2026-09-16** (and re-derivable without touching this
//! file — [`report_the_counts_that_size_this_module`] prints every number below from these same
//! functions):
//!
//! | intersection | size | what a bare symbol there resolves to |
//! |---|---|---|
//! | `spot ∩ inverse` | **2** (`BTCUSD`, `ETHUSD`) | a VESTIGIAL spot listing, while the liquid market of that name is the inverse perp |
//! | `spot ∩ linear` | **290** | a liquid, real spot market — which is exactly what the caller meant |
//!
//! 0061's Phase 0 says to refuse *"an unsuffixed symbol the venue lists under a derivative category
//! as well as spot"*. Read literally that is 292 symbols, and **290 of them are cases where the spot
//! default is CORRECT**: bare `BTCUSDT` meaning the spot pair is what an operator expects, and
//! [`vike_catalog::PERP_SUFFIX`] already names the linear perp as `BTCUSDT.P`. Shipping the literal
//! predicate would break every ordinary bybit spot chart in order to fix two symbols. **So
//! `spot ∩ linear` is deliberately NOT refused** — that is not a gap to be closed later, and the two
//! counts above are why.
//!
//! What makes the two differ is not the category pair. It is that for `BTCUSD` the spot side is
//! vestigial: 0061's own measurement 2 recorded four of six bars carrying zero volume and
//! flat-lining a price roughly seventy dollars off the perp's, on a listing whose `status` is
//! nonetheless `Trading`. The bare→spot rule is internally consistent there and practically wrong.
//!
//! # ⚠ THE ROUTE PREDICATE rests on ONE measurement, and that measurement is now a GATE
//!
//! **MEASURED 2026-09-16 — the number the ambiguity work never computed: `linear ∩ inverse` is
//! EMPTY.** 873 Trading linear symbols, 28 Trading inverse ones, and not one string in both. That
//! emptiness is the whole reason [`perp_book`] can answer from the venue's own listings with no
//! extra input from the caller: a perpetual claim plus a symbol names exactly one derivative book,
//! for every one of the venue's perpetuals, with nothing guessed from the symbol's SHAPE.
//!
//! It is a measurement of the venue today rather than a law, so it is not written down as one:
//! [`PerpBook::Both`] is the arm for a symbol appearing in both lists, [`perp_book`] returns it, and
//! `route_target` REFUSES on it. That arm is unreachable against the live venue today, and that is
//! exactly what makes the emptiness a GATE instead of a dated claim in prose — the day bybit lists
//! one string in both derivative categories, this refuses rather than silently picking whichever
//! book the code happened to test first.
//!
//! # ⚠ THE RESIDUAL, recorded rather than left to be discovered
//!
//! **`spot ∩ inverse` is a PROXY.** The true property is *"the spot side of this collision is
//! vestigial"*, and vestigial is not something an instrument list reports — `status: "Trading"` is
//! all the venue says, and it says it for both sides. The intersection happens to select exactly the
//! vestigial cases on bybit today. If bybit ever lists a LIQUID spot pair whose string collides with
//! an inverse perpetual, this refusal fires where the spot default was fine. That is a cheap false
//! positive (the operator names the class, or picks the perp row out of the picker) and an accepted
//! one; what would not be acceptable is for it to be undocumented.
//!
//! # ⚠ The sets are COMPUTED, never hardcoded
//!
//! `["BTCUSD", "ETHUSD"]` as a literal would silently stop covering a third listing the day bybit
//! adds one, and a refusal that quietly narrows is worse than one that never existed, because nobody
//! re-checks it. That goes double for the inverse perpetuals: a hardcoded roster of them would route
//! a newly-listed one to `linear`, which is the failure this module removes.
//! [`ambiguous_bare_symbols`] and [`perp_book`] are the pure decisions; [`listings_live`] is the
//! once-per-process derivation.
//!
//! # What it costs, and how often
//!
//! THREE keyless unsigned GETs (`instruments-info?category=` `spot`, `linear`, `inverse`), each
//! paged to exhaustion, **once per process** — about 1.1 MB, dominated by the linear list.
//!
//! ⚠ **That is one GET more than the ambiguity predicate alone needed, and the REACH is wider too.**
//! Before this, a `.P`-suffixed symbol and a symbol whose caller named a class both routed without
//! opening a socket at all; now every PERPETUAL route consults the listings. That is the price of
//! the book being READ off the venue instead of assumed, and it is paid once per process: the
//! derivation is cached for the life of the process, and a spot route still never reaches it.
//!
//! A fetch FAILURE is not cached, so a transient outage is retried by the next call rather than
//! poisoning the process; and it is a REFUSAL rather than a fall-through, because "could not prove
//! which book this is" and "proved which book this is" must not look the same — that equivalence is
//! the fail-permissive shape 0061 forbids. The instrument lists live on the same host as the
//! klines, so a box that cannot read them generally cannot fetch bars either.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde_json::Value;

/// The keyless spot instrument list. ⚠ `limit=1000` is load-bearing and is why this is not
/// `crates/bridges/bybit/src/catalog.rs`'s `SPOT_URL`: that one passes no `limit`, so it takes the
/// venue's default page and a set this predicate under-reads would fail PERMISSIVE — the one
/// direction that must not happen. [`fetch_trading_symbols`] also follows `nextPageCursor`, so the
/// cap is a page size rather than a ceiling.
const SPOT_INSTRUMENTS_URL: &str =
    "https://api.bybit.com/v5/market/instruments-info?category=spot&limit=1000";
/// The keyless LINEAR (quote-settled) instrument list — the larger half of the route predicate, and
/// the reason a perpetual claim now costs a third GET. Same `limit` and paging contract as its
/// siblings, for the same fail-permissive reason.
const LINEAR_INSTRUMENTS_URL: &str =
    "https://api.bybit.com/v5/market/instruments-info?category=linear&limit=1000";
/// The keyless INVERSE (coin-settled) instrument list — the category whose Trading rows are what
/// both predicates here exist to see: the two that collide with a spot pair, and the perpetuals a
/// `.P` claim used to reach as `category=linear` by the venue's leniency alone.
const INVERSE_INSTRUMENTS_URL: &str =
    "https://api.bybit.com/v5/market/instruments-info?category=inverse&limit=1000";

/// Bound on the cursor walk. A page is 1000 rows and the largest category measured 873, so one page
/// is the observed reality; the bound exists so a venue-side change cannot spin here forever.
const MAX_PAGES: usize = 20;

/// **Which derivative book a PERPETUAL claim on one bybit symbol names** — the venue's own answer,
/// read off its instrument listings rather than inferred from the symbol's shape.
///
/// This type is what turns `crates/bridges/bybit/src/data.rs`'s `rest_category` from a two-valued
/// spot-vs-linear question into a three-valued one, and it picks the WS host in
/// `crates/bridges/bybit/src/market_feed.rs`. A perpetual claim ([`vike_catalog::PERP_SUFFIX`], or
/// `AssetClass::CryptoPerp`) names the PRODUCT; nothing in the core vocabulary names the book, and
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 1 refuses to add a variant that
/// would. So the book is a venue-local FACT, looked up here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerpBook {
    /// Trading on `category=linear` and not on `category=inverse`: the quote-settled book, and the
    /// answer for the overwhelming majority of the venue's Trading perpetuals.
    Linear,
    /// Trading on `category=inverse` and not on `category=linear`: the coin-settled book.
    Inverse,
    /// Trading on BOTH derivative categories. **Unreachable against the live venue today** —
    /// `linear ∩ inverse` is EMPTY (MEASURED 2026-09-16) — and it exists so that emptiness is a
    /// GATE rather than a dated sentence: a caller gets a refusal naming the collision instead of
    /// whichever book this code happened to test first.
    Both,
    /// Trading in NEITHER derivative category — an unknown, delisted or misspelled symbol.
    /// Deliberately its own variant rather than folded into [`PerpBook::Linear`]: "measured linear"
    /// and "never measured" are different facts, and conflating them is how a routing default hides
    /// a typo. It still ROUTES to linear (byte-identical to this crate's behaviour before the
    /// lookup existed) — no route is right for a symbol the venue does not list, and the venue
    /// rejects it by name on whichever category it is asked of.
    Unlisted,
}

/// Every `status: "Trading"` symbol in one `{result:{list}}` page, upper-cased. Pure.
///
/// A non-Trading row is deliberately dropped: a delisted spot pair cannot be what a caller reached
/// by accident, and counting one would widen the refusal past what the venue actually serves.
#[must_use]
pub fn trading_symbols(payload: &Value) -> BTreeSet<String> {
    let Some(list) = payload.get("result").and_then(|r| r.get("list")).and_then(|l| l.as_array())
    else {
        return BTreeSet::new();
    };
    list.iter()
        .filter(|e| e.get("status").and_then(Value::as_str) == Some("Trading"))
        .filter_map(|e| e.get("symbol").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .collect()
}

/// The `result.nextPageCursor` of one page, or `None` when the venue says there is no more. Pure.
///
/// Bybit sends the field as an EMPTY STRING rather than omitting it when a listing is exhausted, so
/// "present" is not "more pages" and an empty value must read as the end.
#[must_use]
pub fn next_page_cursor(payload: &Value) -> Option<String> {
    payload
        .get("result")
        .and_then(|r| r.get("nextPageCursor"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

/// **The ambiguity predicate.** Bare symbols this venue lists Trading under BOTH spot and inverse —
/// see this module's doc for why the linear category is not in it. Pure; the whole decision is here.
#[must_use]
pub fn ambiguous_bare_symbols(
    spot: &BTreeSet<String>,
    inverse: &BTreeSet<String>,
) -> BTreeSet<String> {
    spot.intersection(inverse).cloned().collect()
}

/// **The route predicate.** Which derivative book `symbol` names, from the venue's two derivative
/// listings. Pure; the whole decision is here, and it is a pair of set memberships rather than any
/// reading of the string — a quote asset of `USD` rather than `USDT` is what bybit's inverse perps
/// happen to carry today, and routing on that would be the implicit encoding 0061 exists to remove.
#[must_use]
pub fn perp_book(symbol: &str, linear: &BTreeSet<String>, inverse: &BTreeSet<String>) -> PerpBook {
    let s = symbol.to_uppercase();
    match (linear.contains(&s), inverse.contains(&s)) {
        (true, true) => PerpBook::Both,
        (false, true) => PerpBook::Inverse,
        (true, false) => PerpBook::Linear,
        (false, false) => PerpBook::Unlisted,
    }
}

/// Walk one category's pages to exhaustion over an INJECTED fetcher, accumulating Trading symbols.
///
/// The fetcher takes a full URL so a test drives the paging without a socket. Paging matters for the
/// same reason `limit=1000` does: a SHORT read makes these predicates answer "unambiguous" or
/// "linear" for a symbol they simply did not see.
fn fetch_trading_symbols(
    base_url: &str,
    fetch: &impl Fn(&str) -> Result<Value, String>,
) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let url = match &cursor {
            None => base_url.to_string(),
            Some(c) => format!("{base_url}&cursor={c}"),
        };
        let payload = fetch(&url)?;
        out.extend(trading_symbols(&payload));
        match next_page_cursor(&payload) {
            None => return Ok(out),
            Some(next) => cursor = Some(next),
        }
    }
    Err(format!(
        "bybit instruments-info: {base_url} still offered a page after {MAX_PAGES} — refusing to \
         answer from a list that may be short"
    ))
}

/// The venue's derivative listings plus the ambiguity set derived alongside them — everything this
/// module answers from, in ONE cache entry so the two questions can never disagree about which
/// listing snapshot they read.
#[derive(Debug)]
struct Listings {
    linear: BTreeSet<String>,
    inverse: BTreeSet<String>,
    /// `spot ∩ inverse`. The spot list itself is not kept: nothing asks a second question of it, and
    /// holding 538 strings for the life of the process to answer none of them is not free.
    ambiguous: BTreeSet<String>,
}

/// The pure half of the once-per-process derivation: all three categories through one injected
/// fetcher. ANY of them failing fails the whole thing — a partial answer is the fail-permissive
/// shape whichever half is the missing one.
fn derive_listings(fetch: &impl Fn(&str) -> Result<Value, String>) -> Result<Listings, String> {
    let spot = fetch_trading_symbols(SPOT_INSTRUMENTS_URL, fetch)?;
    let linear = fetch_trading_symbols(LINEAR_INSTRUMENTS_URL, fetch)?;
    let inverse = fetch_trading_symbols(INVERSE_INSTRUMENTS_URL, fetch)?;
    let ambiguous = ambiguous_bare_symbols(&spot, &inverse);
    Ok(Listings { linear, inverse, ambiguous })
}

/// The process-wide cache. Only ever set from a SUCCESSFUL derivation — a failure returns `Err` and
/// leaves this empty, so the next caller retries instead of inheriting an outage forever.
static LISTINGS: OnceLock<Listings> = OnceLock::new();

fn fetch_json(url: &str) -> Result<Value, String> {
    vike_bridge_core::http::get_json(url)
}

/// The venue's Trading listings, derived once per process. Network I/O on the first call only.
///
/// # Errors
///
/// The venue's own error, verbatim, when any list cannot be read. A caller must treat that as "not
/// proven" — never as a default.
fn listings_live() -> Result<&'static Listings, String> {
    if let Some(cached) = LISTINGS.get() {
        return Ok(cached);
    }
    let derived = derive_listings(&fetch_json)?;
    tracing::info!(
        target: "vike_bybit::instruments",
        ambiguous = derived.ambiguous.len() as u64,
        linear = derived.linear.len() as u64,
        inverse = derived.inverse.len() as u64,
        symbols = %derived.ambiguous.iter().cloned().collect::<Vec<_>>().join(","),
        "bybit instrument listings read once: `symbols` is the bare set listed under BOTH spot and \
         inverse, which refuses an unclaimed route; `linear`/`inverse` are what a perpetual claim \
         is routed by"
    );
    let _ = LISTINGS.set(derived);
    Ok(LISTINGS.get().expect("just set"))
}

/// Every bare symbol bybit lists under spot AND inverse, derived once per process. Network I/O on
/// the first call only.
///
/// # Errors
///
/// The venue's own error, verbatim, when the listings cannot be read. A caller must treat that as
/// "not proven unambiguous" — never as "unambiguous".
pub fn ambiguous_bare_symbols_live() -> Result<&'static BTreeSet<String>, String> {
    Ok(&listings_live()?.ambiguous)
}

/// Is this bare symbol one the venue lists under spot AND inverse? Network I/O on the first call per
/// process; see [`ambiguous_bare_symbols_live`].
///
/// # Errors
///
/// When the instrument lists cannot be read — which is NOT the same answer as `Ok(false)`.
pub(crate) fn bare_symbol_is_ambiguous(symbol: &str) -> Result<bool, String> {
    Ok(listings_live()?.ambiguous.contains(&symbol.to_uppercase()))
}

/// Which derivative book a PERPETUAL claim on `symbol` names, per the venue's own listings. Network
/// I/O on the first call per process — the SAME derivation the ambiguity predicate uses, so a
/// process that has asked either question has already paid for both.
///
/// # Errors
///
/// When the instrument lists cannot be read — which is NOT the same answer as
/// `Ok(PerpBook::Linear)`, and no caller may treat it as one.
pub fn perp_book_for(symbol: &str) -> Result<PerpBook, String> {
    let l = listings_live()?;
    Ok(perp_book(symbol, &l.linear, &l.inverse))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(symbols: &[(&str, &str)], cursor: &str) -> Value {
        let list: Vec<Value> = symbols
            .iter()
            .map(|(s, status)| serde_json::json!({ "symbol": s, "status": status }))
            .collect();
        serde_json::json!({ "result": { "list": list, "nextPageCursor": cursor } })
    }

    #[test]
    fn only_trading_rows_count() {
        let p = page(&[("BTCUSD", "Trading"), ("OLDUSD", "Delisted"), ("", "Trading")], "");
        let got = trading_symbols(&p);
        assert_eq!(got.len(), 1);
        assert!(got.contains("BTCUSD"));
    }

    #[test]
    fn a_missing_envelope_is_an_empty_page_not_a_panic() {
        assert!(trading_symbols(&serde_json::json!({})).is_empty());
        assert!(trading_symbols(&serde_json::json!({ "result": { "list": "nope" } })).is_empty());
    }

    /// ⚠ Bybit sends an EMPTY STRING rather than omitting the field when a listing is exhausted.
    /// Reading "present" as "more pages" would spin the walk to its bound on every single fetch.
    #[test]
    fn an_empty_cursor_is_the_end_of_the_listing() {
        assert_eq!(next_page_cursor(&page(&[], "")), None);
        assert_eq!(next_page_cursor(&page(&[], "abc")), Some("abc".to_string()));
        assert_eq!(next_page_cursor(&serde_json::json!({ "result": {} })), None);
    }

    /// **THE AMBIGUITY PREDICATE, stated as a test.** spot ∩ inverse, and NOT spot ∩ linear — the
    /// 290-symbol case that must keep routing to spot is the second half of this assertion.
    #[test]
    fn the_predicate_is_spot_intersect_inverse() {
        let spot: BTreeSet<String> =
            ["BTCUSD", "ETHUSD", "BTCUSDT", "SOLUSDT"].iter().map(|s| s.to_string()).collect();
        let inverse: BTreeSet<String> =
            ["BTCUSD", "ETHUSD", "XRPUSD"].iter().map(|s| s.to_string()).collect();
        let ambiguous = ambiguous_bare_symbols(&spot, &inverse);
        assert_eq!(
            ambiguous,
            ["BTCUSD", "ETHUSD"].iter().map(|s| s.to_string()).collect::<BTreeSet<_>>()
        );
        assert!(
            !ambiguous.contains("BTCUSDT"),
            "a symbol listed under spot and LINEAR must stay unambiguous — 290 of them on the live \
             venue, and bare->spot is correct for every one"
        );
        assert!(
            !ambiguous.contains("XRPUSD"),
            "an inverse-only symbol has no spot listing to be confused with"
        );
    }

    /// **THE ROUTE PREDICATE, stated as a test.** Every one of the four answers, including the two
    /// that exist only so a venue-side change cannot pass silently.
    #[test]
    fn the_route_predicate_reads_both_derivative_listings() {
        let linear: BTreeSet<String> =
            ["BTCUSDT", "SOLUSDT", "BOTHUSD"].iter().map(|s| s.to_string()).collect();
        let inverse: BTreeSet<String> =
            ["BTCUSD", "XRPUSD", "BOTHUSD"].iter().map(|s| s.to_string()).collect();

        assert_eq!(perp_book("BTCUSDT", &linear, &inverse), PerpBook::Linear);
        assert_eq!(perp_book("BTCUSD", &linear, &inverse), PerpBook::Inverse);
        assert_eq!(
            perp_book("XRPUSD", &linear, &inverse),
            PerpBook::Inverse,
            "an inverse-only symbol with NO spot twin still routes — 26 of the venue's 28 inverse \
             listings are this case, and asking their caller to name a class would be ceremony"
        );
        assert_eq!(
            perp_book("BOTHUSD", &linear, &inverse),
            PerpBook::Both,
            "the arm that makes `linear n inverse = {{}}` a gate rather than a dated claim"
        );
        assert_eq!(perp_book("NOSUCHUSDT", &linear, &inverse), PerpBook::Unlisted);
    }

    /// ⚠ The predicate is a set MEMBERSHIP, never a reading of the string. `USD`-vs-`USDT` is what
    /// bybit's inverse perps happen to carry today and is exactly the implicit encoding 0061 exists
    /// to remove — so a `…USD` symbol the venue lists on LINEAR must answer `Linear`, and a
    /// `…USDT` one it lists on INVERSE must answer `Inverse`. Both are hypothetical on the live
    /// venue; that is the point.
    #[test]
    fn the_route_predicate_does_not_read_the_quote_asset_off_the_string() {
        let linear: BTreeSet<String> = ["WEIRDUSD".to_string()].into_iter().collect();
        let inverse: BTreeSet<String> = ["WEIRDUSDT".to_string()].into_iter().collect();
        assert_eq!(perp_book("WEIRDUSD", &linear, &inverse), PerpBook::Linear);
        assert_eq!(perp_book("WEIRDUSDT", &linear, &inverse), PerpBook::Inverse);
    }

    /// Both predicates are case-insensitive on the way in — the listings are upper-cased on parse,
    /// and a caller's lower-case symbol must not read as "not listed".
    #[test]
    fn a_lower_case_symbol_still_finds_its_listing() {
        let linear: BTreeSet<String> = ["BTCUSDT".to_string()].into_iter().collect();
        let inverse: BTreeSet<String> = ["BTCUSD".to_string()].into_iter().collect();
        assert_eq!(perp_book("btcusd", &linear, &inverse), PerpBook::Inverse);
        assert_eq!(perp_book("btcusdt", &linear, &inverse), PerpBook::Linear);
    }

    /// The walk follows the cursor and unions the pages. A single-page test cannot see a short read,
    /// which is the failure these predicates must not have.
    #[test]
    fn the_walk_follows_the_cursor_to_exhaustion() {
        let fetch = |url: &str| -> Result<Value, String> {
            if url.contains("cursor=p2") {
                Ok(page(&[("CUSD", "Trading")], ""))
            } else {
                Ok(page(&[("AUSD", "Trading"), ("BUSD", "Trading")], "p2"))
            }
        };
        let got = fetch_trading_symbols("https://example.invalid/x?category=spot", &fetch)
            .expect("walk completes");
        assert_eq!(got.len(), 3);
        assert!(got.contains("CUSD"), "the second page must be unioned in, not dropped");
    }

    /// A venue that keeps offering pages is a REFUSAL, not a partial answer: an under-read set makes
    /// these predicates answer "unambiguous"/"linear" for a symbol they never saw.
    #[test]
    fn an_endless_listing_refuses_rather_than_answering_short() {
        let fetch = |_: &str| -> Result<Value, String> { Ok(page(&[("AUSD", "Trading")], "more")) };
        let err = fetch_trading_symbols("https://example.invalid/x", &fetch)
            .expect_err("an endless listing must not answer");
        assert!(err.contains("may be short"), "{err}");
    }

    /// A fetch failure propagates rather than becoming an empty set — an empty set would read as
    /// "nothing is ambiguous" and "no symbol is inverse", which is the fail-permissive shape this
    /// whole module exists to avoid.
    #[test]
    fn a_fetch_failure_is_an_error_not_an_empty_set() {
        let fetch = |_: &str| -> Result<Value, String> { Err("HTTP 503".into()) };
        assert!(derive_listings(&fetch).is_err());
    }

    /// ANY one list failing must not answer from the others. Each category gets its own round of
    /// this: an answer assembled from two of three is a different kind of wrong for each hole.
    #[test]
    fn any_single_list_failing_refuses_the_whole_derivation() {
        for missing in ["category=spot", "category=linear", "category=inverse"] {
            let fetch = |url: &str| -> Result<Value, String> {
                if url.contains(missing) {
                    Err("HTTP 503".into())
                } else {
                    Ok(page(&[("BTCUSD", "Trading")], ""))
                }
            };
            assert!(
                derive_listings(&fetch).is_err(),
                "{missing} failing must refuse the whole derivation"
            );
        }
    }

    /// All three categories are consulted, and each exactly once on a single-page venue. The
    /// `limit=1000` half is the fail-permissive guard, asserted on every URL rather than on one.
    #[test]
    fn all_three_categories_are_read() {
        let seen = std::sync::Mutex::new(Vec::new());
        let fetch = |url: &str| -> Result<Value, String> {
            seen.lock().expect("lock").push(url.to_string());
            Ok(page(&[("BTCUSD", "Trading")], ""))
        };
        let got = derive_listings(&fetch).expect("derives");
        let urls = seen.lock().expect("lock").clone();
        assert_eq!(urls.len(), 3, "one page per category: {urls:?}");
        for category in ["category=spot", "category=linear", "category=inverse"] {
            assert!(urls.iter().any(|u| u.contains(category)), "{category} unread: {urls:?}");
        }
        assert!(urls.iter().all(|u| u.contains("limit=1000")), "a short page fails permissive");
        assert!(got.ambiguous.contains("BTCUSD"));
        assert_eq!(
            perp_book("BTCUSD", &got.linear, &got.inverse),
            PerpBook::Both,
            "this fixture plants BTCUSD in every category, so the collision arm is what answers"
        );
    }

    /// **The live re-derivation** — `#[ignore]`d, network, run by hand. It prints every number this
    /// module's doc is sized by, from this module's own code rather than from a shell one-liner, so
    /// "if it does not get these, one of us is wrong" is answerable at any time.
    ///
    /// ⚠ The `linear ∩ inverse` line is the one that decides the ROUTE, and it is asserted rather
    /// than merely printed: the production guard for a non-empty answer is [`PerpBook::Both`], which
    /// no live symbol can reach while this holds.
    ///
    /// ```sh
    /// cargo test -p vike-bybit --lib instruments::tests::report_the_counts -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "network: keyless public GETs against api.bybit.com"]
    fn report_the_counts_that_size_this_module() {
        let spot = fetch_trading_symbols(SPOT_INSTRUMENTS_URL, &fetch_json).expect("spot list");
        let linear =
            fetch_trading_symbols(LINEAR_INSTRUMENTS_URL, &fetch_json).expect("linear list");
        let inverse =
            fetch_trading_symbols(INVERSE_INSTRUMENTS_URL, &fetch_json).expect("inverse list");
        let refused = ambiguous_bare_symbols(&spot, &inverse);
        let not_refused: BTreeSet<String> = spot.intersection(&linear).cloned().collect();
        let both_books: BTreeSet<String> = linear.intersection(&inverse).cloned().collect();
        println!(
            "bybit trading symbols: spot={} inverse={} linear={}",
            spot.len(),
            inverse.len(),
            linear.len()
        );
        println!(
            "REFUSED   spot n inverse = {} {:?}",
            refused.len(),
            refused.iter().collect::<Vec<_>>()
        );
        println!("ALLOWED   spot n linear  = {} (deliberately not refused)", not_refused.len());
        println!(
            "ROUTABLE  linear n inverse = {} {:?} (must be 0 — see PerpBook::Both)",
            both_books.len(),
            both_books.iter().collect::<Vec<_>>()
        );
        assert!(!refused.is_empty(), "the predicate selected nothing — re-read this module's doc");
        assert!(
            both_books.is_empty(),
            "linear n inverse is NO LONGER EMPTY: {both_books:?}. A perpetual claim on those \
             symbols now names two books and `route_target` refuses them — which is the designed \
             behaviour, but this module's doc claims the set is empty and must be re-measured"
        );
    }
}
