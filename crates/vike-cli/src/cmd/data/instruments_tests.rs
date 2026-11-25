use vike_datahub_client::catalog::{
    OperatorInstrumentOutcome, OperatorInstrumentRow, OperatorInstrumentsReply,
};
use vike_model::{Contract, ContractFx, Instrument};

use super::*;

/// `--asset-class` is the one spelling of an asset class; the retired `--class` is refused by name with it.
#[test]
fn the_retired_class_flag_names_asset_class() {
    let line = ["add", "--venue", "ig", "X", "--tick", "1", "--quote", "USD", "--class", "fx"];
    let InstrumentsVerb::Add(flags) = parse(&line).expect("clap reads it") else { panic!() };
    assert_eq!(
        stated(&flags),
        Err(RetiredDataSpelling::Class { verb: "instruments add" }.rendered())
    );
}

/// The runbook's lines parse: a looked-up batch, a stated row with all four facts, a remove, a read.
#[test]
fn the_runbook_lines_parse() {
    let add = parse(&["add", "--venue", "ig", "EURUSD", "GBPUSD", "USDJPY"]).expect("add");
    let InstrumentsVerb::Add(flags) = &add else { panic!("{add:?}") };
    assert_eq!(flags.change.symbols, ["EURUSD", "GBPUSD", "USDJPY"]);
    assert_eq!(judge(&add), Ok(()));
    assert_eq!(stated(flags), Ok(None), "no facts: the venue is asked");

    let line = [
        "add",
        "--venue",
        "dukascopy",
        "AAPL.US/USD",
        "--tick",
        "0.001",
        "--quote",
        "USD",
        "--asset-class",
        "share",
        "--point-scale",
        "1000",
    ];
    let InstrumentsVerb::Add(flags) = parse(&line).expect("stated") else { panic!() };
    let facts = stated(&flags).expect("all four").expect("stated");
    assert_eq!((facts.tick, facts.point_scale, facts.class.as_str()), (0.001, 1000.0, "share"));

    assert!(matches!(parse(&["rm", "--venue", "ig", "EURUSD"]), Ok(InstrumentsVerb::Rm(_))));
    assert!(matches!(parse(&["ls"]), Ok(InstrumentsVerb::Ls(_))));
}

/// **The runbook's IBKR lines parse** (decision 0290's O-10): the exact `AAPL` row added before accounts 12, 17 and
/// 22 are renamed to `AAPL` (`docs/ops/operator-instruments.md`), a pair and a future, each its wire name with its
/// class, base and quote; a `--base` or `--expiry` without the four facts is refused locally.
#[test]
fn the_runbook_ibkr_lines_parse() {
    let aapl = "add --venue ibkr AAPL.SMART.USD --asset-class equity --base AAPL --quote USD --tick 0.01 --point-scale 100";
    let pair = "add --venue ibkr USD.JPY.IDEALPRO --asset-class fx --base USD --quote JPY --tick 0.005 --point-scale 200";
    let future = "add --venue ibkr ES.CME.USD.FUT.20261218 --asset-class future --base ES --expiry 2026-12-18 --quote USD \
                  --tick 0.25 --point-scale 4";
    for (line, wire, class, base, expiry) in [
        (aapl, "AAPL.SMART.USD", "equity", "AAPL", None),
        (pair, "USD.JPY.IDEALPRO", "fx", "USD", None),
        (future, "ES.CME.USD.FUT.20261218", "future", "ES", Some("2026-12-18")),
    ] {
        let argv: Vec<&str> = line.split_whitespace().collect();
        let InstrumentsVerb::Add(flags) = parse(&argv).expect(line) else { panic!("{line}") };
        assert_eq!(flags.change.venue, "ibkr");
        assert_eq!(flags.change.symbols, [wire]);
        let facts = stated(&flags).expect(line).expect("stated");
        assert_eq!(
            (facts.class.as_str(), facts.base.as_deref(), facts.expiry.as_deref()),
            (class, Some(base), expiry),
            "{line}"
        );
    }
    let half =
        parse(&["add", "--venue", "ibkr", "AAPL.SMART.USD", "--base", "AAPL"]).expect("parses");
    assert!(judge(&half).is_err_and(|e| e.contains("stated together")), "a base alone");
}

/// Some facts but not all is refused before anything is sent; a symbol and a venue are required.
#[test]
fn missing_facts_and_missing_words_are_refused_locally() {
    let half =
        parse(&["add", "--venue", "dukascopy", "AAPL.US/USD", "--tick", "0.001"]).expect("parses");
    assert!(judge(&half).is_err_and(|e| e.contains("stated together")), "half the facts");
    assert!(parse(&["add", "--venue", "ig"]).is_err(), "no symbol");
    assert!(parse(&["add", "EURUSD"]).is_err(), "no venue");
    assert!(parse(&["rm", "--venue", "ig"]).is_err(), "no symbol to remove");
}

/// What a reply prints: every outcome by name, a refusal loud, and the rows of the venue asked.
#[test]
fn a_reply_prints_each_outcome_and_the_rows() {
    let fx = Contract::Fx(ContractFx {
        pip_size: None,
        lot_size: None,
        swap_long: None,
        swap_short: None,
    });
    let row = |venue: &str, raw: &str| OperatorInstrumentRow {
        asked: "EURUSD".into(),
        added: "2026-11-13".into(),
        instrument: Instrument::new(venue, raw, "EUR", "USD", fx.clone())
            .with_description("EUR/USD Mini"),
    };
    let reply = OperatorInstrumentsReply {
        outcomes: vec![
            OperatorInstrumentOutcome {
                symbol: "EURUSD".into(),
                done: Ok("ig: EURUSD is CS.D.EURUSD.MINI.IP (EUR/USD Mini)".into()),
            },
            OperatorInstrumentOutcome { symbol: "XAUXAG".into(), done: Err("no such pair".into()) },
        ],
        rows: vec![row("ig", "CS.D.EURUSD.MINI.IP"), row("dukascopy", "EURUSD")],
        list_error: None,
    };
    let lines = reply_lines(&reply, Some("ig"));
    assert!(lines[0].starts_with("ok") && lines[0].contains("CS.D.EURUSD.MINI.IP"), "{lines:?}");
    assert_eq!(lines[1], "REFUSED XAUXAG: no such pair");
    assert_eq!(lines[2], "1 instrument(s) added by hand:", "only the venue asked");
    assert!(lines[3].contains("CS.D.EURUSD.MINI.IP") && lines[3].contains("Fx"), "{lines:?}");
    assert_eq!(reply_lines(&reply, None).len(), 5, "every venue's rows without --venue");
}
