use super::*;

use std::cell::RefCell;

use vike_model::{
    ContractCfd, ContractCfdExpiry, ContractCfdProduct, ContractCfdUnderlying, ContractEquity,
    ContractFx,
};

/// **No timer fills a split; the operator's refresh is the one caller** (the owner's ruling of 2026-11-16: filled during
/// the manual `vike-cli data catalog refresh`, no timer and no cron). This module starts no thread, the boot names
/// nothing of it, and the one call in the crate is the refresh verb's route.
///
/// MUTATION PROOF: call `with_yahoo_splits` from `datahub_cli.rs` or another verb, or put a `std::thread` here.
#[test]
fn no_timer_fills_a_split() {
    let module = include_str!("stock_splits.rs");
    for spawn in ["thread::", "spawn(", "sleep("] {
        assert!(!module.contains(spawn), "the splits' module starts no timer: `{spawn}`");
    }
    let mut callers = Vec::new();
    let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("src").map(|e| e.expect("entry").path()) {
            let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            if entry.is_dir() {
                dirs.push(entry);
            } else if name.ends_with(".rs") && !name.starts_with("stock_splits") {
                let text = std::fs::read_to_string(&entry).expect("utf-8");
                callers.extend(text.matches("stock_splits::").map(|_| name.clone()));
            }
        }
    }
    assert_eq!(callers, ["route.rs"], "the refresh verb's route is the one caller");
    let route = include_str!("server/route.rs");
    let at = route.find("stock_splits::with_yahoo_splits").expect("the call");
    let arm = route[..at].rfind("Request::").expect("an arm");
    assert!(route[arm..].starts_with("Request::RefreshInstrumentLists"), "only under the refresh");
}

fn share_cfd() -> Contract {
    Contract::Cfd(ContractCfd {
        underlying_kind: ContractCfdUnderlying::Share,
        underlying: None,
        product: ContractCfdProduct::Cfd,
        expiry: ContractCfdExpiry::Rolling,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    })
}

/// A dukascopy share CFD as the operator added it (no base) or as the login lists it (`base` given: its ticker,
/// decision 0290).
fn cfd(raw: &str, base: &str) -> Instrument {
    let quote = raw.rsplit_once('/').map_or("", |(_, q)| q);
    Instrument::new("dukascopy", raw, base, quote, share_cfd())
}

fn share(venue: &str, raw: &str, exchange: Option<&str>) -> Instrument {
    let listing =
        ContractEquity { exchange: exchange.map(str::to_string), currency: Some("USD".into()) };
    Instrument::new(venue, raw, raw, "USD", Contract::Equity(listing))
}

/// **A share maps to its Yahoo ticker from its row's facts alone, never a guess**: a US listing is the plain ticker,
/// a share class after its dot written Yahoo's way (`BRK.B` is `BRK-B`); a non-US one only where the row states an
/// exchange Yahoo has a suffix for; a country, no listing, an exchange with no known suffix, a name that is no plain
/// ticker, and IG's epics (no listing stated) map to nothing.
#[test]
fn a_share_maps_to_its_yahoo_ticker_only_from_its_facts() {
    let ticker = |row: &Instrument| yahoo_ticker(row);
    assert_eq!(ticker(&cfd("AAPL.US/USD", "")), Ok("AAPL".into()), "an operator's row");
    assert_eq!(ticker(&cfd("NFLX.US/USD", "NFLX")), Ok("NFLX".into()), "a listed row");
    assert_eq!(ticker(&share("alpaca", "AAPL", Some("NASDAQ"))), Ok("AAPL".into()));
    assert_eq!(ticker(&share("ibkr", "VOD", Some("LSE"))), Ok("VOD.L".into()), "a stated exchange");
    assert_eq!(
        ticker(&share("alpaca", "BRK.B", Some("NYSE"))),
        Ok("BRK-B".into()),
        "a share class"
    );
    assert_eq!(ticker(&share("alpaca", "BF.B", Some("NYSE"))), Ok("BF-B".into()));

    let none = |row: Instrument, words: &str| {
        let why = yahoo_ticker(&row).expect_err(&row.raw_symbol);
        assert!(why.contains(words), "{}: {why}", row.raw_symbol);
    };
    none(cfd("BMW.DE/EUR", "BMW"), "a country, not an exchange");
    none(cfd("7203.JP/JPY", ""), "a country, not an exchange");
    none(share("ibkr", "AAPL", None), "no listing exchange");
    none(share("ibkr", "7203", Some("TSEJ")), "no suffix known here for the exchange TSEJ");
    none(share("alpaca", "BRK.B.C", Some("NYSE")), "no plain ticker");
    none(share("ibkr", "VOD.LSE", Some("LSE")), "no plain ticker");
    none(Instrument::new("ig", "UA.D.AAPL.CASH.IP", "", "", share_cfd()), "no listing");

    assert!(!Contract::Fx(ContractFx::default()).is_share());
    assert!(cfd("AAPL.US/USD", "").contract.is_share());
    assert!(share("alpaca", "AAPL", None).contract.is_share());
}

