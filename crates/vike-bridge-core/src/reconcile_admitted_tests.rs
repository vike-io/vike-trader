use super::*;
use crate::transport::ErrorKind;

// --- the list: the golden behaviour of vike-execution's former `AdmittedSymbols`, which it replaced -------

#[test]
fn a_new_symbol_is_kept_upper_cased_in_admission_order() {
    let a = ReconcileAdmittedSymbols::default();
    assert!(a.admit("ethusdt.p", "BTCUSDT.P"));
    assert!(a.admit("SOLUSDT.P", "BTCUSDT.P"));
    assert_eq!(a.symbols(), ["ETHUSDT.P", "SOLUSDT.P"]);
}

#[test]
fn the_mounted_symbol_an_empty_one_and_a_repeat_are_not_admitted() {
    let a = ReconcileAdmittedSymbols::default();
    assert!(!a.admit("btcusdt.p", "BTCUSDT.P"), "the mounted symbol is read anyway");
    assert!(!a.admit("ETHUSDT.P", "ethusdt.p"), "the mounted symbol, any case");
    assert!(!a.admit("", "BTCUSDT.P"));
    assert!(a.admit("ETHUSDT.P", "BTCUSDT.P"));
    assert!(!a.admit("ethusdt.p", "BTCUSDT.P"), "a repeat is not a second slot");
    assert_eq!(a.symbols(), ["ETHUSDT.P"]);
}

/// A case-kept list stores each spelling as given and compares exactly: two cases are two symbols, the
/// mounted one is refused only as spelled, and a forget needs the exact spelling. The bound holds alike.
#[test]
fn a_case_kept_list_keeps_and_compares_the_spelling_exactly() {
    let a = ReconcileAdmittedSymbols::case_kept();
    assert!(!a.admit("xyz:GOLD", "xyz:GOLD"), "the mounted symbol, as spelled");
    assert!(a.admit("xyz:gold", "xyz:GOLD"), "another case is another symbol");
    assert!(a.admit("HYPE/USDC", "xyz:GOLD"));
    assert!(!a.admit("HYPE/USDC", "xyz:GOLD"), "a repeat is not a second slot");
    assert!(!a.admit("", "xyz:GOLD"));
    assert_eq!(a.symbols(), ["xyz:gold", "HYPE/USDC"]);
    assert!(!a.forget("XYZ:GOLD"), "a forget needs the exact spelling");
    assert!(a.forget("xyz:gold"));
    assert_eq!(a.symbols(), ["HYPE/USDC"]);
    for i in 0..MAX_EXTRA_SYMBOLS {
        a.admit(&format!("d{i}:c"), "");
    }
    assert_eq!(a.symbols().len(), MAX_EXTRA_SYMBOLS);
}

/// "The venue no longer lists it" is said ONCE per symbol, at ERROR, with the venue, account and symbol; a symbol the
/// engine gave back and admitted again is said anew.
#[test]
fn a_symbol_no_longer_listed_is_said_once_until_given_back() {
    let a = ReconcileAdmittedSymbols::default();
    a.admit("ETH-USDT-SWAP", "BTC-USDT-SWAP");
    let (said, events) = vike_log::capture::captured(|| {
        let first = a.not_listed_logged("eth-usdt-swap", "okx", "main", "an instrument", "51001");
        let again = a.not_listed_logged("ETH-USDT-SWAP", "okx", "main", "an instrument", "51001");
        a.forget("ETH-USDT-SWAP");
        a.admit("ETH-USDT-SWAP", "BTC-USDT-SWAP");
        let anew = a.not_listed_logged("ETH-USDT-SWAP", "okx", "main", "an instrument", "51001");
        (first, again, anew)
    });
    assert_eq!(said, (true, false, true));
    let loud: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(loud.len(), 2, "{events:?}");
    let e = loud[0];
    let named = (e.field("venue"), e.field("account"), e.field("symbol"));
    assert_eq!(named, (Some("okx"), Some("main"), Some("ETH-USDT-SWAP")));
    assert!(
        e.message.contains("no longer lists an instrument the engine admitted (51001)"),
        "{}",
        e.message
    );
}

/// The engine's own cap: the client never reads a symbol the engine could not have admitted, so a
/// 17th extra symbol is refused and the first sixteen stay.
#[test]
fn the_list_is_bounded_by_the_engines_extra_symbol_cap() {
    let a = ReconcileAdmittedSymbols::default();
    for i in 0..MAX_EXTRA_SYMBOLS {
        assert!(a.admit(&format!("S{i}USDT.P"), "BTCUSDT.P"), "slot {i}");
    }
    assert!(!a.admit("OVERUSDT.P", "BTCUSDT.P"));
    assert_eq!(a.symbols().len(), MAX_EXTRA_SYMBOLS);
    assert!(!a.symbols().contains(&"OVERUSDT.P".to_string()));
}

