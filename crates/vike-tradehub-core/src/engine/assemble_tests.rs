//! The shared tail's unit tests: what a bind becomes before the fold builds the engine.

use super::*;
use crate::test_support::doubles::NoopClient;
use vike_model::AccountTier;

/// The spec's Finding 1, generic: the tier the parts carry is the BOUND tier — at both tiers, so
/// a fold that carried one fixed tier fails one half.
#[test]
fn the_parts_carry_the_bound_tier() {
    for bound_tier in [AccountTier::Live, AccountTier::Demo] {
        let bind = AdapterBind {
            client: Box::new(NoopClient),
            reconcile: None,
            bound_tier,
            grid: None,
            contract_value: None,
            margin_mode: None,
            leg_grids: vec![],
            leverage_cap: None,
            account_currency: None,
            native_time_release: false,
            broker_code_len: 0,
            mount_symbol: None,
        };
        let parts = parts_from_bind(
            "BTCUSDT.P",
            &[],
            bind,
            GridSource::PerSymbolFetch,
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(parts.bound_tier, bound_tier, "the bound tier, never a CEX conjunct");
        assert_eq!(parts.grid_source, GridSource::PerSymbolFetch);
    }
}

/// A bind with no venue-fetched grid builds the unbounded limits the old parts did, and a declared
/// leg the venue could not answer gets no row (the lookup closure answers `None`).
#[test]
fn a_bind_with_no_grid_builds_empty_limits() {
    let bind = AdapterBind {
        client: Box::new(NoopClient),
        reconcile: None,
        bound_tier: AccountTier::Demo,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: vec![],
        leverage_cap: None,
        account_currency: None,
        native_time_release: false,
        broker_code_len: 0,
        mount_symbol: None,
    };
    let legs = vec!["ETHUSDT".to_string()];
    let parts =
        parts_from_bind("BTCUSDT", &legs, bind, GridSource::InHand, vike_model::FeeSchedule::Free);
    assert_eq!(parts.contract_value, vike_model::ContractValue::default());
    assert_eq!(parts.default_margin_mode, vike_model::MarginMode::Cross);
    assert!(parts.symbol_grids.is_empty(), "no leg grid was answered");
}

/// A degenerate value from the wire (a zero or negative unit, NaN) is FOLDED to a unit of 1.0 with
/// its kind kept, so it can neither size every order at nothing nor flip a sign (decision 0170).
#[test]
fn a_degenerate_contract_value_is_folded_at_the_bind() {
    for bad in [0.0, -2.0, f64::NAN, f64::INFINITY] {
        let bind = AdapterBind {
            client: Box::new(NoopClient),
            reconcile: None,
            bound_tier: AccountTier::Demo,
            grid: None,
            contract_value: Some(vike_model::ContractValue::inverse(bad)),
            margin_mode: None,
            leg_grids: vec![],
            leverage_cap: None,
            account_currency: None,
            native_time_release: false,
            broker_code_len: 0,
            mount_symbol: None,
        };
        let parts = parts_from_bind(
            "BTC-PERPETUAL",
            &[],
            bind,
            GridSource::PerSymbolFetch,
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(parts.contract_value, vike_model::ContractValue::inverse(1.0), "{bad}");
    }
}

// ---- a SPOT mount books `Cash` ----

/// The engine the shared tail builds for `venue`'s DEFAULT account at `demo`, mounted on `symbol`, over a
/// scripted venue that answers no margin mode (as every bridge but hyperliquid's and polymarket's does).
fn mounted(
    venue: &'static str,
    symbol: &str,
) -> vike_execution::Engine<Box<dyn vike_execution::ExecutionClient + Send>> {
    started(venue, symbol, AccountTier::Demo, None).0
}

/// The engine `make_engine_for_account` builds for `venue`'s DEFAULT account at `tier`, mounted on `symbol`, over
/// a scripted venue whose start answers `margin_mode`, and every call the backend was asked.
fn started(
    venue: &'static str,
    symbol: &str,
    tier: AccountTier,
    margin_mode: Option<vike_model::MarginMode>,
) -> (
    vike_execution::Engine<Box<dyn vike_execution::ExecutionClient + Send>>,
    Vec<crate::VenueBackendScriptCall>,
) {
    started_with_legs(venue, symbol, tier, margin_mode, &[])
}

/// [`started`] for a start that declares `legs`.
fn started_with_legs(
    venue: &'static str,
    symbol: &str,
    tier: AccountTier,
    margin_mode: Option<vike_model::MarginMode>,
    legs: &[String],
) -> (
    vike_execution::Engine<Box<dyn vike_execution::ExecutionClient + Send>>,
    Vec<crate::VenueBackendScriptCall>,
) {
    let facts = std::collections::BTreeMap::from([(
        venue.to_string(),
        vike_model::VenueAdapterDeclarationView {
            adapter_present: true,
            addresses_accounts: true,
            takes_reconcile_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_declaration_word: vike_model::ClockDeclarationKind::NotWired,
        },
    )]);
    let row = crate::ArmingRow {
        arming: vike_config::ArmingAccount {
            venue,
            label: AccountLabel::Default,
            tier: Some(tier),
            effective: Some(tier),
            block: vike_model::ArmingBlock::None,
            account_ids: vec![1],
        },
        book: None,
    };
    let table = crate::ArmingTable::new(facts, vec![row]);
    let bind = AdapterBind {
        client: Box::new(NoopClient),
        reconcile: None,
        bound_tier: tier,
        grid: None,
        contract_value: None,
        margin_mode,
        leg_grids: vec![],
        leverage_cap: None,
        account_currency: None,
        native_time_release: false,
        broker_code_len: 0,
        mount_symbol: None,
    };
    let backend = crate::VenueBackendScript::new(table.clone()).script_start(
        &vike_model::accounts::account_keys::route_key_of(venue, &AccountLabel::Default),
        Ok(bind),
    );
    let policy = AdapterPolicy::default().with_account_row(vike_secrets::AccountRow {
        id: 1,
        venue: venue.to_string(),
        tier,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol(venue).map(str::to_string),
        facts: Default::default(),
    });
    let risk = crate::test_support::risk::budget();
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut live = HashSet::new();
    let mut env = crate::AdapterEnv::new(&backend, &table, &tx, &mut live);
    env.risk_profile = Some(&risk);
    env.policy = Some(&policy);
    let engine =
        match crate::make_engine_for_account(&mut env, venue, symbol, &AccountLabel::Default, legs)
        {
            Ok((engine, _)) => engine,
            Err(EngineError::NotRunning(stopped)) => {
                panic!("{venue}/{symbol} did not run: {stopped:?}")
            }
            Err(other) => panic!("{venue}/{symbol} was refused: {other}"),
        };
    (engine, backend.calls())
}

/// **The margin-call sweep never sells a spot holding for its own cost.** A spot account's balance is its quote
/// wallet NET of what it bought (binance spot's `outboundAccountPosition` states USDT, folded authoritatively), so
/// with the holding booked `Cross` a wallet of 5 000 USDT 97 % invested in BTC at a FLAT price read equity 150
/// against a maintenance of 242.5 (5 %), past the 10 % buffer, and the law sold 0.0239 BTC (1 850 USDT). Booked
/// `Cash` the holding is in no pool. The same book on a perp mount is judged by the cross law, unchanged.
#[test]
fn a_spot_mount_is_never_margin_called_on_its_own_cost() {
    let (wallet, px) = (5_000.0, 77_450.0);
    let qty = 0.97 * wallet / px;
    let verdict = |symbol: &str| {
        let mut engine = mounted("binance", symbol);
        engine.account.apply_fill(&vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::prefixed("spot-", symbol),
            order_client_id: String::new(),
            venue: "binance".into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: qty,
            last_price: px,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: vike_model::events::LiquiditySide::Taker,
            timestamp: 0,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        });
        engine.account.apply_account_state(
            &vike_model::events::AccountState {
                venue: "binance".into(),
                balances: vec![("USDT".into(), wallet - qty * px), ("BTC".into(), qty)],
                timestamp: 0,
                route_key: None,
            },
            "USDT",
        );
        engine.account.set_mark_from(
            "binance",
            symbol,
            px,
            vike_execution::MarkSource::VenueMark,
            0,
        );
        let equity = engine.account.equity_all(1.0);
        assert!(
            (equity - 150.0).abs() < 1e-6,
            "{symbol}: equity is the wallet net of cost: {equity}"
        );
        vike_execution::check_margin_call_priced(
            &engine.account,
            equity,
            &vike_model::MarginCallConfig::default(),
            |(v, s, _side), _p| engine.account.mark_of(v, s),
        )
    };
    assert_eq!(
        verdict("BTCUSDT"),
        vike_execution::MarginCall::Healthy,
        "a spot holding is fully funded"
    );
    match verdict("BTCUSDT.P") {
        vike_execution::MarginCall::Liquidate(intents) => {
            assert_eq!(intents.len(), 1);
            assert!((intents[0].qty * px - 1_850.0).abs() < 1e-6, "{intents:?}");
        }
        other => panic!("a perp mount keeps the cross law: {other:?}"),
    }
}

/// A spot mount's `Cash` rides the engine's SNAPSHOT, so a replay core (`Engine::from_snapshot`, which a
/// boot restore's refold also builds) opens the next position on it `Cash` as the live engine does. A derivative
/// mount states nothing: its snapshot carries no admitted mode, so its bytes are what they were.
#[test]
fn a_spot_mount_states_cash_through_its_snapshot_and_a_derivative_mount_states_nothing() {
    use vike_model::MarginMode::{Cash, Cross};
    for (venue, symbol, want) in [
        ("binance", "BTCUSDT", Cash),
        ("binance", "BTCUSDT.P", Cross),
        ("aster", "BTCUSDT", Cash),
        ("aster", "BTCUSDT.P", Cross),
        ("bybit", "BTCUSDT", Cash),
        ("bybit", "BTCUSDT.P", Cross),
        ("okx", "BTCUSDT", Cash),
        ("okx", "BTCUSDT.P", Cross),
        ("hyperliquid", "HYPEUSDC", Cash),
        ("hyperliquid", "BTCUSDC.P", Cross),
        ("deribit", "BTCUSD.P", Cross),
    ] {
        let engine = mounted(venue, symbol);
        assert_eq!(engine.account.default_margin_mode_of(symbol), want, "{venue}/{symbol}");
        let snap = engine.snapshot_state();
        assert_eq!(
            snap.account.admitted_margin_modes.is_empty(),
            want == Cross,
            "{venue}/{symbol}"
        );
        let replayed = vike_execution::Engine::from_snapshot(&snap, NoopClient);
        assert_eq!(
            replayed.account.default_margin_mode_of(symbol),
            want,
            "{venue}/{symbol}: the replay core"
        );
    }
}

/// **A declared leg whose name says spot books `Cash` too**, on the live tail and on a `paper` account alike: the
/// venue stated a mode for the mount's own symbol only, and a leg reaches no `AdmitSymbol`, so booked `Cross` a
/// spot leg was the margin-call sweep's false breach live and a sale of coins paper never bought (the spot rule).
/// A derivative leg states nothing.
#[test]
fn a_declared_spot_leg_books_cash_on_both_arms() {
    use vike_model::MarginMode::{Cash, Cross};
    for (venue, symbol, leg, want) in [
        ("binance", "BTCUSDT.P", "ETHUSDT", Cash),
        ("binance", "BTCUSDT", "ETHUSDT.P", Cross),
        ("bybit", "BTCUSDT.P", "ETHUSDT", Cash),
        ("bybit", "BTCUSDT", "ETHUSDT.P", Cross),
    ] {
        for tier in [AccountTier::Demo, AccountTier::Paper] {
            let legs = [leg.to_string()];
            let (engine, _) = started_with_legs(venue, symbol, tier, None, &legs);
            assert_eq!(engine.extra_symbols, legs, "{venue}/{leg} {tier:?}: the leg is admitted");
            assert_eq!(engine.account.default_margin_mode_of(leg), want, "{venue}/{leg} {tier:?}");
        }
    }
}

/// A mount the VENUE states `Cash` for (polymarket's outcome shares, whatever the symbol's shape) books `Cash`
/// through the same snapshotted row, so its replay agrees too.
#[test]
fn a_mount_the_venue_states_cash_for_books_cash_through_its_snapshot() {
    use vike_model::MarginMode::Cash;
    let token = "71321045679252212594626385532706912750332728571942532289631379312455583992563";
    let (engine, _) = started("polymarket", token, AccountTier::Live, Some(Cash));
    assert_eq!(engine.account.default_margin_mode_of(token), Cash);
    let snap = engine.snapshot_state();
    assert_eq!(snap.account.admitted_margin_modes, vec![(token.to_string(), Cash)]);
    let replayed = vike_execution::Engine::from_snapshot(&snap, NoopClient);
    assert_eq!(replayed.account.default_margin_mode_of(token), Cash, "the replay core");
}

/// **A `paper` account never reaches the shared tail or the venue**: `make_engine_for_account` returns
/// `paper_fallback`'s engine before any backend call, so no start (and no gatewayhub answer) is asked, and its
/// binance SPOT mount books `Cash` as its live twin does (the spot rule, owner 2026-11-11: `crate::arming`'s
/// `admit_spot_cash`), through the same snapshotted row. (Its admissions are refused before any gatewayhub facts
/// are read: the node's `TradeLeaseRegistry` holds a lease only for an account whose start answered `Live`, which a
/// `paper` start never asks.)
#[test]
fn a_paper_spot_engine_is_built_without_the_tail_and_books_cash() {
    use vike_model::MarginMode::Cash;
    let (engine, calls) = started("binance", "BTCUSDT", AccountTier::Paper, None);
    assert!(calls.is_empty(), "a paper start asks the backend nothing: {calls:?}");
    assert_eq!(engine.tier, AccountTier::Paper);
    assert_eq!(engine.account.default_margin_mode_of("BTCUSDT"), Cash);
    assert_eq!(
        engine.snapshot_state().account.admitted_margin_modes,
        vec![("BTCUSDT".to_string(), Cash)]
    );
}
