use super::*;

#[test]
fn unrealized_at_applies_multiplier_and_side() {
    // multiplier 10 for "BTC", default 1
    let mut mults = IndexMap::new();
    mults.insert("BTC".to_string(), 10.0);
    let acc = Account::new(1.0, "binance", Some(mults), BalanceMode::Delta);
    // long 2 @ 100, priced at 105 -> (105-100)*2*10 = 100
    assert_eq!(acc.unrealized_at("BTC", PositionSide::Long, 2.0, 100.0, 105.0), 100.0);
    // short 2 @ 100 (size stored signed, mirroring unrealized_pnl): short valued at 105 ->
    // loss -> (105-100) * (-2) * 10 = -100
    assert_eq!(acc.unrealized_at("BTC", PositionSide::Short, -2.0, 100.0, 105.0), -100.0);
    // default multiplier (unknown symbol) = 1
    assert_eq!(acc.unrealized_at("ETH", PositionSide::Long, 1.0, 10.0, 12.0), 2.0);
}

#[test]
fn unrealized_at_matches_unrealized_pnl_when_px_equals_mark() {
    let mut mults = IndexMap::new();
    mults.insert("BTC".to_string(), 10.0);
    let mut acc = Account::new(1.0, "binance", Some(mults), BalanceMode::Delta);
    let key: PositionKey = ("binance".into(), "BTC".into(), PositionSide::Long);
    acc.positions.insert(key, PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() });
    acc.set_mark_from("binance", "BTC", 105.0, MarkSource::VenueMark, 0);

    let via_marks = acc.unrealized_pnl("binance", "BTC", "LONG");
    let pos = acc.positions[&key];
    let via_supplied_px = acc.unrealized_at("BTC", PositionSide::Long, pos.size, pos.avg_px, 105.0);

    assert_eq!(via_supplied_px, via_marks);
}

// --- margin_in_use (the ONE shared margin fold) ------------------------------------------

