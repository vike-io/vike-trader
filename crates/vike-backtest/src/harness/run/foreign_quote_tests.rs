//! A backtest books ONE cash pool in ONE currency and holds no conversion-rate series (decision 0183,
//! item 13). A series quoted in the pool's own currency is booked exactly; a series whose symbol NAME
//! says another quote is refused in words, never run with its yen or pounds booked as dollars.
//!
//! The pool's currency is `engine.account_ccy` (absent = the dollar). The figures are the decision's:
//! USDJPY, 100,000 units, closed 0.10 higher, is 10,000 JPY, which is 10,000 / 150.10 = 66.62 USD
//! (a dollar ledger would book 10,000 "USD"); EURGBP, 100,000 units at 0.85, is 85,000 GBP, which is
//! 114,750 USD at 1.35 (a dollar ledger would book 85,000). The run books the first, in its own
//! currency, and never produces the second: that needs a rate series, so it stays refused.

use super::*;
use vike_marketdata::test_support::{flat_bar_zero_volume, quote};
use vike_store::DataFusionHist;

const VENUE: &str = "oanda";

/// The TOML of a buy-and-hold profile over `symbols` (the first is the strategy's), with
/// `engine_extra` lines appended under `[engine]` after `cash`.
fn toml_over(symbols: &[&str], kind: &str, cash: f64, engine_extra: &str) -> String {
    let listed = symbols.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ");
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = [{listed}]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = {cash:?}
{engine_extra}

[strategy]
name = "buy_hold"
[strategy.params]
size = 100000.0
symbol = "{first}"
"#,
        first = symbols[0]
    )
}

fn profile_over(symbols: &[&str], kind: &str, cash: f64, engine_extra: &str) -> BacktestProfile {
    BacktestProfile::from_toml_str(&toml_over(symbols, kind, cash, engine_extra)).unwrap()
}

/// The dollar profile every refusal test starts from (`engine.account_ccy` absent).
fn profile(symbol: &str, kind: &str) -> BacktestProfile {
    profile_over(&[symbol], kind, 1_000_000.0, "")
}

/// A profile on the ledger `ccy`, over one symbol.
fn profile_in(symbol: &str, kind: &str, cash: f64, ccy: &str) -> BacktestProfile {
    profile_over(&[symbol], kind, cash, &format!("account_ccy = \"{ccy}\""))
}

/// A store holding `symbol` as three daily bars and three quotes: the first at `px`, the second (where
/// `buy_hold`'s order fills, one step after it is placed) also at `px`, the last 0.10 above, so 100,000
/// units are bought at `px` and marked at `px + 0.10` on either lane. The quotes have no spread, so the
/// buy fills at `px` and the mark is `px + 0.10` exactly. The directory is returned beside it: it must
/// outlive the store.
fn store_with(symbol: &str, px: f64) -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, px),
        flat_bar_zero_volume(1000, px),
        flat_bar_zero_volume(2000, px + 0.10),
    ];
    store.append_bars(VENUE, symbol, "1d", &bars, None).unwrap();
    let quotes = vec![quote(0, px, px), quote(1000, px, px), quote(2000, px + 0.10, px + 0.10)];
    store.append_quotes(VENUE, symbol, &quotes, None).unwrap();
    (dir, store)
}

/// Run `profile` over `symbol` seeded at `px` and hand back the refusal sentence.
fn refusal_of(profile: &BacktestProfile, symbol: &str, px: f64) -> String {
    let (_dir, store) = store_with(symbol, px);
    match run_backtest(profile, store) {
        Err(HarnessError::Data(m)) => m,
        Err(other) => panic!("{symbol}: expected a data refusal, got {other:?}"),
        Ok(r) => panic!(
            "{symbol}: the run was NOT refused and booked its quote currency as the ledger's \
             (final equity {})",
            r.final_equity
        ),
    }
}

fn refusal(symbol: &str, px: f64, kind: &str) -> String {
    refusal_of(&profile(symbol, kind), symbol, px)
}

#[test]
fn a_yen_quoted_bar_series_is_refused_on_the_dollar_ledger_in_words() {
    let m = refusal("USDJPY", 150.0, "bar");
    assert!(m.contains("USDJPY (JPY)"), "names the symbol and its currency: {m}");
    assert!(m.contains("USD ledger"), "and the ledger it would be booked in: {m}");
    assert!(m.contains("absent, which means USD"), "and that the key is what to set: {m}");
    assert!(m.contains("engine.account_ccy = \"JPY\""), "and the way out that works: {m}");
}

