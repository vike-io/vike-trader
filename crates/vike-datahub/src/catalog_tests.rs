use super::*;
use std::assert_matches;

fn lane() -> CatalogLane {
    // A lane whose table is empty: these tests exercise the BUCKET and the MEMO, which are
    // independent of which providers are mounted.
    CatalogLane::new(CatalogTable::new(Vec::new()), CATALOG_TTL)
}

fn inst(venue: &str, sym: &str) -> Instrument {
    Instrument::new(venue, sym, "BTC", "USDT", vike_model::Contract::CryptoSpot)
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
    assert_matches!(l.admit("binance", almost), CatalogAdmission::Cached(_));
    // At the TTL: the memo is stale, and the bucket has long since refilled.
    assert_eq!(l.admit("binance", t0 + CATALOG_TTL), CatalogAdmission::Fetch);
}

/// **A box without the `config.datahub_catalog_ttl_secs` row keeps the six hours it always had**,
/// and a box with one gets exactly the seconds it wrote — no rounding, no clamp (the range is
/// `vike-config`'s to refuse, and `catalog_ttl` reads what got through).
#[test]
fn the_ttl_row_is_the_memo_and_no_row_is_six_hours() {
    assert_eq!(catalog_ttl(None), CATALOG_TTL);
    assert_eq!(CATALOG_TTL, Duration::from_secs(6 * 60 * 60), "the default is six hours");
    assert_eq!(catalog_ttl(Some(600)), Duration::from_secs(600));
    assert_eq!(lane().ttl(), CATALOG_TTL, "the test lane is the default lane");

    let l = CatalogLane::new(CatalogTable::new(Vec::new()), catalog_ttl(Some(600)));
    assert_eq!(l.ttl(), Duration::from_secs(600));
    let t0 = Instant::now();
    assert_eq!(l.admit("binance", t0), CatalogAdmission::Fetch);
    l.remember("binance", vec![inst("binance", "BTCUSDT")], t0);
    // One millisecond before the row's TTL: still cached. At it: stale, and the venue is asked again
    // (the bucket has refilled by then, so the TTL is the only thing that decided).
    let almost = t0 + Duration::from_secs(600) - Duration::from_millis(1);
    assert_matches!(l.admit("binance", almost), CatalogAdmission::Cached(_));
    assert_eq!(l.admit("binance", t0 + Duration::from_secs(600)), CatalogAdmission::Fetch);
    // ...and the default lane at those same instants is still serving from memory.
    let d = lane();
    assert_eq!(d.admit("binance", t0), CatalogAdmission::Fetch);
    d.remember("binance", vec![inst("binance", "BTCUSDT")], t0);
    assert_matches!(d.admit("binance", t0 + Duration::from_secs(600)), CatalogAdmission::Cached(_));
}

