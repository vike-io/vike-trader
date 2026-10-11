//! The four params refusals and the resolved echo, each against the reader it describes.

use super::*;
use crate::registry::echo::resolved_params;
use crate::registry::gates::unarmable_params;
use crate::registry::keys::{PARAM_KEYS, ParamKeys, mistyped_params, unknown_params};

#[test]
fn unknown_params_names_the_typo_and_passes_the_real_key() {
    let params: Value = toml::from_str("size = 2.0\nsizee = 3.0\nzzz = 1\n").unwrap();
    assert_eq!(unknown_params("buy_hold", &params), vec!["sizee".to_string(), "zzz".to_string()]);
    // A fully-recognised table is clean...
    let ok: Value = toml::from_str("size = 2.0\nsymbol = \"BTC\"\n").unwrap();
    assert!(unknown_params("buy_hold", &ok).is_empty());
    // ...a NotEnumerated row reports nothing (its consumer owes the stricter rule)...
    assert!(unknown_params("spread_maker", &params).is_empty());
    // ...and so does a name this registry does not resolve.
    assert!(unknown_params("nope", &params).is_empty());
}

/// A declared key carrying a value of the WRONG TYPE is reported, naming both types: `size = "2"`
/// is a key the reader knows and a value it cannot take, so the knob would mount at its default.
#[test]
fn mistyped_params_names_the_key_and_both_types() {
    let quoted: Value = toml::from_str("size = \"2\"\n").unwrap();
    let e = mistyped_params("grid", &quoted);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].key, "size");
    assert_eq!(e[0].got, "string");
    assert!(e[0].expected.contains("number"), "{:?}", e[0]);
    // ...and it really would have mounted the compiled default.
    assert_eq!(Grid::from_params(&quoted).size, Grid::default().size);

    let float_count: Value = toml::from_str("rungs = 4.0\n").unwrap();
    let e = mistyped_params("grid", &float_count);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].key.as_str(), e[0].got, e[0].expected), ("rungs", "float", "an integer"));
    assert_eq!(Grid::from_params(&float_count).rungs, Grid::default().rungs);

    let boolean: Value = toml::from_str("band = true\n").unwrap();
    let e = mistyped_params("grid", &boolean);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].key.as_str(), e[0].got), ("band", "boolean"));
    assert_eq!(Grid::from_params(&boolean).band, Grid::default().band);

    let e = mistyped_params("buy_hold", &toml::from_str("size = \"3\"\n").unwrap());
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].key.as_str(), e[0].got), ("size", "string"));
}

/// The abstentions, each for its own reason — the same three [`unknown_params`] has, plus the
/// one that matters most: a key at a type the reader DOES take is not an error, because a rule
/// refusing `qty = 1` where the reader happily takes `1.0` would break working profiles.
#[test]
fn mistyped_params_accepts_every_spelling_its_reader_accepts() {
    // The lenient numeric convention: BOTH spellings are legal for an `as_f64` key.
    for src in ["size = 2", "size = 2.0"] {
        assert!(
            mistyped_params("grid", &toml::from_str(src).unwrap()).is_empty(),
            "`{src}` must be accepted — `as_f64` takes either"
        );
    }
    // `read_side` genuinely takes EITHER, so both must pass.
    for src in ["side = \"short\"", "side = -1"] {
        assert!(
            mistyped_params("dca_accumulate", &toml::from_str(src).unwrap()).is_empty(),
            "`{src}` must be accepted — `read_side` takes either"
        );
    }
    // ...and a third type on that same key is still refused.
    assert_eq!(
        mistyped_params("dca_accumulate", &toml::from_str("side = 1.0").unwrap()).len(),
        1,
        "a float `side` reads as nothing in either arm of `read_side`"
    );
    // An UNKNOWN key is `unknown_params`' business, not this one's.
    assert!(mistyped_params("grid", &toml::from_str("sizee = \"2\"").unwrap()).is_empty());
    // A NotEnumerated row, an unknown name and a non-table all abstain.
    let bad: Value = toml::from_str("qty = \"2\"").unwrap();
    assert!(mistyped_params("spread_maker", &bad).is_empty());
    assert!(mistyped_params("nope", &bad).is_empty());
    assert!(mistyped_params("grid", &Value::Integer(3)).is_empty());
}

