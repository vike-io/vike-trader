use super::*;

fn lane() -> CatalogLane {
    // A lane whose table is empty: these tests exercise the BUCKET and the MEMO, which are
    // independent of which providers are mounted.
    CatalogLane::new(CatalogTable::new(Vec::new()))
}

fn inst(venue: &str, sym: &str) -> Instrument {
    Instrument {
        venue: venue.into(),
        raw_symbol: sym.into(),
        asset_class: vike_model::AssetClass::CryptoSpot,
        base: "BTC".into(),
        quote: "USDT".into(),
        description: String::new(),
        properties: Default::default(),
        // ⚠ `None` is the ORDINARY state for both — see `vike_catalog::Instrument::contract_type`.
        contract_type: None,
        settle_asset: None,
    }
}

fn fake(venue: &str) -> (String, CatalogFn) {
    let v = venue.to_string();
    (venue.to_string(), Box::new(move || Ok(vec![inst(&v, "BTCUSDT")])))
}

#[test]
fn a_credentialed_venue_cannot_be_mounted_even_when_a_caller_asks_for_it() {
    // ⚠ THE MUTATION PROOF for 0062's decision 3, and it drives the PRODUCTION constructor
    // rather than a harness: `CatalogTable::new` is what `real_catalog_table` calls. A caller
    // handing it alpaca gets a table that does not serve alpaca — so the refusal cannot be
    // reordered past, because there is no order to get wrong.
    let t =
        CatalogTable::new(vec![fake("binance"), fake("alpaca"), fake("oanda"), fake("ctrader")]);
    assert_eq!(t.supported(), vec!["binance"]);
    for v in ["alpaca", "oanda", "ctrader"] {
        assert!(t.get(v).is_none(), "{v} must not be reachable through a mounted table");
    }
}

#[test]
fn a_venue_with_no_bulk_list_cannot_be_mounted_either() {
    // ig/ibkr are not credentialed — they have no list at all — and the same constructor rule
    // keeps them out, so the server can never answer `Listed { instruments: [] }` for them by
    // having mounted a provider that returns empty.
    let t = CatalogTable::new(vec![fake("ig"), fake("ibkr"), fake("okx")]);
    assert_eq!(t.supported(), vec!["okx"]);
}

#[test]
fn an_unknown_venue_cannot_be_mounted() {
    let t = CatalogTable::new(vec![fake("kraken"), fake("bybit")]);
    assert_eq!(t.supported(), vec!["bybit"]);
}

#[test]
fn a_fresh_memo_answers_without_spending_a_token() {
    // The server-side leg of "one fetch per TTL, not one per picker open". A client that ignores
    // its own cache and asks a thousand times gets one fetch and 999 free answers, and — the
    // part that matters — does not drain the venue bucket doing it.
    let l = lane();
    let t0 = Instant::now();
    assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    l.remember("binance", vec![inst("binance", "BTCUSDT")], t0);
    for _ in 0..1_000 {
        match l.admit("binance", t0) {
            CatalogAdmission::Cached(list) => assert_eq!(list.len(), 1),
            other => panic!("a fresh memo must answer, got {other:?}"),
        }
    }
    // ...and the bucket still holds its remaining burst: a DIFFERENT venue is unaffected and
    // binance itself still has one token left of two.
    assert_eq!(l.admit("okx", t0), CatalogAdmission::Fetch);
    assert_eq!(l.cached_venues(), 1);
}

#[test]
fn the_memo_expires_at_the_ttl_and_the_venue_is_asked_again() {
    let l = lane();
    let t0 = Instant::now();
    assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    l.remember("binance", vec![inst("binance", "BTCUSDT")], t0);
    // One millisecond before the TTL: still cached.
    let almost = t0 + CATALOG_TTL - Duration::from_millis(1);
    assert!(matches!(l.admit("binance", almost), CatalogAdmission::Cached(_)));
    // At the TTL: the memo is stale, and the bucket has long since refilled.
    assert_eq!(l.admit("binance", t0 + CATALOG_TTL), CatalogAdmission::Fetch);
}

#[test]
fn a_failed_fetch_memoizes_nothing_so_a_blip_is_not_six_hours_of_refusal() {
    // `remember` is called only on success — asserted by NOT calling it, which is the shape the
    // server's verb takes.
    let l = lane();
    let t0 = Instant::now();
    assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    assert_eq!(l.cached_venues(), 0, "a fetch that failed must leave no memo");
    // The second token is still there for the retry.
    assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
}

#[test]
fn the_burst_is_exhausted_then_refills_one_token_per_period() {
    let l = lane();
    let t0 = Instant::now();
    for _ in 0..CATALOG_VENUE_BURST {
        assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    }
    match l.admit("binance", t0) {
        CatalogAdmission::Refused(why) => {
            assert!(why.contains("budget is spent"), "{why}");
            assert!(why.contains("order-signing daemon"), "the reason is named: {why}");
        }
        other => panic!("the next fetch must be refused, got {other:?}"),
    }
    let t1 = t0 + CATALOG_VENUE_REFILL;
    assert_eq!(l.admit("binance", t1), CatalogAdmission::Fetch);
    assert!(matches!(l.admit("binance", t1), CatalogAdmission::Refused(_)));
}

#[test]
fn polling_faster_than_the_refill_still_accrues_tokens() {
    // The bucket bug this `take` is written against: advancing `last_refill` to `now` on every
    // call would discard the remainder, so a caller polling every 10 s against a 60 s period
    // would never accrue anything.
    let l = lane();
    let t0 = Instant::now();
    for _ in 0..CATALOG_VENUE_BURST {
        assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    }
    let mut got = None;
    for s in (10..=70).step_by(10) {
        if l.admit("binance", t0 + Duration::from_secs(s)) == CatalogAdmission::Fetch {
            got = Some(s);
            break;
        }
    }
    assert_eq!(got, Some(CATALOG_VENUE_REFILL.as_secs()), "a token must accrue on schedule");
}

#[test]
fn the_buckets_are_per_venue_so_the_heavy_venues_cannot_starve_binance() {
    // The first of `CATALOG_VENUE_REFILL`'s three "real work" properties, held rather than
    // asserted in prose: polymarket's 40-page walk spends polymarket's budget.
    let l = lane();
    let t0 = Instant::now();
    for _ in 0..CATALOG_VENUE_BURST {
        assert_eq!(l.admit("polymarket", t0), CatalogAdmission::Fetch);
    }
    assert!(matches!(l.admit("polymarket", t0), CatalogAdmission::Refused(_)));
    assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    assert_eq!(l.admit("okx", t0), CatalogAdmission::Fetch);
}

#[test]
fn the_memo_is_one_entry_per_venue_not_per_request() {
    // The declared residue bound: this state is bounded by the TABLE's length, whatever a
    // client does. A thousand asks over three venues leave three entries.
    let l = lane();
    let t0 = Instant::now();
    for v in ["binance", "okx", "bybit"] {
        l.remember(v, vec![inst(v, "X")], t0);
    }
    for _ in 0..1_000 {
        for v in ["binance", "okx", "bybit"] {
            let _ = l.admit(v, t0);
        }
    }
    assert_eq!(l.cached_venues(), 3, "the memo must not grow with request count");
}