/// Two open positions, one with a bespoke multiplier and one flat, plus one unmarked.
fn margin_account() -> Account {
    let mut mults = IndexMap::new();
    mults.insert("ETHUSDT".to_string(), 10.0); // multiplier ≠ 1
    let mut a = Account::new(1.0, "binance", Some(mults), BalanceMode::Delta);
    // BTC: long 2 @ ..., marked 100, mult 1
    a.positions.insert(
        ("binance".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry { size: 2.0, avg_px: 90.0, ..Default::default() },
    );
    a.set_mark_from("binance", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
    // ETH: short 3 @ ..., marked 50, mult 10
    a.positions.insert(
        ("binance".into(), "ETHUSDT".into(), "BOTH".into()),
        PositionEntry { size: -3.0, avg_px: 55.0, ..Default::default() },
    );
    a.set_mark_from("binance", "ETHUSDT", 50.0, MarkSource::VenueMark, 0);
    a
}

#[test]
fn margin_in_use_folds_abs_size_mark_mult_rate_in_order() {
    let a = margin_account();
    // flat rate 0.1 over both: BTC 2·100·1·0.1 = 20 ; ETH 3·50·10·0.1 = 150 → 170
    let used = a.margin_in_use(|_s| Some(0.1));
    assert_eq!(used.to_bits(), 170.0_f64.to_bits());
    // multiplier IS folded for the mult=10 position (else ETH would be 15, total 35).
    assert_ne!(used, 35.0);
}

#[test]
fn margin_in_use_none_rate_excludes_that_position() {
    let a = margin_account();
    // rate only for BTC; ETH declined → excluded. 2·100·1·0.1 = 20.
    let used = a.margin_in_use(|s| if s == "BTCUSDT" { Some(0.1) } else { None });
    assert_eq!(used.to_bits(), 20.0_f64.to_bits());
}

#[test]
fn margin_in_use_skips_flat_and_unmarked() {
    let mut a = margin_account();
    // add a flat position (counts 0) and an unmarked one (unpriceable → 0)
    a.positions.insert(
        ("binance".into(), "FLAT".into(), "BOTH".into()),
        PositionEntry { size: 0.0, avg_px: 10.0, ..Default::default() },
    );
    a.set_mark_from("binance", "FLAT", 10.0, MarkSource::VenueMark, 0);
    a.positions.insert(
        ("binance".into(), "NOMARK".into(), "BOTH".into()),
        PositionEntry { size: 5.0, avg_px: 10.0, ..Default::default() },
    );
    // still 170 — the flat and the unmarked contribute nothing.
    let used = a.margin_in_use(|_s| Some(0.1));
    assert_eq!(used.to_bits(), 170.0_f64.to_bits());
}

#[test]
fn margin_in_use_gate_and_snapshot_policies_agree_on_no_override_position() {
    // Reproduce the closed divergence at the Account level: per-symbol margin armed for BTC
    // only (im_by_symbol={BTCUSDT:0.2}, no global default). ETH has NO override.
    let a = margin_account();
    let mut im_by_symbol: IndexMap<String, f64> = IndexMap::new();
    im_by_symbol.insert("BTCUSDT".to_string(), 0.2);
    // im_for(sym) = im_by_symbol.get(sym).or(None)  → BTC Some(0.2), ETH None.
    let im_for = |s: &str| im_by_symbol.get(s).copied();

    // GATE policy (order on BTC, order im 0.2): no-override ETH falls back to the order im.
    let order_im = 0.2_f64;
    let gate = a.margin_in_use(|s| Some(im_for(s).unwrap_or(order_im)));

    // OLD snapshot policy (the BUG): im_for with no fallback → ETH skipped, understated.
    let old_snapshot = a.margin_in_use(im_for);

    // NEW snapshot policy: fall back to the max armed rate (0.2 here) → ETH now counted.
    let fallback = 0.2_f64;
    let new_snapshot = a.margin_in_use(|s| Some(im_for(s).unwrap_or(fallback)));

    // BTC 2·100·1·0.2 = 40 ; ETH 3·50·10·0.2 = 300.
    assert_eq!(old_snapshot.to_bits(), 40.0_f64.to_bits()); // ETH invisible → understated
    assert_eq!(gate.to_bits(), 340.0_f64.to_bits()); // gate always counted ETH
    assert_eq!(new_snapshot.to_bits(), gate.to_bits()); // divergence closed
}

/// The delegation pin: `margin_in_use_priced` with a marks-lookup price closure IS
/// `margin_in_use_by` bit-for-bit (the fold law stays ONE — only the price input
/// generalizes), and a caller-supplied price re-values the SAME fold at that price.
#[test]
fn margin_in_use_priced_marks_closure_is_bit_identical_and_price_generalizes() {
    let a = margin_account();
    let via_by = a.margin_in_use_by(|_k, _p| Some(0.1));
    let via_priced = a.margin_in_use_priced(
        |(v, s, _side), _p| a.marks.get(&(*v, *s)).copied(),
        |_k, _p| Some(0.1),
    );
    assert_eq!(via_by.to_bits(), via_priced.to_bits());
    // a different price basis re-values the fold: BTC 2·90·1·0.1 = 18 ; ETH 3·45·10·0.1 = 135
    let repriced = a.margin_in_use_priced(
        |(_v, s, _side), _p| Some(if s == "BTCUSDT" { 90.0 } else { 45.0 }),
        |_k, _p| Some(0.1),
    );
    assert_eq!(repriced.to_bits(), 153.0_f64.to_bits());
    // None from the price closure is the same unpriceable skip the unmarked position takes
    let btc_only = a.margin_in_use_priced(
        |(_v, s, _side), _p| (s == "BTCUSDT").then_some(100.0),
        |_k, _p| Some(0.1),
    );
    assert_eq!(btc_only.to_bits(), 20.0_f64.to_bits());
}

// --- net-exposure query helpers (Ext 3) --------------------------------------------------

#[test]
fn net_qty_sums_signed_size_per_symbol() {
    let a = margin_account(); // BTC long 2, ETH short 3
    assert_eq!(a.net_qty("BTCUSDT"), 2.0);
    assert_eq!(a.net_qty("ETHUSDT"), -3.0);
    assert_eq!(a.net_qty("NOPE"), 0.0);
}

#[test]
fn net_and_gross_notional_price_signed_and_abs() {
    let a = margin_account();
    let marks = |(v, s, _side): &PositionKey, _p: &PositionEntry| a.marks.get(&(*v, *s)).copied();
    // BTC 2·100·1 = 200 (long, +) ; ETH 3·50·10 = 1500 (short, −) → net −1300
    assert_eq!(a.net_notional_priced(marks).to_bits(), (-1_300.0_f64).to_bits());
    // gross never nets long against short: 200 + 1500 = 1700 (multiplier folded for ETH)
    assert_eq!(a.gross_notional_priced(marks).to_bits(), 1_700.0_f64.to_bits());
}

#[test]
fn notional_skips_flat_and_unpriceable_like_margin() {
    let mut a = margin_account();
    a.positions.insert(
        ("binance".into(), "FLAT".into(), "BOTH".into()),
        PositionEntry { size: 0.0, avg_px: 10.0, ..Default::default() },
    );
    // NOMARK has a size but no marks entry → price_of returns None → excluded.
    a.positions.insert(
        ("binance".into(), "NOMARK".into(), "BOTH".into()),
        PositionEntry { size: 5.0, avg_px: 10.0, ..Default::default() },
    );
    let marks = |(v, s, _side): &PositionKey, _p: &PositionEntry| a.marks.get(&(*v, *s)).copied();
    // still 1700 / −1300 — the flat and the unpriceable contribute nothing.
    assert_eq!(a.gross_notional_priced(marks).to_bits(), 1_700.0_f64.to_bits());
    assert_eq!(a.net_notional_priced(marks).to_bits(), (-1_300.0_f64).to_bits());
}

// --- margin_mode carrier (feat/margin-mode-field) ----------------------------------------

#[test]
fn position_entry_defaults_to_cross_none() {
    let p = PositionEntry::default();
    assert_eq!(p.margin_mode, MarginMode::Cross);
    assert_eq!(p.isolated_margin, None);
    // an explicit two-field construction is Cross/None too (the byte-identity default path).
    let q = PositionEntry { size: 1.0, avg_px: 2.0, ..Default::default() };
    assert_eq!(q.margin_mode, MarginMode::Cross);
    assert_eq!(q.isolated_margin, None);
}

/// THE byte-identity proof at the serde level: a Cross/None position serializes to EXACTLY
/// the two-field object it did before this field existed (no `margin_mode`, no
/// `isolated_margin` key). Since `state_hash` is FNV over these canonical JSON bytes, absence
/// of both keys is proof the hash is unchanged for any all-cross account.
#[test]
fn cross_position_serializes_without_the_new_keys() {
    let p = PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() };
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(json, r#"{"size":2.0,"avg_px":100.0}"#);
    assert!(!json.contains("margin_mode") && !json.contains("isolated_margin"));
}

/// A `Cash` position also leaves `isolated_margin` None — but Cash is NOT the skip default, so
/// it DOES serialize its mode (only `Cross` is skipped). Documents the carrier's shape.
#[test]
fn isolated_position_carries_mode_and_wallet_through_serde() {
    let iso = PositionEntry {
        size: 2.0,
        avg_px: 100.0,
        margin_mode: MarginMode::Isolated,
        isolated_margin: Some(250.0),
    };
    let json = serde_json::to_string(&iso).unwrap();
    assert!(json.contains(r#""margin_mode":"Isolated""#), "{json}");
    assert!(json.contains(r#""isolated_margin":250.0"#), "{json}");
    let back: PositionEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back, iso);

    // Cash carries its mode but no wallet.
    let cash = PositionEntry {
        size: 1.0,
        avg_px: 10.0,
        margin_mode: MarginMode::Cash,
        isolated_margin: None,
    };
    let cjson = serde_json::to_string(&cash).unwrap();
    assert!(cjson.contains(r#""margin_mode":"Cash""#), "{cjson}");
    assert!(!cjson.contains("isolated_margin"), "{cjson}");
    assert_eq!(serde_json::from_str::<PositionEntry>(&cjson).unwrap(), cash);
}

/// Compat pin: an OLD two-field journal/snapshot object (no mode keys) deserializes to
/// Cross/None — `#[serde(default)]` makes the new fields optional on the wire.
#[test]
fn legacy_two_field_json_deserializes_to_cross_none() {
    let p: PositionEntry = serde_json::from_str(r#"{"size":-3.0,"avg_px":55.0}"#).unwrap();
    assert_eq!(p, PositionEntry { size: -3.0, avg_px: 55.0, ..Default::default() });
    assert_eq!(p.margin_mode, MarginMode::Cross);
    assert_eq!(p.isolated_margin, None);
}

/// The fold (sole position writer) carries an isolated position's mode + wallet forward across
/// a subsequent fill — the mode is not reset to Cross by a fill on an existing isolated pos.
#[test]
fn fold_carries_margin_mode_forward_across_fills() {
    let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
    let key: PositionKey = ("binance".into(), "BTCUSDT".into(), "BOTH".into());
    // Seed an ISOLATED position directly (the mode-parsing PR will do this from a venue frame).
    a.positions.insert(
        key,
        PositionEntry {
            size: 1.0,
            avg_px: 100.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(50.0),
        },
    );
    // Add to it via the fold (a buy 1 @ 110).
    a.fold(key, 1, 1.0, 110.0, 1.0);
    let p = a.positions[&key];
    assert_eq!(p.size, 2.0);
    assert_eq!(p.margin_mode, MarginMode::Isolated, "mode must survive the fill");
    assert_eq!(p.isolated_margin, Some(50.0), "allocated wallet must survive the fill");
}

/// A cross (default) position folded through a fill stays Cross/None — byte-identical carrier.
#[test]
fn fold_keeps_cross_positions_cross() {
    let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
    let key: PositionKey = ("binance".into(), "BTCUSDT".into(), "BOTH".into());
    a.fold(key, 1, 1.0, 100.0, 1.0);
    a.fold(key, 1, 1.0, 120.0, 1.0);
    let p = a.positions[&key];
    assert_eq!(p.margin_mode, MarginMode::Cross);
    assert_eq!(p.isolated_margin, None);
}