/// A symbol the engine gave back leaves the list (in any case), keeps the others in their order, and
/// frees its place: after it a 17th symbol is taken that a full list refused.
#[test]
fn a_forgotten_symbol_frees_its_place() {
    let a = ReconcileAdmittedSymbols::default();
    for i in 0..MAX_EXTRA_SYMBOLS {
        assert!(a.admit(&format!("S{i}USDT.P"), "BTCUSDT.P"));
    }
    assert!(!a.admit("OVERUSDT.P", "BTCUSDT.P"), "full");
    assert!(a.forget("s3usdt.p"), "held, whatever the case");
    assert!(!a.forget("S3USDT.P"), "a second forget is a no-op");
    assert!(!a.forget("NEVERUSDT.P"), "so is a symbol never held");
    assert_eq!(a.symbols().len(), MAX_EXTRA_SYMBOLS - 1);
    assert_eq!(a.symbols()[3], "S4USDT.P", "the others keep their order");
    assert!(a.admit("OVERUSDT.P", "BTCUSDT.P"), "the freed place takes the next symbol");
}

/// The forwarding's one line each way: said only when the list changed, the symbol upper-cased as it
/// is stored, the venue a field and the venue's noun in the sentence (the sentence the alpaca, ctrader,
/// oanda, ig and IBKR copies spoke).
#[test]
fn the_logged_forwarding_says_a_change_once_in_the_venues_noun() {
    let a = ReconcileAdmittedSymbols::default();
    let ((), events) = vike_log::capture::captured(|| {
        a.admit_logged("cs.d.eurusd", "CS.D.GBPUSD", "ig", "an epic");
        a.admit_logged("CS.D.EURUSD", "CS.D.GBPUSD", "ig", "an epic"); // a repeat: silent
        a.admit_logged("cs.d.gbpusd", "CS.D.GBPUSD", "ig", "an epic"); // the mounted one: silent
        a.forget_logged("cs.d.eurusd", "ig", "an epic");
        a.forget_logged("CS.D.EURUSD", "ig", "an epic"); // never held now: silent
    });
    let said: Vec<(&str, Option<&str>, Option<&str>)> =
        events.iter().map(|e| (e.message.as_str(), e.field("venue"), e.field("symbol"))).collect();
    assert_eq!(
        said,
        [
            (
                "reconcile: the engine admitted an epic; every pass now reads it too",
                Some("ig"),
                Some("CS.D.EURUSD")
            ),
            (
                "reconcile: the engine gave an epic back; no pass reads it any more",
                Some("ig"),
                Some("CS.D.EURUSD")
            ),
        ]
    );
    assert!(events.iter().all(|e| e.level == tracing::Level::INFO), "{events:?}");
    assert!(a.symbols().is_empty());
}

// --- the read judgement: the binance family's and bybit's `admitted_rows`, each venue's facts ------

/// binance and aster (the binance family client's former `admitted_rows`): `-1121`, the taxonomy's
/// rate limit, the error's whole `Display`.
const BINANCE: ReconcileAdmittedReadSpec = ReconcileAdmittedReadSpec {
    not_listed: |e| e.code == -1121,
    rate_limited: |e| e.kind() == ErrorKind::RateLimited,
    text: |e| e.to_string(),
};

/// bybit (its recon client's former `admitted_rows`): `10001` naming the symbol, the taxonomy's rate
/// limit or the Cloudflare `403`, the message alone.
const BYBIT: ReconcileAdmittedReadSpec = ReconcileAdmittedReadSpec {
    not_listed: |e| e.code == 10001 && e.msg.to_ascii_lowercase().contains("symbol"),
    rate_limited: |e| e.kind() == ErrorKind::RateLimited || e.code == 403,
    text: |e| e.msg,
};

fn venue_err(code: i64, msg: &str) -> Result<Vec<u8>, ReconcileReadError> {
    Err(ReconcileReadError::Venue(VenueApiError { code, msg: msg.to_string() }))
}

fn the_warning(events: &[vike_log::capture::CapturedEvent]) -> &vike_log::capture::CapturedEvent {
    let mut warns = events.iter().filter(|e| e.level == tracing::Level::WARN);
    let warn = warns.next().unwrap_or_else(|| panic!("no warning: {events:?}"));
    assert!(warns.next().is_none(), "one warning: {events:?}");
    warn
}