/// ⚠ **The rate that is missing is named.** EURGBP in dollars needs GBP/USD; the run does not carry
/// it, so it is refused, and the sentence says which series is absent.
#[test]
fn a_pound_quoted_tick_series_is_refused_on_the_dollar_ledger_naming_the_missing_rate() {
    let m = refusal("EURGBP", 0.85, "tick");
    assert!(m.contains("EURGBP (GBP)"), "{m}");
    assert!(m.contains("GBP/USD conversion-rate series"), "the missing rate by name: {m}");
    assert!(m.contains("engine.account_ccy = \"GBP\""), "{m}");
}

/// A dollar-quoted series is untouched: the run proceeds and books exactly as before.
#[test]
fn a_dollar_quoted_series_runs_as_before() {
    for kind in ["bar", "tick"] {
        let (_dir, store) = store_with("EURUSD", 1.08);
        let result = run_backtest(&profile("EURUSD", kind), store)
            .unwrap_or_else(|e| panic!("EURUSD/{kind} must still run: {e:?}"));
        assert_ne!(result.final_equity, 1_000_000.0, "{kind}: buy_hold traded");
    }
}

/// ⚠ **THE GOLDEN.** USDJPY, 100,000 units bought at 150.00 and marked at 150.10, on a ledger that
/// IS yen: the pool moves by exactly 10,000 JPY, the figure the decision gives. Nothing converts.
#[test]
fn a_yen_ledger_books_a_usdjpy_bar_run_in_yen() {
    let cash = 100_000_000.0;
    let (_dir, store) = store_with("USDJPY", 150.0);
    let result = run_backtest(&profile_in("USDJPY", "bar", cash, "JPY"), store)
        .expect("a yen-quoted symbol on a yen ledger runs");
    let pnl = result.final_equity - cash;
    assert!((pnl - 10_000.0).abs() < 1e-6, "100,000 units * 0.10 = 10,000 JPY, booked {pnl}");
    // The figure the refusal exists to protect, taken from what the ENGINE booked rather than from a
    // literal: the same yen, read as dollars at the closing 150.10, are 66.62. A dollar ledger would
    // have printed `pnl` itself, a hundred and fifty times too many.
    let as_dollars = pnl / 150.10;
    assert!((as_dollars - 66.62).abs() < 0.005, "{as_dollars}");
}

/// The tick lane takes the same ledger from the same key and books the same 10,000 yen: the order
/// placed on the first quote fills at the second (150.00, no spread) and is marked at the third.
#[test]
fn a_yen_ledger_books_a_usdjpy_tick_run_in_yen() {
    let cash = 100_000_000.0;
    let (_dir, store) = store_with("USDJPY", 150.0);
    let result = run_backtest(&profile_in("USDJPY", "tick", cash, "JPY"), store)
        .expect("a yen-quoted symbol on a yen ledger runs on the tick lane too");
    let pnl = result.final_equity - cash;
    assert!((pnl - 10_000.0).abs() < 1e-6, "100,000 units * 0.10 = 10,000 JPY, booked {pnl}");
}

/// ⚠ **EURGBP on a pound ledger: 100,000 units at 0.85 is 85,000 GBP.** The shared-cash gate admits an
/// order exactly when the pool can fund its notional IN THE POOL'S UNIT, so 86,000 admits it and 84,000
/// refuses it: a dollar reading (114,750 at 1.35) would have admitted neither. The admitted run then
/// earns 10,000 GBP on a 0.10 move.
#[test]
fn a_pound_ledger_books_eurgbp_notional_and_pnl_in_pounds() {
    let (_dir, store) = store_with("EURGBP", 0.85);
    let funded =
        profile_over(&["EURGBP"], "bar", 86_000.0, "account_ccy = \"GBP\"\ncash_gate = true");
    let result = run_backtest(&funded, store.clone()).expect("a pound ledger takes EURGBP");
    assert!(
        (result.final_equity - 96_000.0).abs() < 1e-6,
        "86,000 cash funds the 85,000 GBP notional and earns 10,000 GBP: {}",
        result.final_equity
    );

    let short =
        profile_over(&["EURGBP"], "bar", 84_000.0, "account_ccy = \"GBP\"\ncash_gate = true");
    let result = run_backtest(&short, store).expect("a pound ledger takes EURGBP");
    assert_eq!(result.final_equity, 84_000.0, "84,000 GBP cannot fund an 85,000 GBP notional");
}

