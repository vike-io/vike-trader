use super::*;

fn buy<'a>(
    wallet: &'a str,
    cid: &'a str,
    oi: u8,
    size: f64,
    price: f64,
    tx: &'a str,
) -> WalletBuy<'a> {
    WalletBuy {
        wallet,
        condition_id: cid,
        outcome_index: oi,
        asset: "tok",
        slug: "slug",
        size,
        price,
        tx_hash: tx,
    }
}

const W: &str = "0x29b52d98ac9ef9414b04164246c95bc63d74cc6c";

#[test]
fn vwap_guards_zero_size() {
    assert_eq!(vwap(100.0, 0.0), 0.0);
    assert_eq!(vwap(100.0, 200.0), 0.5);
}

#[test]
fn crossed_is_inclusive_at_the_threshold() {
    // `>=`, not `>` — an exactly-$2000 accumulation FIRES.
    assert!(crossed(2000.0, 2000.0));
    assert!(!crossed(1_999.999_9, 2000.0));
}

#[test]
fn segments_match_the_python_table() {
    assert_eq!(segment_for("0x32ed517a571c01b6e9adecf61ba81ca48ff2f960"), "multi");
    assert_eq!(segment_for(W), "esports");
    assert_eq!(segment_for("0x31864feb9d25dee93728c6225ba891530967e9ca"), "esports");
    assert_eq!(segment_for("0xdeadbeef"), "");
}

#[test]
fn flat_stake_pnl_equals_shares_minus_stake() {
    // Won at 0.40: shares = 100/0.40 = 250 -> +150.
    assert!((flat_stake_pnl(0.4, 1.0, 100.0) - 150.0).abs() < 1e-12);
    // Lost: exactly -stake, at any price.
    assert_eq!(flat_stake_pnl(0.4, 0.0, 100.0), -100.0);
    assert_eq!(flat_stake_pnl(0.97, 0.0, 100.0), -100.0);
    // No price -> no bet (the `_shares` guard), NOT an infinite share count.
    assert_eq!(flat_stake_pnl(0.0, 1.0, 100.0), 0.0);
}

#[test]
fn tracker_fires_once_at_the_crossing_fill_with_the_wallet_vwap() {
    let mut t = SignalTracker::new(2000.0);
    // 1000 @ 0.50 = $500, then 2000 @ 0.60 = $1200 (cum $1700, below), then 1000 @ 0.55 = $550
    // -> cum $2250 CROSSES. VWAP = 2250 / 4000 = 0.5625.
    assert!(t.ingest_one(&buy(W, "c", 1, 1000.0, 0.50, "a"), true).is_none());
    assert!(t.ingest_one(&buy(W, "c", 1, 2000.0, 0.60, "b"), true).is_none());
    let s = t.ingest_one(&buy(W, "c", 1, 1000.0, 0.55, "c"), true).expect("crossed");
    assert_eq!(s.trigger_buy_usd, 2250.0);
    assert_eq!(s.ideal_price, 0.5625);
    assert_eq!(s.copied_wallet, W);
    assert_eq!(s.segment, "esports");
    assert_eq!(s.outcome_index, 1);
    // ONE-SHOT: further buys on the same key never fire again.
    assert!(t.ingest_one(&buy(W, "c", 1, 9999.0, 0.90, "d"), true).is_none());
    assert_eq!(t.opened_len(), 1);
}

#[test]
fn outcome_index_and_condition_id_are_separate_keys() {
    let mut t = SignalTracker::new(2000.0);
    // $1500 on oi=0 and $1500 on oi=1 of the SAME market cross NEITHER — the key is per side.
    assert!(t.ingest_one(&buy(W, "c", 0, 3000.0, 0.5, "a"), true).is_none());
    assert!(t.ingest_one(&buy(W, "c", 1, 3000.0, 0.5, "b"), true).is_none());
    assert_eq!(t.opened_len(), 0);
    // ...and the same for a different condition_id.
    assert!(t.ingest_one(&buy(W, "d", 0, 3000.0, 0.5, "e"), true).is_none());
    assert_eq!(t.opened_len(), 0);
}

#[test]
fn seed_open_suppresses_a_key_that_would_otherwise_fire() {
    let mut t = SignalTracker::new(2000.0);
    t.seed_open([(W, "c", 1u8)]);
    assert!(t.ingest_one(&buy(W, "c", 1, 10_000.0, 0.9, "a"), true).is_none());
    // ...and does not accumulate it either (the Python `continue`s before the fold).
    assert_eq!(t.cum_usd(W, "c", 1), 0.0);
}