/// The FOURTH reader: every key spelled, typed and routed right, and the ladder they describe
/// EMPTY. The first two cases are the measured ones (`crates/vike-strategy/tests/param_gates.rs`'s
/// `DEAD` ledger); the near-misses below matter as much — a rule about a suspicious VALUE would be
/// the over-refusal this exists to avoid.
#[test]
fn unarmable_params_names_the_empty_ladder_and_its_resolution() {
    let at = |name: &str, src: &str| {
        unarmable_params(name, &toml::from_str::<Value>(src).expect("test TOML"))
    };
    // A FIXED anchor left at its compiled default price.
    let why = at("dca_accumulate", "anchor = \"fixed\"").expect("refused");
    assert!(why.contains("dca_accumulate"), "{why}");
    assert!(why.contains("NO rung"), "names what is wrong: {why}");
    assert!(why.contains("anchor=fixed"), "carries the resolution: {why}");
    assert!(why.contains("anchor_price=0"), "...including the knob left at its default: {why}");
    // A 0..1 grid at the compiled `step = 1.0`.
    let why = at("grid", "bounded01 = true").expect("refused");
    assert!(why.contains("bounded01=true") && why.contains("step=1"), "{why}");
    // ...and the degenerate ladder, which is the same defect: no rungs, no size, no spacing.
    for src in ["rungs = 0", "rungs = -5", "size = 0.0", "step = 0.0"] {
        assert!(at("grid", src).is_some(), "grid `{src}`");
        assert!(at("dca_accumulate", src).is_some(), "dca `{src}`");
    }
    // ⚠ The near-misses, which a rule about VALUES rather than ladders would wrongly refuse: a
    // SHORT ladder anchored at zero rests `step`, `2·step`, … and a bounded grid whose step
    // fits inside the 0..1 walls rests rungs.
    assert!(at("dca_accumulate", "anchor = \"fixed\"\nside = \"short\"\nstep = 0.05").is_none());
    assert!(at("grid", "bounded01 = true\nstep = 0.05").is_none());
    // The ordinary tables both names ship with are armable, or every row above is trivial.
    for name in ["grid", "dca_accumulate"] {
        assert!(at(name, "").is_none(), "{name}'s own defaults must load");
        assert!(at(name, "rungs = 4\nstep = 0.5\nsize = 2.0").is_none(), "{name}");
    }
    // Every other name abstains — the question is not asked of a strategy whose order flow is a
    // function of the market rather than of the table.
    for name in ["buy_hold", "momentum", "trailing_scalper", "spread_maker", "nope"] {
        assert!(unarmable_params(name, &empty()).is_none(), "{name} must abstain");
    }
}

/// The echo's key set is the TABLE's key set, in order — so a knob added to a reader (and
/// therefore to [`PARAM_KEYS`], which its own gate enforces) cannot be left out of the mount log.
#[test]
fn resolved_params_reports_exactly_the_declared_keys_in_order() {
    for (name, keys) in PARAM_KEYS {
        match keys {
            ParamKeys::Declared(declared) => {
                let got = resolved_params(name, &empty())
                    .unwrap_or_else(|| panic!("{name} declares keys but reports none"));
                let got_keys: Vec<&str> = got.iter().map(|(k, _)| *k).collect();
                let want: Vec<&str> = declared.iter().map(|(k, _)| *k).collect();
                assert_eq!(got_keys, want, "{name}'s echo and its PARAM_KEYS row disagree");
                assert!(
                    got.iter().all(|(_, v)| !v.is_empty()),
                    "{name} echoes an EMPTY value — say what it resolved to"
                );
            }
            ParamKeys::NotEnumerated(_) => assert!(
                resolved_params(name, &empty()).is_none(),
                "{name} enumerates nothing, so it can report nothing"
            ),
        }
    }
    assert!(resolved_params("nope", &empty()).is_none());
}