#[test]
fn rows_and_an_unjudged_failure_pass_through_in_the_venues_own_text() {
    let pause = RateGatePause::default();
    assert_eq!(BINANCE.admitted_rows("binance", "ETHUSDT", &pause, Ok(vec![1, 2])), Ok(vec![1, 2]));
    // D4: one failure, two texts, each venue's as before
    assert_eq!(
        BINANCE.admitted_rows("binance", "ETHUSDT", &pause, venue_err(-2015, "Invalid API-key")),
        Err("venue error -2015: Invalid API-key".to_string())
    );
    assert_eq!(
        BYBIT.admitted_rows("bybit", "ETHUSDT", &pause, venue_err(10003, "API key is invalid.")),
        Err("API key is invalid.".to_string())
    );
    let parse = || Err::<Vec<u8>, _>(ReconcileReadError::Parse("bad body".to_string()));
    assert_eq!(
        BINANCE.admitted_rows("binance", "ETHUSDT", &pause, parse()),
        Err("bad body".into())
    );
    assert_eq!(BYBIT.admitted_rows("bybit", "ETHUSDT", &pause, parse()), Err("bad body".into()));
    assert_eq!(pause.left(), None, "nothing above is a rate limit");
}

#[test]
fn a_symbol_the_venue_no_longer_lists_holds_nothing_and_says_so() {
    let pause = RateGatePause::default();
    let (rows, events) = vike_log::capture::captured(|| {
        BINANCE.admitted_rows("aster", "ETHUSDT.P", &pause, venue_err(-1121, "Invalid symbol."))
    });
    assert_eq!(rows, Ok(vec![]));
    let warn = the_warning(&events);
    assert_eq!(
        warn.message,
        "reconcile: the venue does not list this admitted symbol (venue error -1121: Invalid \
         symbol.); reading it as holding nothing"
    );
    assert_eq!((warn.field("venue"), warn.field("symbol")), (Some("aster"), Some("ETHUSDT.P")));

    let msg = "params error: symbol invalid";
    assert_eq!(BYBIT.admitted_rows("bybit", "ETHUSDT", &pause, venue_err(10001, msg)), Ok(vec![]));
    // bybit's 10001 counts only when it names the symbol
    assert_eq!(
        BYBIT.admitted_rows("bybit", "ETHUSDT", &pause, venue_err(10001, "qty invalid")),
        Err("qty invalid".to_string())
    );
    assert_eq!(pause.left(), None);
}

#[test]
fn a_rate_limit_quiets_the_pass_and_fails_the_read_in_the_venues_text() {
    for (spec, venue, code, text) in [
        (&BINANCE, "binance", -1003, "venue error -1003: Too many requests."),
        (&BINANCE, "binance", 429, "venue error 429: Too many requests."),
        (&BYBIT, "bybit", 10006, "Too many requests."),
        (&BYBIT, "bybit", 403, "Too many requests."), // the Cloudflare block
    ] {
        let pause = RateGatePause::default();
        assert_eq!(not_backing_off(venue, &pause), Ok(()), "{venue} {code}: not quiet yet");
        let (read, events) = vike_log::capture::captured(|| {
            spec.admitted_rows(venue, "ETHUSDT", &pause, venue_err(code, "Too many requests."))
        });
        assert_eq!(read, Err(text.to_string()), "{venue} {code}");
        let warn = the_warning(&events);
        assert_eq!(
            warn.message,
            format!(
                "reconcile: rate limited reading an admitted symbol ({text}); the pass is silent \
                 for 60s"
            ),
            "{venue} {code}"
        );
        let left = pause.left().expect("the pass is quiet");
        assert!(left <= RATE_LIMIT_BACKOFF && left > RATE_LIMIT_BACKOFF / 2, "{left:?}");
        let quiet = not_backing_off(venue, &pause).expect_err("every fetch fails while quiet");
        assert!(
            quiet.starts_with(&format!("{venue} reconcile is backing off for "))
                && quiet.ends_with("s after a rate-limit answer on an admitted symbol"),
            "{quiet}"
        );
    }
    // binance's 403 is the key's, not a rate limit: no pause
    let pause = RateGatePause::default();
    assert_eq!(
        BINANCE.admitted_rows("binance", "ETHUSDT", &pause, venue_err(403, "Forbidden")),
        Err("venue error 403: Forbidden".to_string())
    );
    assert_eq!(pause.left(), None);
}

#[test]
fn a_mounted_read_error_takes_the_same_text() {
    let venue = || ReconcileReadError::from(VenueApiError { code: -1000, msg: "x".to_string() });
    assert_eq!(venue().into_text(BINANCE.text), "venue error -1000: x");
    assert_eq!(venue().into_text(BYBIT.text), "x");
    let parse = ReconcileReadError::<VenueApiError>::Parse("page chain never ended".to_string());
    assert_eq!(parse.into_text(BYBIT.text), "page chain never ended");
}
