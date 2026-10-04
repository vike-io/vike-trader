use super::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate, RiskLimits};
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};

/// An engine whose ROUTING key is `route_key` while its `venue` stays the canonical id — the
/// exact shape `vike_mount::make_engine_for_account` produces for a labelled account.
fn engine(venue: &str, route_key: &str) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        "BTCUSDT",
    );
    e.route_key = route_key.to_string();
    e
}

fn build_of(
    primary: &ExecutionEngine<RecordingClient>,
    extras: &[(f64, ExecutionEngine<RecordingClient>)],
) -> CoreSnapshot {
    CoreSnapshot::build(
        1,
        primary,
        extras,
        1_000.0,
        PriceCfg::default(),
        vike_exec::MarginCallConfig::default().mm_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        &[],
        &None,
        0,
        0,
        ReconBlock::default(),
        &[],
    )
}

/// ⚠ **THE BYTE-IDENTITY BASELINE.** On a single-account core every published block's
/// `route_key` IS its `venue` and its `account` is ABSENT — so every consumer that reads
/// `venue` (which is every consumer that exists) reads exactly what it read before these fields
/// were added, and the two new fields carry the "there is one account and this is it" shape.
#[test]
fn a_single_account_node_publishes_the_absent_bare_venue_shape() {
    let snap = build_of(&engine("binance", "binance"), &[(1.0, engine("bybit", "bybit"))]);
    assert_eq!(snap.portfolio.venues.len(), 2);
    for v in &snap.portfolio.venues {
        assert_eq!(v.account, None, "{}: no account key on a single-account node", v.venue);
        assert_eq!(v.route_key, v.venue, "{}: the route key IS the venue id", v.venue);
    }
}

/// …and a LABELLED account says which one it is, through the inverse of the same renderer the
/// mount stamped the engine with — so a reader can tell two blocks of one exchange apart, which
/// is the whole reason §7.1 adds the fields.
#[test]
fn two_accounts_of_one_exchange_publish_two_distinguishable_blocks() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let key = route_key_of("binance", &alt);
    let snap = build_of(&engine("binance", "binance"), &[(1.0, engine("binance", &key))]);

    let venues: Vec<&str> = snap.portfolio.venues.iter().map(|v| v.venue.as_str()).collect();
    assert_eq!(venues, vec!["binance", "binance"], "…which is exactly why `venue` cannot answer");

    assert_eq!(snap.portfolio.venues[0].account, None, "the default account, absent as always");
    assert_eq!(snap.portfolio.venues[0].route_key, "binance");
    assert_eq!(snap.portfolio.venues[1].account, Some(alt));
    assert_eq!(snap.portfolio.venues[1].route_key, "binance#ALT");
}

/// ⚠ **An order names the account of the ENGINE that holds it, across the whole fan-out — not just
/// the primary.** Two accounts of one exchange can each rest an order on one symbol; `venue` and
/// `symbol` are then identical on both and `account` is the only field that says whose book each
/// one rests in (Trade window spec §4.3). The primary's order names none, the labelled extra
/// engine's names its label, and neither is read off the other's engine.
#[test]
fn each_order_names_the_account_of_the_engine_that_holds_it() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let order = |coid: &str| vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(99.0),
        ..Default::default()
    };
    let mut primary = engine("binance", "binance");
    let mut second = engine("binance", &route_key_of("binance", &alt));
    primary.submit_order(&order("p-1"), 0, &mut vike_exec::Outbox::default());
    second.submit_order(&order("a-1"), 0, &mut vike_exec::Outbox::default());

    let snap = build_of(&primary, &[(1.0, second)]);

    let p = snap.order("p-1").expect("the primary engine's order is published");
    let a = snap.order("a-1").expect("the extra engine's order is published");
    assert_eq!(
        (p.venue.as_str(), p.symbol.as_str()),
        (a.venue.as_str(), a.symbol.as_str()),
        "…the same venue and the same symbol, which is why `account` has to exist"
    );
    assert_eq!(p.account, None, "the default account names none");
    assert_eq!(a.account, Some(alt), "the labelled engine's order names its label");
}

/// `VenueBlock::default()` still lets a consumer test name two or three fields and
/// `..Default::default()` the rest — the §7.1 clause that keeps `vike-alerting` (which cannot
/// spell `BalanceMode` at all) compiling — and the defaults are the absent/bare shape.
#[test]
fn the_default_block_defaults_both_new_fields() {
    let d = VenueBlock { venue: "bybit".into(), realized_pnl: -450.0, ..Default::default() };
    assert_eq!(d.account, None);
    assert_eq!(d.route_key, "", "consistent with this block's own EMPTY venue — see the doc");
}

/// The epoch ANSWERS THE QUESTION IT EXISTS FOR: two different account sets get two different
/// values, and the same set gets the same value whatever order the fan-out registered it in.
#[test]
fn the_accounts_epoch_changes_with_the_account_set_and_not_with_the_order() {
    let one = CoreSnapshot::accounts_epoch_of(["binance"]);
    let two = CoreSnapshot::accounts_epoch_of(["binance", "binance#ALT"]);
    assert_ne!(one, two, "arming a second account MUST change the epoch");
    assert_eq!(
        CoreSnapshot::accounts_epoch_of(["binance#ALT", "binance"]),
        two,
        "registration order is not part of the account SET"
    );
    assert_ne!(
        CoreSnapshot::accounts_epoch_of(["binance", "binance#HEDGE"]),
        two,
        "a different label is a different set"
    );
    // ⚠ Nothing but `CoreSnapshot::empty` may publish 0 — the "no fold yet" window
    // `docs/decisions/0041` measured as a hole in its own gate.
    assert_eq!(CoreSnapshot::accounts_epoch_of(std::iter::empty()), 0);
    assert_ne!(one, 0);
    assert_ne!(two, 0);
    assert_eq!(CoreSnapshot::empty("binance", "BTCUSDT").accounts_epoch, 0);
}

/// A concatenation cannot masquerade as a different set: the digest is over NUL-terminated
/// keys, so `["ab","c"]` and `["a","bc"]` are different account sets and different values.
#[test]
fn the_epoch_separates_keys_rather_than_concatenating_them() {
    assert_ne!(
        CoreSnapshot::accounts_epoch_of(["binance", "x"]),
        CoreSnapshot::accounts_epoch_of(["binancex"]),
    );
}

/// The epoch a real build publishes is the digest of the engines it published blocks for —
/// asserted against the derivation so the two cannot drift.
#[test]
fn a_built_snapshot_publishes_its_own_engines_epoch() {
    let snap = build_of(&engine("binance", "binance"), &[(1.0, engine("binance", "binance#ALT"))]);
    assert_eq!(snap.accounts_epoch, CoreSnapshot::accounts_epoch_of(["binance", "binance#ALT"]),);
    assert_ne!(
        snap.accounts_epoch,
        build_of(&engine("binance", "binance"), &[]).accounts_epoch,
        "dropping the second account is an account-set change and must be visible"
    );
}