/// Yahoo's answers: AAPL and NFLX as Yahoo gave them on 2026-11-16 (`vike_backfill::testing`), CMCSA's two splits as
/// it lists them (a 2:1 of 2017 and Versant's spin-off of 2026-01-05 as `1067:1000`), any other ticker a 404.
fn yahoo_fixture(ticker: &str) -> YahooAnswer {
    let body = match ticker {
        "AAPL" => vike_backfill::testing::YAHOO_AAPL_SPLITS,
        "NFLX" => vike_backfill::testing::YAHOO_NFLX_SPLITS,
        "CMCSA" => {
            return Ok(vec![
                ("2017-02-21".to_string(), 2.0, 1.0),
                ("2026-01-05".to_string(), 1067.0, 1000.0),
            ]);
        }
        _ => return Err(YahooRefusal::Unlisted("venue fetch: HTTP 404".to_string())),
    };
    let listed =
        vike_backfill::eod::yahoo::parse_yahoo_splits(&serde_json::from_str(body).expect("json"))
            .map_err(|e| e.to_string())?;
    Ok(listed.into_iter().map(|s| (s.date, s.numerator, s.denominator)).collect())
}

/// FMP's three answers as measured 2026-11-16: AAPL and NFLX list their splits (`tests/fixtures/fmp/`), CMCSA is
/// refused (`402`, the transport's words; the body is `CMCSA-premium.txt`).
fn fmp_fixture(ticker: &str) -> FmpAnswer {
    match ticker {
        "AAPL" => parse_fmp_splits(include_str!("../tests/fixtures/fmp/AAPL-splits.json")),
        "NFLX" => parse_fmp_splits(include_str!("../tests/fixtures/fmp/NFLX-splits.json")),
        _ => Err("HTTP 402".to_string()),
    }
}

fn settings_store() -> tempfile::TempDir {
    let settings = tempfile::tempdir().expect("temp");
    vike_secrets::create_store(Some(settings.path().to_str().expect("utf-8"))).expect("a store");
    settings
}

/// 2020-01-02 and 2025-08-01, 00:00 UTC: the first bars of the store's AAPL and other share CFD series.
const FROM_2020: i64 = 1_577_923_200_000;
const FROM_2025_08: i64 = 1_754_006_400_000;

fn held(series: &[(&str, i64)]) -> BTreeMap<String, i64> {
    series.iter().map(|(s, first)| (s.to_string(), *first)).collect()
}