/// **`vike-config` restates this lane's TTL range, and this holds the two equal.** That crate is
/// below this one and cannot name `CATALOG_VENUE_BURST`, so the floor of
/// `vike_config::config::DATAHUB_CATALOG_TTL_SECS` is a number it typed; the lane's own floor is
/// derived from the burst. If either moves, this reddens — and so does the compile-time assertion
/// beside `CATALOG_TTL` if the default ever falls under the floor. The default is also a value the
/// range takes, so writing it back to the row is legal.
#[test]
fn the_ttl_row_range_is_this_lanes_floor_and_holds_the_default() {
    let range = vike_config::config::DATAHUB_CATALOG_TTL_SECS;
    assert_eq!(
        u64::from(*range.start()),
        CATALOG_TTL_FLOOR.as_secs(),
        "vike-config refuses below the floor this lane derives from its burst"
    );
    assert_eq!(CATALOG_TTL_FLOOR.as_secs(), u64::from(CATALOG_VENUE_BURST) * 4 * 60);
    assert_eq!(u64::from(*range.end()), 7 * 24 * 60 * 60, "the ceiling is one week");
    assert!(range.contains(&u32::try_from(CATALOG_TTL.as_secs()).unwrap()));
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
    assert_matches!(l.admit("binance", t1), CatalogAdmission::Refused(_));
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
    assert_matches!(l.admit("polymarket", t0), CatalogAdmission::Refused(_));
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

/// **The catalog lane names no venue bridge.** Decision 0164: the providers are the gatewayhub's, and
/// `catalog.rs` holds the bounds and a `RemoteCatalog` per roster venue, nothing that dials a venue
/// from this process. A STRUCTURAL pin, and why it cannot be a behavioural one: a bridge path in code
/// would still list correctly, only from the wrong process. It reads the sibling file's code lines
/// (comments excluded), so the default lane runs it even though the function that used to name the
/// providers is compiled only under `catalog-serve`.
#[test]
fn the_catalog_lane_names_no_venue_bridge() {
    let src = include_str!("catalog.rs");
    let code = src
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for bridge in
        "binance bybit okx aster hyperliquid polymarket dukascopy fxcm deribit oanda".split(' ')
    {
        let path = format!("vike_{bridge}::");
        assert!(
            !code.contains(&path),
            "catalog.rs names `{path}` in code: the providers are the gatewayhub's"
        );
    }
    assert!(
        code.contains("RemoteCatalog::open(") && code.contains("CATALOG_VENUES"),
        "the production table is the client's roster, one `RemoteCatalog` per venue"
    );
}

/// The production table over a LIVE (loopback) gatewayhub: the testkit's real server around scripted
/// lanes, dialled by the client crate's real `RemoteCatalog`. Gated like the function it drives.
#[cfg(feature = "catalog-serve")]
mod over_the_gatewayhub {
    use super::*;
    use vike_catalog::CatalogProvider;
    use vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS;
    use vike_gatewayhub::testkit::data::ScriptedData;
    use vike_gatewayhub::testkit::gateway::LoopbackGateway;
    use vike_gatewayhub::testkit::rest::{Answer, FetchCall, ScriptedRest};
    use vike_gatewayhub_client::catalog::{CATALOG_VENUES, RemoteCatalog};
    use vike_gatewayhub_client::client::GatewayLink;
    use vike_gatewayhub_client::proto::{FetchWhat, WireCatalogMode};
    use vike_hub_proto::auth::HubKeys;
    use vike_model::AssetClass;

    /// `n` distinct instruments of `venue`.
    fn listing(venue: &str, n: usize) -> Vec<Instrument> {
        (0..n).map(|i| inst(venue, &format!("SYM{i}USDT"))).collect()
    }

    /// Every roster venue scripted with its own, differently sized listing (the venue at position `i`
    /// lists `i + 1` instruments), so a listing answered by the WRONG venue cannot pass for the right
    /// one. Scripted before the gateway starts: the server reads the lane's facts once.
    fn scripted_all() -> ScriptedRest {
        CATALOG_VENUES.iter().enumerate().fold(ScriptedRest::new(), |rest, (i, venue)| {
            rest.with_catalog(
                venue,
                vec![AssetClass::CryptoSpot],
                WireCatalogMode::Enumerable,
                listing(venue, i + 1),
            )
        })
    }

    fn keyless_link(addr: String) -> GatewayLink {
        GatewayLink::new(addr, HubKeys::new(Vec::new(), Vec::new()))
    }

    /// **Membership and order are the in-process table's**: the eight public providers, deribit
    /// absent, in the order a `NotServed` refusal prints them — and each one lists through the hop
    /// exactly what its venue answered, as ONE ordinary (non-credentialed) catalog fetch.
    #[test]
    fn the_table_holds_the_old_eight_in_the_old_order_and_each_lists_through_the_hop() {
        let rest = scripted_all();
        let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
        let table = real_catalog_table(&gw.link(), false);

        // The old table's order, spelled as one string.
        let old: Vec<&str> =
            "binance bybit okx aster hyperliquid polymarket dukascopy fxcm".split(' ').collect();
        assert_eq!(table.supported(), old);
        assert_eq!(table.supported(), CATALOG_VENUES.to_vec());
        for &venue in vike_model::VENUES {
            assert_eq!(
                table.get(venue).is_some(),
                catalog_availability(venue).is_public() && venue != "deribit",
                "`{venue}`: the table serves exactly the public venues, deribit left out"
            );
        }

        for (i, venue) in old.iter().enumerate() {
            let list = (table.get(venue).expect("served"))()
                .unwrap_or_else(|e| panic!("`{venue}` must list through the hop: {e}"));
            assert_eq!(list, listing(venue, i + 1), "`{venue}` lists its own universe");
        }
        let asked: Vec<FetchCall> = old
            .iter()
            .map(|venue| FetchCall {
                what: FetchWhat::CatalogList { venue: (*venue).to_string() },
                credentialed: false,
            })
            .collect();
        assert_eq!(rest.fetches(), asked, "one keyless catalog fetch per venue, in order");
    }

    /// **The cap is the datahub's.** The daemon sends the provider's FULL answer (it is paged) and
    /// `list_via` cuts it, exactly where the in-process provider seam did: the head is kept, in order.
    #[test]
    fn the_cap_is_the_datahubs_and_the_daemon_sends_the_whole_universe() {
        let whole = CATALOG_MAX_INSTRUMENTS + 7;
        let rest = ScriptedRest::new().with_catalog(
            "polymarket",
            vec![AssetClass::CryptoSpot],
            WireCatalogMode::Enumerable,
            listing("polymarket", whole),
        );
        let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest);
        let link = gw.link();

        let daemon_sent = RemoteCatalog::open(link.clone(), "polymarket")
            .expect("describe")
            .list_instruments()
            .expect("list");
        assert_eq!(daemon_sent.len(), whole, "the daemon truncates nothing");

        let table = real_catalog_table(&link, false);
        let kept = (table.get("polymarket").expect("served"))().expect("list");
        assert_eq!(kept.len(), CATALOG_MAX_INSTRUMENTS, "the datahub cuts at its own cap");
        assert!(
            kept[..] == daemon_sent[..CATALOG_MAX_INSTRUMENTS],
            "...and keeps the head in order"
        );
    }

    /// **A failing venue prints the words it printed in-process.** The daemon sends the provider's
    /// inner text; `CatalogError`'s `Display` re-adds `catalog error: ` in `list_via`, so the string
    /// the server wraps into `catalog {venue}: ...` is byte-identical to the old one. A failure is an
    /// `Err`, never an empty listing, and it leaves the other venues listing.
    #[test]
    fn a_failing_venue_prints_the_words_the_in_process_provider_printed() {
        let rest = scripted_all().with_catalog_list(
            "bybit",
            Answer::fetch_error("HTTP 503 from the exchangeInfo endpoint"),
        );
        let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest);
        let table = real_catalog_table(&gw.link(), false);

        let err = (table.get("bybit").expect("served"))().expect_err("the venue failed");
        assert_eq!(err, "catalog error: HTTP 503 from the exchangeInfo endpoint");
        assert_eq!(
            (table.get("okx").expect("served"))().expect("okx is unaffected"),
            listing("okx", 3)
        );
    }

    /// **A dead or unkeyed daemon is an error that names the gatewayhub, never an empty listing**
    /// (drift register R12: an empty list is a legitimate venue fact the picker would store). The
    /// unkeyed link is the production shape until the hub keys are provisioned: nothing is dialled.
    #[test]
    fn an_unreachable_or_unkeyed_daemon_is_an_error_never_an_empty_listing() {
        let mut gw = LoopbackGateway::start(ScriptedData::new(&[]), scripted_all());
        let link = gw.link();
        let addr = gw.addr_string();
        gw.stop().expect("stop the gateway");

        let table = real_catalog_table(&link, false);
        for venue in table.supported() {
            let err = (table.get(venue).expect("served"))().expect_err("nothing is listening");
            assert!(err.starts_with("catalog error: gatewayhub "), "`{venue}`: {err}");
            assert!(err.contains(&addr), "`{venue}` names the address it dialled: {err}");
        }

        let table = real_catalog_table(&keyless_link(addr), false);
        let err = (table.get("binance").expect("served"))().expect_err("no key, no dial");
        assert!(err.starts_with("catalog error: gatewayhub "), "{err}");
        assert!(err.contains("nothing was dialled"), "{err}");
    }

    /// **The HIP-3 flag is not silently dropped.** The wave-1 catalog verbs carry no per-request
    /// flag, so the daemon decides the hyperliquid universe; a flag that is ON here is announced once
    /// at build, and a flag that is off adds no line to a box that never had one.
    #[test]
    fn the_hip3_flag_is_announced_when_on_and_adds_no_line_when_off() {
        let link = keyless_link("127.0.0.1:1".to_string());

        let (_, off) = vike_log::capture::captured(|| real_catalog_table(&link, false));
        assert!(off.is_empty(), "flag off: no line about it: {off:?}");

        let (_, on) = vike_log::capture::captured(|| real_catalog_table(&link, true));
        assert_eq!(on.len(), 1, "flag on: announced once: {on:?}");
        assert_eq!(on[0].level, tracing::Level::INFO);
        assert!(on[0].message.contains("flags.hyperliquid_hip3"), "{}", on[0].message);
        assert!(on[0].message.contains("127.0.0.1:1"), "it names the daemon: {}", on[0].message);
    }
}