/// The ledger is typed by a person: any case, surrounding space, and the same run.
#[test]
fn a_ledger_currency_is_read_in_any_case() {
    let cash = 100_000_000.0;
    for typed in ["jpy", " Jpy "] {
        let (_dir, store) = store_with("USDJPY", 150.0);
        let result = run_backtest(&profile_in("USDJPY", "bar", cash, typed), store)
            .unwrap_or_else(|e| panic!("{typed:?}: {e:?}"));
        assert!((result.final_equity - cash - 10_000.0).abs() < 1e-6, "{typed:?}");
    }
}

/// ⚠ **A yen ledger takes only yen.** A dollar-quoted symbol on it is refused (it would be booked as
/// yen), and so is a name that states no fiat quote (`BTCUSDT`): "does not say" is dollars only on the
/// dollar ledger.
#[test]
fn a_yen_ledger_refuses_a_symbol_that_is_not_quoted_in_yen() {
    let m = refusal_of(&profile_in("EURUSD", "bar", 100_000_000.0, "JPY"), "EURUSD", 1.08);
    assert!(m.contains("EURUSD (USD)"), "{m}");
    assert!(m.contains("JPY ledger"), "{m}");
    assert!(m.contains("USD/JPY conversion-rate series"), "{m}");

    let m = refusal_of(&profile_in("BTCUSDT", "bar", 100_000_000.0, "JPY"), "BTCUSDT", 60_000.0);
    assert!(m.contains("BTCUSDT (no fiat quote)"), "{m}");
    assert!(m.contains("JPY ledger"), "{m}");
}

/// ⚠ **One run, one currency.** Symbols quoted in different currencies cannot share a pool whichever
/// ledger is named: that needs a rate series per currency, and the sentence says to split the run.
#[test]
fn symbols_quoted_in_different_currencies_stay_refused_whichever_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    for ccy in [None, Some("JPY"), Some("GBP")] {
        let extra = ccy.map(|c| format!("account_ccy = \"{c}\"")).unwrap_or_default();
        let p = profile_over(&["USDJPY", "EURGBP"], "bar", 1_000_000.0, &extra);
        let m = match run_backtest(&p, store.clone()) {
            Err(HarnessError::Data(m)) => m,
            other => panic!("{ccy:?}: expected a data refusal, got {other:?}"),
        };
        assert!(m.contains("different currencies"), "{ccy:?}: {m}");
        assert!(m.contains("Split them into separate runs"), "{ccy:?}: {m}");
    }
}

/// Crosses that share a quote share a ledger: the guard admits them together on that currency and on
/// no other, and an IG epic and a venue-separated spelling read the same.
#[test]
fn symbols_that_share_a_quote_share_a_ledger() {
    let yen = ["USDJPY", "EUR_JPY", "CS.D.GBPJPY.CFD.IP"];
    assert!(refuse_foreign_quoted_symbols(yen, Some("JPY")).is_ok());
    assert!(refuse_foreign_quoted_symbols(yen, Some("jpy")).is_ok());
    assert!(refuse_foreign_quoted_symbols(yen, None).is_err());
    assert!(refuse_foreign_quoted_symbols(yen, Some("GBP")).is_err());
    assert!(refuse_foreign_quoted_symbols(["EURUSD", "BTCUSDT", "AAPL"], None).is_ok());
}

/// A currency the build does not know is a VALIDATION error at profile load, naming the key, for a
/// typo, a coin, a stablecoin and the empty string alike; and the guard says the same if a caller
/// bypasses the load.
#[test]
fn an_unknown_ledger_currency_is_refused_at_load() {
    for bad in ["JPYY", "BTC", "USDT", "", "  "] {
        let toml = toml_over(&["USDJPY"], "bar", 1000.0, &format!("account_ccy = {bad:?}"));
        match BacktestProfile::from_toml_str(&toml) {
            Err(HarnessError::Validation(m)) => {
                assert!(m.contains("engine.account_ccy"), "{bad:?}: {m}");
            }
            other => panic!("{bad:?}: expected a validation refusal, got {other:?}"),
        }
        assert!(matches!(
            refuse_foreign_quoted_symbols(["USDJPY"], Some(bad)),
            Err(HarnessError::Validation(_))
        ));
    }
}

