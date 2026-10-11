use super::*;

fn a_bar() -> Bar {
    Bar {
        ts: 7,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn a_quote() -> QuoteTick {
    QuoteTick {
        ts: 1,
        local_ts: 2,
        bid: 1.0,
        ask: 1.1,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: "BTCUSDT".into(),
    }
}

fn a_trade() -> TradeTick {
    TradeTick {
        ts: 1,
        local_ts: 2,
        price: 1.0,
        size: 3.0,
        is_buyer_maker: true,
        symbol: "BTCUSDT".into(),
    }
}

fn a_book() -> BookUpdate {
    BookUpdate {
        ts: 1,
        local_ts: 2,
        seq: 9,
        kind: vike_model::BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(1.0, 2.0)],
        asks: vec![BookLevel::new(1.1, 3.0)],
        symbol: "BTCUSDT".into(),
    }
}

/// [`RowKind::columns`] is what an error message advertises and [`field_of`] is what actually
/// projects. A name in one and not the other is a silent `()` column or an unreachable field,
/// so the two are checked against each other rather than trusted to stay in step.
#[test]
fn every_advertised_column_actually_projects_and_nothing_else_does() {
    let rows: [(RowKind, Dynamic); 4] = [
        (RowKind::Bar, Dynamic::from(a_bar())),
        (RowKind::Quote, Dynamic::from(a_quote())),
        (RowKind::Trade, Dynamic::from(a_trade())),
        (RowKind::Book, Dynamic::from(a_book())),
    ];
    for (kind, row) in &rows {
        for field in kind.columns() {
            assert!(
                field_of(*kind, row, field).is_some(),
                "{}.{field} is advertised but does not project",
                kind.label()
            );
        }
        assert!(
            field_of(*kind, row, "definitely_not_a_field").is_none(),
            "{} projected a field it does not have",
            kind.label()
        );
        assert_eq!(row_kind(row), Some(*kind));
    }
    assert_eq!(row_kind(&Dynamic::from(1_i64)), None, "a plain int is not a row");
}

/// `bids`/`asks` are level LISTS, not scalars — reachable as properties, never as a column. If
/// they ever joined the table, `column(rows, "bids")` would build an array of arrays that
/// nothing downstream (the fit matrix above all) could consume.
#[test]
fn the_level_lists_are_properties_and_not_columns() {
    for f in ["bids", "asks"] {
        assert!(!RowKind::Book.columns().contains(&f));
    }
    assert_eq!(levels(&[BookLevel::new(1.0, 2.0)]).len(), 1);
}

/// An absent optional is NaN in a float column — never `0.0`, which reads as an observation.
#[test]
fn an_absent_optional_projects_as_nan_rather_than_zero() {
    let row = Dynamic::from(a_bar());
    for f in ["funding", "bid", "ask"] {
        let v = field_of(RowKind::Bar, &row, f).unwrap();
        assert!(v.as_float().unwrap().is_nan(), "{f} projected {v:?}");
    }
    assert!(field_of(RowKind::Bar, &row, "symbol").unwrap().is_unit());
}

#[test]
fn a_count_past_the_rhai_integer_saturates_rather_than_wrapping() {
    assert_eq!(saturating_int(9), 9);
    assert_eq!(saturating_int(u64::MAX), i64::MAX);
}

#[test]
fn column_refuses_an_unknown_field_and_a_mixed_array_by_naming_both() {
    let bars = vec![Dynamic::from(a_bar())];
    let e = column(bars.clone(), "clsoe".into()).unwrap_err().to_string();
    assert!(e.contains("clsoe") && e.contains("close"), "{e}");

    let mixed = vec![Dynamic::from(a_bar()), Dynamic::from(a_quote())];
    let e = column(mixed, "ts".into()).unwrap_err().to_string();
    assert!(e.contains("row 1") && e.contains("QuoteTick"), "{e}");

    let e = column(vec![Dynamic::from(1_i64)], "ts".into()).unwrap_err().to_string();
    assert!(e.contains("not a row"), "{e}");

    assert!(
        column(rhai::Array::new(), "anything".into()).unwrap().is_empty(),
        "empty in, empty out"
    );
    assert_eq!(column(bars, "close".into()).unwrap()[0].as_float().unwrap(), 1.5);
}

#[test]
fn an_unknown_fit_parameter_is_refused_by_name_rather_than_ignored() {
    let mut m = rhai::Map::new();
    m.insert("learning_Rate".into(), Dynamic::from(0.1_f64));
    let e = fit_params(&m).unwrap_err().to_string();
    assert!(e.contains("learning_Rate") && e.contains("learning_rate"), "{e}");
}

/// Both of rhai's numeric types reach every axis: `num_leaves: 31` and `num_leaves: 31.0` must
/// mean the same thing, which is the `param`-shaped trap this workspace has already shipped
/// once.
#[test]
fn a_fit_parameter_takes_an_integer_and_a_float_alike() {
    let mut m = rhai::Map::new();
    m.insert("num_leaves".into(), Dynamic::from(31_i64));
    m.insert("learning_rate".into(), Dynamic::from(0.25_f64));
    m.insert("seed".into(), Dynamic::from(7_i64));
    m.insert("capture_text".into(), Dynamic::from(true));
    let (p, seed, want) = fit_params(&m).unwrap();
    assert_eq!(p.num_leaves, 31);
    assert_eq!(p.learning_rate, 0.25);
    assert_eq!(p.max_depth, DEFAULT_POINT.max_depth, "an unset axis keeps the default point");
    assert_eq!(seed, 7);
    assert_eq!(want, Capture { importance: false, text: true });

    let mut m = rhai::Map::new();
    m.insert("num_leaves".into(), Dynamic::from(31.0_f64));
    assert_eq!(fit_params(&m).unwrap().0.num_leaves, 31);
}

#[test]
fn a_non_numeric_fit_parameter_is_refused_rather_than_coerced() {
    let mut m = rhai::Map::new();
    m.insert("learning_rate".into(), Dynamic::from("0.1"));
    let e = fit_params(&m).unwrap_err().to_string();
    assert!(e.contains("learning_rate") && e.contains("must be a number"), "{e}");

    let mut m = rhai::Map::new();
    m.insert("capture_text".into(), Dynamic::from(1_i64));
    assert!(fit_params(&m).unwrap_err().to_string().contains("true or false"));
}

#[test]
fn a_non_numeric_matrix_entry_is_refused_by_index() {
    let a = vec![Dynamic::from(1.0_f64), Dynamic::from("nope")];
    let e = floats(&a, "fit: x").unwrap_err().to_string();
    assert!(e.contains("fit: x[1]"), "{e}");
    let one = vec![Dynamic::from(2_i64)];
    assert_eq!(floats(&one, "fit: x").unwrap(), vec![2.0]);
}