/// **FMP's three answers parse, and each Yahoo split is judged by them**: a date FMP lists as a split is applied
/// under `yahoo+fmp`; a date it does not list, or lists as another type, is NOT; a refusal (premium) or a failure
/// falls to the ratio rule, which applies `2:1` and refuses CMCSA's `1067:1000` as a spin-off; a split FMP was not
/// asked about (it moves no stored bar) is the ratio rule's too.
#[test]
fn fmps_three_answers_and_the_ratio_rule_judge_each_split() {
    let nflx = fmp_fixture("NFLX").expect("FMP lists NFLX");
    assert!(nflx.contains(&("2025-11-17".to_string(), "stock-split".to_string())), "{nflx:?}");
    assert!(fmp_fixture("AAPL").expect("FMP lists AAPL").len() == 5);
    let premium = parse_fmp_splits(include_str!("../tests/fixtures/fmp/CMCSA-premium.txt"));
    assert!(premium.is_err(), "the premium refusal is no list: {premium:?}");

    let confirmed = judge("2025-11-17", 10.0, 1.0, Some(&Ok(nflx.clone())));
    assert_eq!(confirmed, (Some(YAHOO_FMP), "FMP confirms the split".to_string()));
    let unlisted: FmpAnswer = Ok(nflx.iter().filter(|(d, _)| d != "2025-11-17").cloned().collect());
    let other_type: FmpAnswer = Ok(vec![("2025-11-17".to_string(), "spin-off".to_string())]);
    for answer in [unlisted, other_type] {
        let judged = judge("2025-11-17", 10.0, 1.0, Some(&answer));
        assert_eq!(
            judged,
            (None, "FMP does not confirm this as a split".to_string()),
            "{answer:?}"
        );
    }
    let refused = fmp_fixture("CMCSA");
    assert_eq!(
        judge("2026-01-05", 1067.0, 1000.0, Some(&refused)),
        (None, "looks like a spin-off (FMP: HTTP 402)".to_string())
    );
    assert_eq!(
        judge("2017-02-21", 2.0, 1.0, Some(&refused)),
        (Some(YAHOO), "by the ratio rule (FMP: HTTP 402)".to_string())
    );
    assert_eq!(judge("2014-06-09", 7.0, 1.0, None).0, Some(YAHOO), "not asked: the ratio rule");
}

/// **The ratio rule**: whole sides, reduced, each at most 100.
#[test]
fn the_ratio_rule_applies_a_small_whole_split_only() {
    for (n, d) in [(4.0, 1.0), (3.0, 2.0), (1.0, 10.0), (100.0, 1.0), (200.0, 2.0), (5.0, 4.0)] {
        assert!(ratio_rule(n, d), "{n}:{d}");
    }
    for (n, d) in [
        (1067.0, 1000.0),
        (10_000.0, 9356.0),
        (2002.0, 1000.0),
        (101.0, 1.0),
        (1.5, 1.0),
        (0.0, 1.0),
    ] {
        assert!(!ratio_rule(n, d), "{n}:{d}");
    }
}