/// The walk-forward door hands the harness bars it already holds: it is refused the same way on the
/// dollar ledger, and admitted on the yen one.
#[test]
fn bars_a_caller_already_holds_follow_the_ledger_too() {
    let (_dir, store) = store_with("USDJPY", 150.0);
    let store: Arc<dyn HistStore + Send + Sync> = store;
    let bars = || {
        vec![(
            "USDJPY".to_string(),
            vec![
                flat_bar_zero_volume(0, 150.0),
                flat_bar_zero_volume(1000, 150.0),
                flat_bar_zero_volume(2000, 150.10),
            ],
        )]
    };
    let err = run_backtest_over_bars(&profile("USDJPY", "bar"), &store, bars())
        .expect_err("a yen-quoted window is refused on the dollar ledger");
    assert!(matches!(&err, HarnessError::Data(m) if m.contains("USDJPY (JPY)")), "{err:?}");

    let cash = 100_000_000.0;
    let ok = run_backtest_over_bars(&profile_in("USDJPY", "bar", cash, "JPY"), &store, bars())
        .expect("a yen ledger takes the same window");
    assert!((ok.final_equity - cash - 10_000.0).abs() < 1e-6, "{}", ok.final_equity);
}

/// The ledger label rides the realism stamp so a report can print it, and the dollar's word is the
/// catalog's (`vike-analytics` sits below `vike-catalog` and spells it itself).
#[test]
fn the_stamp_carries_the_resolved_ledger_and_the_dollar_is_one_word() {
    use crate::harness::report::realism_stamp;
    use vike_analytics::realism::{ACCOUNT_CCY_KEY, DOLLAR_LEDGER};

    assert_eq!(DOLLAR_LEDGER, vike_catalog::LEDGER_CCY);
    let dollar = realism_stamp(&profile("EURUSD", "bar"));
    assert_eq!(dollar.get(ACCOUNT_CCY_KEY), Some("USD"));
    assert_eq!(dollar.off_dollar_ledger(), None);
    let yen = realism_stamp(&profile_in("USDJPY", "bar", 1_000_000.0, " jpy "));
    assert_eq!(yen.get(ACCOUNT_CCY_KEY), Some("JPY"), "resolved, not the text typed");
    assert_eq!(yen.off_dollar_ledger(), Some("JPY"));
}

/// ⚠ **An ABSENT key and an explicit `"USD"` are the SAME run**, bit for bit, on both lanes — the
/// byte-identity claim for every dollar profile, asserted over the whole `BacktestResult` (its
/// `Debug` prints every `f64` in its shortest round-tripping form) rather than over one number that
/// "moved off 1,000,000".
#[test]
fn an_absent_ledger_and_an_explicit_dollar_ledger_give_the_identical_result() {
    for kind in ["bar", "tick"] {
        let run = |extra: &str| {
            let (_dir, store) = store_with("EURUSD", 1.08);
            run_backtest(&profile_over(&["EURUSD"], kind, 1_000_000.0, extra), store)
                .unwrap_or_else(|e| panic!("EURUSD/{kind}: {e:?}"))
        };
        let (absent, stated, lower) =
            (run(""), run("account_ccy = \"USD\""), run("account_ccy = \" usd \""));
        assert_ne!(absent.final_equity, 1_000_000.0, "{kind}: buy_hold traded");
        assert_eq!(format!("{absent:?}"), format!("{stated:?}"), "{kind}: absent == \"USD\"");
        assert_eq!(format!("{absent:?}"), format!("{lower:?}"), "{kind}: absent == \" usd \"");
    }
}