#[test]
fn emit_false_adopts_without_signalling() {
    let mut t = SignalTracker::new(2000.0);
    assert!(t.ingest_one(&buy(W, "c", 1, 5000.0, 0.5, "a"), false).is_none());
    assert_eq!(t.opened_len(), 1);
}

#[test]
fn batch_ingest_dedups_a_repeated_window_row() {
    // The live failure this guards: the data-API returns the same ~200 rows every 5 s.
    let mut t = SignalTracker::new(2000.0);
    let row = buy(W, "c", 1, 1500.0, 0.5, "tx1"); // $750
    for _ in 0..10 {
        assert!(t.ingest(&[row], true).is_empty(), "a repeated row must never accumulate");
    }
    assert_eq!(t.cum_usd(W, "c", 1), 750.0);
    // A genuinely new fill in the same window then crosses.
    let sigs = t.ingest(&[row, buy(W, "c", 1, 2500.0, 0.5, "tx2")], true);
    assert_eq!(sigs.len(), 1);
    assert_eq!(sigs[0].trigger_buy_usd, 2000.0);
}

#[test]
fn batch_prune_keeps_seen_bounded_to_the_window() {
    let mut t = SignalTracker::new(1e12); // never fires — we are testing the prune only
    t.ingest(&[buy(W, "c", 1, 1.0, 0.5, "old")], true);
    t.ingest(&[buy(W, "c", 1, 1.0, 0.5, "new")], true);
    // "old" scrolled out of the window, so its dedup key was dropped — a fill re-presented
    // after scrolling out would be DOUBLE-COUNTED. That is the Python's own documented
    // trade-off, pinned here so a change to it is visible.
    let before = t.cum_usd(W, "c", 1);
    t.ingest(&[buy(W, "c", 1, 1.0, 0.5, "old")], true);
    assert!(t.cum_usd(W, "c", 1) > before);
}

#[test]
fn lossy_dedup_key_collapses_two_identical_fills_in_one_transaction() {
    // The Python key omits `log_index`, so two on-chain fills of the same size and price in
    // ONE transaction are indistinguishable. Counted, never silently swallowed.
    let mut t = SignalTracker::new(2000.0);
    assert!(t.ingest_one(&buy(W, "c", 1, 1000.0, 0.5, "tx"), true).is_none());
    assert!(t.ingest_one(&buy(W, "c", 1, 1000.0, 0.5, "tx"), true).is_none());
    assert_eq!(t.cum_usd(W, "c", 1), 500.0, "the second fill was dropped by the key");
    assert_eq!(t.dedup_collisions(), 1);
}

#[test]
fn symbol_grammar_round_trips_and_rejects_a_market_symbol() {
    let s = signal_symbol(W, "0xabc", 1);
    assert_eq!(s, format!("{W}@0xabc#1"));
    let p = parse_signal_symbol(&s).expect("parses");
    assert_eq!(p.wallet, W);
    assert_eq!(p.market, "0xabc#1");
    assert_eq!(p.condition_id, "0xabc");
    assert_eq!(p.outcome_index, 1);
    // A bare MARKET symbol must NOT parse — that is what keeps the fill tape inert.
    assert!(parse_signal_symbol(&market_symbol("0xabc", 1)).is_none());
    assert!(parse_signal_symbol("0xabc").is_none());
    assert!(parse_signal_symbol("@0xabc#1").is_none());
    assert!(parse_signal_symbol(&format!("{W}@0xabc#x")).is_none());
}

#[test]
fn from_params_reads_every_knob() {
    let toml_src = r#"
wallets = ["0xaaa", "0xbbb"]
stake_usd = 250.0
conviction_usd = 5000
qty = 2
include_maker_fills = false
"#;
    let params: toml::Value = toml::from_str(toml_src).unwrap();
    let s = SportTaker::from_params(&params);
    assert_eq!(s.wallets, vec!["0xaaa".to_string(), "0xbbb".to_string()]);
    assert_eq!(s.stake_usd, 250.0);
    assert_eq!(s.tracker.threshold(), 5000.0);
    assert_eq!(s.qty, 2.0);
    assert!(!s.include_maker_fills);
}

#[test]
fn from_params_defaults_to_the_python_constants() {
    let s = SportTaker::from_params(&toml::Value::Table(Default::default()));
    assert_eq!(s.stake_usd, STAKE_USD);
    assert_eq!(s.tracker.threshold(), CONVICTION_USD);
    assert_eq!(s.qty, 1.0);
    assert!(s.include_maker_fills);
    assert_eq!(s.wallets.len(), 3);
}