/// **FMP's key never leaves in an error text**: a transport error that echoes the URL is redacted, the key rides the
/// query of the documented endpoint, and no key is an error naming the key's NAME only.
#[test]
fn an_fmp_error_never_carries_the_key() {
    use crate::tool_feeds::{
        FMP_API_KEY, ToolFeedHttp, ToolFeedHttpGet, ToolFeedKeyRead, ToolFeedKeyReader,
    };
    use vike_model::secret::Secret;
    struct Keys(Option<&'static str>);
    impl ToolFeedKeyReader for Keys {
        fn read(&self, name: &str) -> ToolFeedKeyRead {
            assert_eq!(name, FMP_API_KEY);
            self.0
                .and_then(Secret::non_blank)
                .map_or(ToolFeedKeyRead::Absent, ToolFeedKeyRead::Present)
        }
    }
    struct Echo(std::sync::Mutex<Vec<String>>);
    impl ToolFeedHttp for Echo {
        fn get(&self, request: &ToolFeedHttpGet) -> Result<String, String> {
            self.0.lock().expect("urls").push(request.url().to_string());
            assert!(request.carries_key(), "the keyed FMP request must follow no redirect");
            Err(format!("HTTP 402 at {}", request.url()))
        }
    }
    const SECRET: &str = "fmp-secret-0123456789";
    let http = Echo(std::sync::Mutex::new(Vec::new()));
    let why = fmp_ask(&http, &Keys(Some(SECRET)), "http://fmp.test", "CMCSA").expect_err("refused");
    // secret-absent: the planted FMP key, echoed in the failing URL, never reaches the refusal
    assert!(!why.contains(SECRET) && why.contains("HTTP 402"), "{why}");
    assert_eq!(
        *http.0.lock().expect("urls"),
        [format!("http://fmp.test/stable/splits?symbol=CMCSA&apikey={SECRET}")]
    );
    let none = fmp_ask(&http, &Keys(None), "http://fmp.test", "CMCSA").expect_err("no key");
    assert_eq!(none, format!("no {FMP_API_KEY} is stored"));
    assert_eq!(http.0.lock().expect("urls").len(), 1, "no key, no request");
}

/// **The store's Dukascopy share CFDs: Yahoo lists, FMP confirms, the ratio rule decides the rest, the operator's row
/// wins, each vendor asked once per ticker per refresh and FMP only where a split moves a stored bar.** AAPL 2020-08-31
/// ×4 and NFLX 2025-11-17 ×10 are FMP-confirmed (`yahoo+fmp`); CMCSA is refused by FMP, so its 2017 2:1 is applied by
/// the ratio rule and its `1067:1000` of 2026-01-05 is NOT (a spin-off); the operator's AAPL row of 2014-06-09 keeps its
/// ratio; BMW gets one line; NFLX held on a second venue is asked of nobody again. A later refresh whose FMP answer
/// no longer lists NFLX's date removes that row; a series none of whose splits moves a bar asks FMP nothing; a ticker
/// Yahoo does not answer leaves its rows as they were.
#[test]
fn the_stores_share_cfds_are_judged_and_the_operators_row_wins() {
    let settings = settings_store();
    let dir = settings.path();
    let operator = StockSplit::new("dukascopy", "AAPL.CFD", "2014-06-09", 6.0).expect("a split");
    vike_secrets::stock_split::write_stock_split(dir, &operator).expect("the operator's row");
    let kept = vike_catalog::spelled(vec![
        cfd("AAPL.US/USD", ""),
        cfd("CMCSA.US/USD", ""),
        cfd("NFLX.US/USD", ""),
        cfd("BMW.DE/EUR", "BMW"),
        Instrument::new("dukascopy", "EUR/USD", "EUR", "USD", Contract::Fx(ContractFx::default())),
    ]);
    let series = held(&[
        ("AAPL.CFD", FROM_2020),
        ("BMW.CFD", FROM_2025_08),
        ("CMCSA.CFD", FROM_2025_08),
        ("EURUSD", FROM_2025_08),
        ("GONE.CFD", FROM_2025_08),
        ("NFLX.CFD", FROM_2025_08),
    ]);
    let (yahoo_calls, fmp_calls) = (RefCell::new(Vec::new()), RefCell::new(Vec::new()));
    let mut yahoo = |t: &str| {
        yahoo_calls.borrow_mut().push(t.to_string());
        yahoo_fixture(t)
    };
    let mut fmp = |t: &str| {
        fmp_calls.borrow_mut().push(t.to_string());
        fmp_fixture(t)
    };
    let mut vendors = Vendors::new(&mut yahoo, &mut fmp);

    let report =
        refresh_venue(dir, "dukascopy", &kept, &series, &mut vendors).expect("shares held");
    assert_eq!((report.tickers, report.fmp_tickers, report.found), (3, 3, 10));
    assert_eq!((report.confirmed, report.written, report.removed), (8, 8, 0));
    assert_eq!(
        report.decisions,
        [
            "dukascopy AAPL.CFD (AAPL.US/USD) 2020-08-31 4:1: applied, FMP confirms the split",
            "dukascopy CMCSA.CFD (CMCSA.US/USD) 2026-01-05 1067:1000: NOT applied, looks like a spin-off (FMP: HTTP 402)",
            "dukascopy NFLX.CFD (NFLX.US/USD) 2025-11-17 10:1: applied, FMP confirms the split",
        ]
    );
    assert_eq!(
        report.unmapped,
        ["dukascopy BMW.CFD (BMW.DE/EUR): listed in DE: the row names a country, not an exchange"]
    );
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let alpaca = vike_catalog::spelled(vec![share("alpaca", "NFLX", Some("NASDAQ"))]);
    let other =
        refresh_venue(dir, "alpaca", &alpaca, &held(&[("NFLX", FROM_2025_08)]), &mut vendors);
    assert_eq!(other.map(|r| (r.tickers, r.fmp_tickers, r.written)), Some((1, 1, 3)));
    assert_eq!(
        *yahoo_calls.borrow(),
        ["AAPL", "CMCSA", "NFLX"],
        "Yahoo once per ticker per refresh"
    );
    assert_eq!(*fmp_calls.borrow(), ["AAPL", "CMCSA", "NFLX"], "FMP once per ticker per refresh");

    let rows = vike_secrets::stock_split::read_sourced_stock_splits(dir).expect("read");
    let has = |symbol: &str, date: &str, ratio: f64, source: &str| {
        rows.iter().any(|(s, from)| {
            s.symbol == symbol && s.date == date && s.ratio == ratio && from == source
        })
    };
    assert!(has("AAPL.CFD", "2020-08-31", 4.0, "yahoo+fmp"), "{rows:?}");
    assert!(has("NFLX.CFD", "2025-11-17", 10.0, "yahoo+fmp"), "{rows:?}");
    assert!(has("CMCSA.CFD", "2017-02-21", 2.0, "yahoo"), "{rows:?}");
    assert!(
        !rows.iter().any(|(s, _)| s.date == "2026-01-05"),
        "a spin-off is not applied: {rows:?}"
    );
    assert!(has("AAPL.CFD", "2014-06-09", 6.0, "operator"), "the operator's row wins: {rows:?}");
    assert!(!rows.iter().any(|(s, _)| s.symbol == "BMW.CFD"), "no split facts without a ticker");

    // A later refresh: FMP answers for NFLX without its 2025 date, so that row goes.
    let mut yahoo = |t: &str| yahoo_fixture(t);
    let mut fmp = |t: &str| match t {
        "NFLX" => {
            fmp_fixture("NFLX").map(|l| l.into_iter().filter(|(d, _)| d != "2025-11-17").collect())
        }
        other => fmp_fixture(other),
    };
    let mut later = Vendors::new(&mut yahoo, &mut fmp);
    let nflx = held(&[("NFLX.CFD", FROM_2025_08)]);
    let report = refresh_venue(dir, "dukascopy", &kept, &nflx, &mut later).expect("a share held");
    assert_eq!((report.written, report.removed), (0, 1));
    assert_eq!(
        report.decisions,
        [
            "dukascopy NFLX.CFD (NFLX.US/USD) 2025-11-17 10:1: NOT applied, FMP does not confirm this as a split"
        ]
    );

    // No split moves a bar of a series starting in 2026: FMP is not asked; the ratio rule applies AAPL's five.
    let mut yahoo = |t: &str| yahoo_fixture(t);
    let mut never = |t: &str| -> FmpAnswer { panic!("FMP asked about {t}") };
    let mut quiet = Vendors::new(&mut yahoo, &mut never);
    let alpaca = vike_catalog::spelled(vec![share("alpaca", "AAPL", Some("NASDAQ"))]);
    let report =
        refresh_venue(dir, "alpaca", &alpaca, &held(&[("AAPL", 1_767_225_600_000)]), &mut quiet)
            .expect("a share held");
    assert_eq!((report.fmp_tickers, report.confirmed, report.written), (0, 0, 5));
    assert!(report.decisions.is_empty(), "{:?}", report.decisions);

    let before = vike_secrets::stock_split::read_stock_splits(dir).expect("read").len();
    let mut gone = |_: &str| -> YahooAnswer {
        Err(YahooRefusal::Unlisted("venue fetch: HTTP 404".to_string()))
    };
    let mut unasked = |t: &str| -> FmpAnswer { panic!("FMP asked about {t}") };
    let mut down = Vendors::new(&mut gone, &mut unasked);
    let report = refresh_venue(dir, "dukascopy", &kept, &nflx, &mut down).expect("a share held");
    assert_eq!(
        report.failed,
        [
            "dukascopy NFLX.CFD (NFLX.US/USD), Yahoo NFLX: venue fetch: HTTP 404; its rows stand as they were"
        ]
    );
    let after = vike_secrets::stock_split::read_stock_splits(dir).expect("read").len();
    assert_eq!(after, before, "a ticker Yahoo did not answer removes nothing");
}

/// A venue that holds no share series reports nothing (its refresh answer carries no splits).
#[test]
fn a_venue_with_no_share_series_reports_nothing() {
    let settings = settings_store();
    let kept = vike_catalog::spelled(vec![Instrument::new(
        "binance",
        "BTCUSDT",
        "BTC",
        "USDT",
        Contract::CryptoSpot,
    )]);
    let mut never = |t: &str| -> YahooAnswer { panic!("asked {t}") };
    let mut unasked = |t: &str| -> FmpAnswer { panic!("asked {t}") };
    let mut vendors = Vendors::new(&mut never, &mut unasked);
    let report = refresh_venue(
        settings.path(),
        "binance",
        &kept,
        &held(&[("BTCUSDT", FROM_2025_08)]),
        &mut vendors,
    );
    assert_eq!(report, None);
}

/// The share-class retry follows the VARIANT, never the words (decision 0301): a failure that only SAYS `HTTP 404`
/// asks no other spelling, and Yahoo's own `404` (`YahooRefusal::Unlisted`) does, once.
#[test]
fn only_an_unlisted_ticker_is_asked_in_its_other_spelling() {
    for (refusal, asked) in [
        (YahooRefusal::Failed("a proxy said HTTP 404".to_string()), vec!["BRKB"]),
        (YahooRefusal::Unlisted("no such ticker".to_string()), vec!["BRKB", "BRK-B"]),
    ] {
        let calls = RefCell::new(Vec::new());
        let mut yahoo = |t: &str| -> YahooAnswer {
            calls.borrow_mut().push(t.to_string());
            Err(refusal.clone())
        };
        let mut never = |t: &str| -> FmpAnswer { panic!("FMP asked about {t}") };
        let mut vendors = Vendors::new(&mut yahoo, &mut never);
        let (ticker, answer) = vendors.yahoo_splits("BRKB");
        assert_eq!(ticker, "BRKB");
        assert!(answer.is_err());
        assert_eq!(*calls.borrow(), asked);
    }
}

/// **A share-class ticker Yahoo answers `404` is asked once more in Yahoo's spelling, and a rename row is asked under
/// its new ticker** (Yahoo as measured from the CI box on 2026-11-17: `BRKB` is a 404, `BRK-B` lists its 50:1 of
/// 2010-01-21, `FB` is an ETF with no split, `META` has none). `BRKB` is asked as `BRK-B` (FMP too) and the report says
/// so; `XYZQ` (no class letter) gets its 404 with no retry; with no rename row `FB` is asked as `FB`, today's
/// behaviour, and once the operator's row says `FB` is `META`, `META` is asked instead, and the report says so.
#[test]
fn a_share_class_form_is_asked_once_more_and_a_rename_row_is_followed() {
    let settings = settings_store();
    let dir = settings.path();
    let kept = vike_catalog::spelled(vec![
        cfd("BRKB.US/USD", ""),
        cfd("FB.US/USD", ""),
        cfd("XYZQ.US/USD", ""),
    ]);
    // 2009-01-02: before Berkshire's split, so it moves a stored bar and FMP is asked.
    let series = held(&[
        ("BRKB.CFD", 1_230_854_400_000),
        ("FB.CFD", FROM_2025_08),
        ("XYZQ.CFD", FROM_2025_08),
    ]);
    let not_found = |t: &str| {
        YahooRefusal::Unlisted(format!("venue fetch: eod HTTP 404 from https://yahoo.test/{t}"))
    };
    let (yahoo_calls, fmp_calls) = (RefCell::new(Vec::new()), RefCell::new(Vec::new()));
    let mut yahoo = |t: &str| -> YahooAnswer {
        yahoo_calls.borrow_mut().push(t.to_string());
        match t {
            "BRK-B" => Ok(vec![("2010-01-21".to_string(), 50.0, 1.0)]),
            "FB" | "META" => Ok(Vec::new()),
            other => Err(not_found(other)),
        }
    };
    let mut fmp = |t: &str| {
        fmp_calls.borrow_mut().push(t.to_string());
        fmp_fixture(t)
    };

    let mut vendors = Vendors::new(&mut yahoo, &mut fmp);
    let report = refresh_venue(dir, "dukascopy", &kept, &series, &mut vendors).expect("shares");
    assert_eq!(*yahoo_calls.borrow(), ["BRKB", "BRK-B", "FB", "XYZQ"], "one retry, BRKB's alone");
    assert_eq!(*fmp_calls.borrow(), ["BRK-B"], "FMP asks the ticker Yahoo answered");
    assert_eq!((report.tickers, report.found, report.written), (3, 1, 1));
    assert_eq!(
        report.asked_as,
        ["dukascopy BRKB.CFD (BRKB.US/USD): asked as BRK-B (Yahoo has no BRKB)"]
    );
    assert_eq!(
        report.decisions,
        [
            "dukascopy BRKB.CFD (BRKB.US/USD) 2010-01-21 50:1: applied, by the ratio rule (FMP: HTTP 402)"
        ]
    );
    assert_eq!(
        report.failed,
        [format!(
            "dukascopy XYZQ.CFD (XYZQ.US/USD), Yahoo XYZQ: {}; its rows stand as they were",
            not_found("XYZQ")
        )]
    );
    assert!(
        report.lines().contains(
            &"  ticker: dukascopy BRKB.CFD (BRKB.US/USD): asked as BRK-B (Yahoo has no BRKB)"
                .to_string()
        )
    );

    vike_secrets::stock_split::write_ticker_rename(dir, "dukascopy", "FB", "META").expect("row");
    vike_secrets::stock_split::write_ticker_rename(dir, "alpaca", "XYZQ", "XYZ").expect("row");
    yahoo_calls.borrow_mut().clear();
    let fb = held(&[("FB.CFD", FROM_2025_08), ("XYZQ.CFD", FROM_2025_08)]);
    let mut vendors = Vendors::new(&mut yahoo, &mut fmp);
    let report = refresh_venue(dir, "dukascopy", &kept, &fb, &mut vendors).expect("shares");
    assert_eq!(*yahoo_calls.borrow(), ["META", "XYZQ"], "another venue's row renames nothing here");
    assert_eq!(
        report.asked_as,
        ["dukascopy FB.CFD (FB.US/USD): asked as META (the venue's rename row for FB)"]
    );
    assert_eq!(
        vike_secrets::stock_split::read_ticker_renames(dir).expect("read"),
        [
            ("alpaca".to_string(), "XYZQ".to_string(), "XYZ".to_string()),
            ("dukascopy".to_string(), "FB".to_string(), "META".to_string())
        ]
    );
}