/// The ledger a refusal SUGGESTS must itself be accepted: a way out that is also refused sends the
/// user round in a circle. `[EURGBP, BTCUSDT]` is the case that used to do it (it suggested a GBP
/// ledger, which then refused `BTCUSDT` as "no fiat quote"); every refused set below either names a
/// ledger the guard then ACCEPTS or names none.
#[test]
fn a_suggested_ledger_is_one_the_guard_accepts() {
    fn suggested(m: &str) -> Option<String> {
        let at = m.find("engine.account_ccy = \"")? + "engine.account_ccy = \"".len();
        m[at..].split('"').next().map(str::to_string)
    }
    let cases: [(&[&str], Option<&str>); 7] = [
        (&["USDJPY"], None),
        (&["EURGBP", "BTCUSDT"], None),
        (&["USDJPY", "EURGBP"], None),
        (&["USDJPY", "EURJPY"], None),
        (&["BTCUSDT"], Some("JPY")),
        (&["EURUSD"], Some("JPY")),
        (&["BTCEUR", "ETHEUR"], None),
    ];
    for (symbols, ledger) in cases {
        let Err(HarnessError::Data(m)) =
            refuse_foreign_quoted_symbols(symbols.iter().copied(), ledger)
        else {
            panic!("{symbols:?} on {ledger:?} must be refused");
        };
        if let Some(next) = suggested(&m) {
            assert!(
                refuse_foreign_quoted_symbols(symbols.iter().copied(), Some(next.as_str())).is_ok(),
                "{symbols:?} on {ledger:?} suggests {next:?}, which is refused too: {m}"
            );
        }
    }
    // The exact case: no GBP suggestion for a set a GBP ledger would refuse.
    let Err(HarnessError::Data(m)) = refuse_foreign_quoted_symbols(["EURGBP", "BTCUSDT"], None)
    else {
        panic!("refused")
    };
    assert!(!m.contains("engine.account_ccy = \"GBP\""), "{m}");
    assert!(m.contains("No single ledger currency"), "{m}");
}

/// A door with NO profile (the Studio's picker, the named run) is not sent to a key it cannot set:
/// the sentence says the door has no ledger field and names the profile run that works, and it never
/// tells the user to "leave engine.account_ccy out".
#[test]
fn a_door_with_no_profile_names_the_profile_run_not_a_key_it_cannot_set() {
    let Err(HarnessError::Data(m)) = refuse_foreign_quoted_picks(["USDJPY"]) else {
        panic!("a yen-quoted pick is refused")
    };
    assert!(m.contains("USDJPY (JPY)") && m.contains("no ledger field"), "{m}");
    assert!(m.contains("--profile") && m.contains("--set engine.account_ccy=JPY"), "{m}");
    assert!(!m.contains("engine.account_ccy is absent"), "{m}");
    assert!(refuse_foreign_quoted_picks(["EURUSD", "BTCUSDT"]).is_ok(), "a dollar pick runs");
    let Err(HarnessError::Data(mixed)) = refuse_foreign_quoted_picks(["USDJPY", "EURGBP"]) else {
        panic!("a mixed pick is refused")
    };
    assert!(mixed.contains("different currencies") && mixed.contains("no ledger field"), "{mixed}");
}

/// ⚠ **A built-in venue fee table is in DOLLARS.** The interactive-brokers row (`$0.005` a share,
/// `$1` minimum) would be read as euros on a euro ledger, silently; it is refused there, in words that
/// point at the explicit schedule written in the ledger's own currency, which is accepted. On the
/// dollar ledger, and for every unit-free venue row (a rate or free), nothing changes.
#[test]
fn a_dollar_fee_table_is_refused_on_another_ledger() {
    fn params_of(extra: &str) -> Result<(), HarnessError> {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn HistStore + Send + Sync> =
            Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let p = profile_over(&["AAPL.SMART.EUR"], "bar", 100_000.0, extra);
        bar_engine_params(&p, &store).map(|_| ())
    }
    let venue_table = |ccy: &str, venue: &str, symbol: &str| {
        format!(
            "account_ccy = \"{ccy}\"\n[engine.fee]\nkind = \"venue\"\nvenue = \"{venue}\"\n\
             symbol = \"{symbol}\"\n"
        )
    };
    let Err(HarnessError::Data(m)) = params_of(&venue_table("EUR", "ibkr", "AAPL")) else {
        panic!("the dollar table must be refused on a euro ledger")
    };
    assert!(m.contains("DOLLAR") && m.contains("EUR") && m.contains("per_share_with_floor"), "{m}");
    // The explicit schedule, written in euros, is the way out and is accepted.
    assert!(
        params_of(
            "account_ccy = \"EUR\"\n[engine.fee]\nkind = \"per_share_with_floor\"\n\
             per_share = 0.004\nmin = 1.0\n"
        )
        .is_ok()
    );
    // The same venue table on the dollar ledger is untouched, and so is a unit-free venue row.
    assert!(params_of(&venue_table("USD", "ibkr", "AAPL")).is_ok());
    assert!(params_of(&venue_table("EUR", "oanda", "EUR_USD")).is_ok());
}