/// The echo reports the RESOLVED value, not the typed one — which is the whole point, because
/// the divergences a type check cannot catch are exactly the ones nobody can see otherwise.
#[test]
fn resolved_params_reports_the_coercion_not_the_input() {
    let get = |name: &str, src: &str, key: &str| -> String {
        let p: Value = toml::from_str(src).unwrap();
        resolved_params(name, &p)
            .unwrap()
            .into_iter()
            .find(|(k, _)| *k == key)
            .unwrap_or_else(|| panic!("{name} reports no {key}"))
            .1
    };
    // `read_rungs` CLAMPS a negative count to zero — a grid that rests nothing.
    assert_eq!(get("grid", "rungs = -5", "rungs"), "0");
    // `PairsZScore` rounds and floors its window at 2.
    assert_eq!(get("pairs_zscore", "period = 1", "period"), "2");
    assert_eq!(get("pairs_zscore", "period = 2.6", "period"), "3");
    // An unrecognised `anchor` silently means `first` — so the echo says `first`.
    assert_eq!(get("grid", "anchor = \"fixd\"\nanchor_price = 42.0", "anchor"), "first");
    // ...as an unrecognised `side` silently means LONG.
    assert_eq!(get("dca_accumulate", "side = \"shrot\"", "side"), "long");
    // A default the profile never mentions is still reported: `venue` is the literal "sim".
    assert_eq!(get("momentum", "qty = 2.0", "venue"), "sim");
    // A `venues` row whose value is not a string is dropped by the reader — the echo shows the
    // map that survived, not the table that was typed.
    assert_eq!(get("momentum", "[venues]\nBTC = 7", "venues"), "(none)");
    assert_eq!(get("momentum", "[venues]\nBTC = \"okx\"", "venues"), "BTC:okx");
    // An un-armed barrier leg says so rather than printing a fake zero.
    assert_eq!(get("momentum", "qty = 1.0", "tp"), "(unarmed)");
    assert_eq!(get("momentum", "tp = 5", "tp"), "5");
    // A REQUIRED symbol left unset is a mount that cannot route — the echo must not render it
    // as an empty string, which reads like a configured value.
    assert!(get("pairs_zscore", "entry_z = 2.0", "symbol_a").contains("unset"));
    assert_eq!(get("pairs_zscore", "symbol_a = \"BTC\"", "symbol_a"), "BTC");
    // An OPTIONAL symbol has THREE states the echo must keep apart: ABSENT is the legal default
    // (take the symbol off the feed); EMPTY is a STATED value that stops the strategy trading —
    // proven by `the_empty_symbol_this_echo_reports_really_does_stop_the_strategy` below.
    for name in ["buy_hold", "grid", "dca_accumulate", "funding_capture"] {
        assert_eq!(
            get(name, "", "symbol"),
            "(from the feed)",
            "{name}: an ABSENT optional symbol is the working default"
        );
        let empty_sym = get(name, "symbol = \"\"", "symbol");
        assert!(
            empty_sym.contains("cannot trade"),
            "{name}: an EMPTY symbol stops the strategy and the echo must say so, got \
                 {empty_sym:?}"
        );
        assert_ne!(
            empty_sym,
            get(name, "", "symbol"),
            "{name}: empty and absent are different mounts and must not render alike"
        );
        assert_eq!(get(name, "symbol = \"BTCUSDT\"", "symbol"), "BTCUSDT", "{name}");
    }
    // ⚠ The THIRD renderer — `funding_carry`'s inline arm — deliberately differs: there an empty
    // `symbol` is the REAL two-leg mode (`crates/vike-strategy/src/strategies/funding_carry.rs`'s
    // `evaluate` gates on `!self.symbol.is_empty()`), so "cannot trade" would be false about it.
    assert_eq!(get("funding_carry", "qty = 1.0", "symbol"), "(both legs)");
}

/// The claim `resolved_params`' `opt_sym` makes about an EMPTY symbol — that the strategy cannot
/// trade — proven for every strategy it renders, because an echo checked only against its own
/// wording drifts the moment a reader changes its guard. Delete `symbol.is_empty()` from
/// `BuyHold::buy`, either `crates/vike-strategy/src/strategies/grid_dca.rs` `drive`, or
/// `crates/vike-strategy/src/strategies/funding_capture.rs`'s `on_bar`, and this goes red.
///
/// The ABSENT case is the control, and it is load-bearing: without it "empty submits nothing"
/// would also pass for a bar that trades nothing at all.
#[test]
fn the_empty_symbol_this_echo_reports_really_does_stop_the_strategy() {
    // Carries BOTH a symbol and a funding rate, so one bar drives all four strategies: the grid
    // pair arm their ladders off `close`, and `funding_capture` acts only on a funding bar.
    fn a_bar() -> Bar {
        Bar {
            ts: 1,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 0.0,
            funding: Some(0.01),
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT".to_string()),
        }
    }
    fn submits(name: &str, src: &str) -> usize {
        let params: Value = toml::from_str(src).unwrap();
        let mut s = resolve(name, &params).unwrap_or_else(|e| panic!("{name} resolves: {e}"));
        let mut broker = RecordingBroker::default();
        s.on_bar(&mut broker, &a_bar());
        broker.submits()
    }
    for name in ["buy_hold", "grid", "dca_accumulate", "funding_capture"] {
        assert_eq!(
            submits(name, "symbol = \"\""),
            0,
            "{name} routed an order on an EMPTY symbol — the echo's \"cannot trade\" would be \
                 a lie"
        );
        assert!(
            submits(name, "") > 0,
            "{name} routed nothing even with the symbol taken off the bar, so the empty-symbol \
                 assertion above proves nothing about the symbol"
        );
    }
}
