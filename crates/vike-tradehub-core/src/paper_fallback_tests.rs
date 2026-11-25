//! The paper book's HALT arming and its slippage, asserted on the CONSTRUCTED book.

use super::{holds_credentials, report_not_running, report_table_decision};
use crate::AdapterPolicy;
use crate::policy::DEFAULT_PAPER_SLIPPAGE;

/// Every paper book `make_engine` builds — a `paper`-tier account's simulator, the one place a
/// paper engine is still built (decision 0156) — arms the operator HALT sentinel.
///
/// A paper book is a MOUNT, so `touch <project>/settings/state/HALT` must reach it. Before
/// `paper_client`, eleven paper arms armed nothing: an all-paper daemon silently observed no HALT
/// file.
///
/// ⚠ Asserts on the CONSTRUCTED book, not the source text: a text gate cannot tell an armed
/// construction from an unarmed one. Drop `.with_halt_path(..)` from `paper_client` -> red.
#[test]
fn the_paper_fallback_every_venue_arm_uses_is_halt_armed() {
    let client = super::paper_client(
        "binance",
        "BTCUSDT",
        DEFAULT_PAPER_SLIPPAGE,
        vike_model::FeeSchedule::Free,
    );
    assert!(
        client.halt_path().is_some(),
        "a paper-tier account's book must arm the HALT sentinel — an operator rehearses the \
         kill switch on exactly this mount"
    );
}

/// **A box without the `config.paper_slippage` row builds the book it always built**: no policy at
/// all, and a policy that carries no row, both give the compiled default — `0.0002`, the literal the
/// mount hard-coded — and the book is still HALT-armed.
#[test]
fn a_paper_book_without_the_row_keeps_the_compiled_slippage() {
    assert_eq!(DEFAULT_PAPER_SLIPPAGE, 0.0002, "the figure the mount shipped with");
    let no_policy =
        super::paper_book_for("binance", "BTCUSDT", None, vike_model::FeeSchedule::Free);
    assert_eq!(no_policy.slippage, DEFAULT_PAPER_SLIPPAGE);

    let no_row = AdapterPolicy::default();
    assert_eq!(no_row.paper_slippage, None, "the default policy carries no row");
    let book =
        super::paper_book_for("binance", "BTCUSDT", Some(&no_row), vike_model::FeeSchedule::Free);
    assert_eq!(book.slippage, DEFAULT_PAPER_SLIPPAGE);
    assert!(book.halt_path().is_some(), "the default book is HALT-armed as before");
}

/// **The row reaches the book**: whatever the policy carries is the slippage the paper client
/// fills with, `0` (no slippage at all) and a large figure included, and the book is still
/// HALT-armed. Fees are another parameter: the schedule handed in is the schedule the book keeps.
#[test]
fn the_policy_row_is_the_slippage_the_paper_book_fills_with() {
    for row in [0.0, 0.0001, 0.001, 0.05] {
        let policy = AdapterPolicy { paper_slippage: Some(row), ..AdapterPolicy::default() };
        let book = super::paper_book_for(
            "binance",
            "BTCUSDT",
            Some(&policy),
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(book.slippage, row, "the row {row} is the book's slippage");
        assert!(book.halt_path().is_some(), "{row}: the HALT arming is not the row's to drop");
    }
}

/// **The `config.paper_slippage_table` row prices the book by the backtest's table**: its `percent`
/// value or `market_impact` base is the book's flat fraction, whatever `config.paper_slippage` says
/// (vike-config refuses both rows at once; the table states the whole slippage), and the book is
/// still HALT-armed. No table keeps the row's flat figure, as above.
#[test]
fn the_slippage_table_row_prices_the_paper_book() {
    let table =
        |model: &str, value: Option<f64>, base: Option<f64>| vike_model::ProfileEngineSlippage {
            model: model.to_string(),
            value,
            ticks: None,
            tick: None,
            base,
            exec_time: None,
            window: None,
            gamma: None,
            eta: None,
        };
    for (table, fraction) in [
        (table("percent", Some(0.0005), None), 0.0005_f64),
        (table("market_impact", None, Some(0.0003)), 0.0003),
        (table("market_impact", None, None), 0.0),
    ] {
        let policy =
            AdapterPolicy { paper_slippage_table: Some(table.clone()), ..AdapterPolicy::default() };
        let book = super::paper_book_for(
            "binance",
            "BTCUSDT",
            Some(&policy),
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(book.slippage.to_bits(), fraction.to_bits(), "{table:?}");
        assert!(
            book.halt_path().is_some(),
            "{table:?}: the HALT arming is not the table's to drop"
        );
    }
}

/// **A paper start says, at `error`, that its book will refuse a symbol quoted in another fiat
/// currency** (the book has no rate source, `vike_paper::foreign_quote_refusal`), for the engine symbol
/// and for each declared leg; a dollar-quoted leg is not named.
#[tracing_test::traced_test]
#[test]
fn a_paper_start_on_a_foreign_quoted_symbol_says_so() {
    use vike_model::accounts::account_keys::AccountLabel;
    let legs = ["EURGBP".to_string(), "EURUSD".to_string()];
    let _ = super::paper_engine("oanda", "USDJPY", &AccountLabel::Default, &legs, None, None);
    assert!(logs_contain("USDJPY is quoted in JPY"));
    assert!(logs_contain("EURGBP is quoted in GBP"));
    assert!(!logs_contain("EURUSD is quoted"), "a dollar-quoted leg is not refused");
}

/// **A paper start on a deribit USDC option says, at `error`, that its book will refuse it**: the book has
/// no price for the option's underlying, and deribit charges an option a share of the underlying (a measured
/// fill paid 0.2318 where the premium reading charged 0.0057), so it is refused, never charged on the
/// premium (`vike_paper`'s fee tests hold the book's refusal). The engine's perpetual says nothing of it.
#[tracing_test::traced_test]
#[test]
fn a_paper_start_on_a_deribit_usdc_option_says_its_book_refuses_it() {
    use vike_model::accounts::account_keys::AccountLabel;
    let option = "BTC_USDC-27NOV26-80000-C";
    let perp = "BTC_USDC-PERPETUAL";
    let _ = super::paper_engine("deribit", perp, &AccountLabel::Default, &[], None, None);
    assert!(!logs_contain("paper-option-fee"), "a perpetual is no option");
    let _ = super::paper_engine("deribit", option, &AccountLabel::Default, &[], None, None);
    assert!(logs_contain(&format!("paper-option-fee: {option} is an option")));
}

/// The other half: a start on a dollar-quoted or unnamed-quote symbol logs nothing of the kind.
#[tracing_test::traced_test]
#[test]
fn a_paper_start_on_a_dollar_quoted_symbol_says_nothing() {
    use vike_model::accounts::account_keys::AccountLabel;
    for (venue, symbol) in [("oanda", "EURUSD"), ("binance", "BTCUSDT"), ("alpaca", "AAPL")] {
        let _ = super::paper_engine(venue, symbol, &AccountLabel::Default, &[], None, None);
    }
    assert!(!logs_contain("paper-quote-currency"));
}

/// **A paper start on an ig epic says no refusal**: its unit is stated by the node over ig's demo login AFTER the
/// engine exists (`vike_paper::unit_stated_over_demo_login`), so nothing can have stated it yet and the start said
/// `no price: no demo login for ig` at `error` on every boot of the CI box (2026-11-19) with the demo login up. It says
/// at `info` that the size waits for the node; the stater says a missing demo login (`TradeLeaseRegistry::state_paper_units`).
#[tracing_test::traced_test]
#[test]
fn a_paper_start_on_an_ig_epic_waits_for_the_node_without_an_error() {
    use vike_model::accounts::account_keys::AccountLabel;
    let _ = super::paper_engine("ig", "EURUSD", &AccountLabel::Default, &[], None, None);
    assert!(!logs_contain("no price: no demo login for ig"));
    assert!(logs_contain("until the node states its unit"));
}

/// **A paper engine books a coin-margined contract by the law live books it by** (decisions 0170 and 0258).
/// Deribit's `BTC-PERPETUAL` amount is dollars: an order of 10 at 65,700 is worth 10, and it was worth
/// 657,000 while the paper grid read every symbol as linear. The round trip is the worked example of a
/// live Deribit fill: buy 1,000 at 60,000 and sell 1,000 at 66,000, taker 3.5 bps. The engine is mounted on
/// an inverse contract, so it keeps its books in BTC like a live one: realized `1,000 · (1/60,000 −
/// 1/66,000)` = 0.0015151515 BTC (100 dollars at the exit), the 0.35-dollar fee of each leg at its price
/// (0.0000058333 + 0.0000053030 BTC), 0.0015040152 BTC net, which is the venue's 99.265 dollars at 66,000.
/// Linear, the same fills booked 6,000,000 and 21,000 + 23,100 of fees.
///
/// The same dollars on a binance COIN-M or okx inverse contract are 10 contracts of the 100-dollar face
/// their KEPT row states (here the recorded lists, held equal to the venue's rows by each bridge's tests), so 10
/// at 60,000 is 1,000 dollars of notional and the books are the same to the bit's tolerance.
#[test]
fn a_paper_engine_books_an_inverse_perpetual_by_the_live_law() {
    vike_catalog::hold_recorded_kept_lists();
    // deribit's kept row of its coin-margined perpetual: a paper book reads the row its user name finds (decision 0290).
    let inverse = vike_model::Contract::Perp(vike_model::ContractPerp {
        margining: vike_model::Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    vike_catalog::hold_kept_list(
        "deribit",
        vec![vike_model::Instrument::new("deribit", "BTC-PERPETUAL", "BTC", "USD", inverse)],
    );
    for (venue, symbol, face, qty) in [
        ("deribit", "BTCUSD.P", 1.0, 1_000.0),
        ("binance", "BTCUSD.P", 100.0, 10.0),
        ("okx", "BTCUSD.P", 100.0, 10.0),
    ] {
        a_round_trip_books_the_live_law(venue, symbol, face, qty);
    }
}

/// [`a_paper_engine_books_an_inverse_perpetual_by_the_live_law`] on one contract whose face is `face`
/// dollars: `qty` contracts are 1,000 dollars.
fn a_round_trip_books_the_live_law(venue: &str, symbol: &str, face: f64, qty: f64) {
    use vike_execution::ExecutionClient;
    use vike_model::accounts::account_keys::AccountLabel;
    let (mut engine, _) =
        super::paper_engine(venue, symbol, &AccountLabel::Default, &[], None, None);
    let value = engine.account.contract_value_of(symbol);
    assert_eq!(value, vike_model::ContractValue::inverse(face), "{symbol}");
    assert_eq!(value.order_notional(qty, 60_000.0), 1_000.0, "{symbol}: 1,000 dollars");
    assert_eq!(value.order_notional(10.0, 65_700.0), 10.0 * face, "{symbol}: not 657,000");

    let taker = vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 3.5 };
    let mut book = super::paper_client(venue, symbol, 0.0, taker);
    for (order_client_id, side, open) in [("buy", 1, 60_000.0), ("sell", -1, 66_000.0)] {
        book.submit(&vike_model::OrderRequest {
            order_client_id: order_client_id.into(),
            venue: venue.into(),
            symbol: symbol.into(),
            side,
            qty,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        });
        book.on_bar(&vike_marketdata::Bar {
            timestamp: 1,
            open,
            high: open,
            low: open,
            close: open,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        while let Some(event) = book.poll_events() {
            if let vike_model::events::Event::Fill(fill) = event {
                assert!(engine.account.apply_fill(&fill).applied(), "{order_client_id} folds");
            }
        }
    }
    let account = &engine.account;
    assert!(account.coin_key().is_some(), "a coin ledger");
    let coin_pnl = 1_000.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    assert!((account.realized_pnl - coin_pnl).abs() < 1e-15, "realized {}", account.realized_pnl);
    let fees = 0.35 / 60_000.0 + 0.35 / 66_000.0;
    assert!((account.fees_paid - fees).abs() < 1e-15, "fees {}", account.fees_paid);
    let net = account.realized_pnl - account.fees_paid;
    assert!((net * 66_000.0 - 99.265).abs() < 1e-9, "net {net} BTC");
}

/// **A paper spot pair books `Cash`, as its live twin does** (the spot rule, owner 2026-11-11): a paper account
/// must not be the permissive side of live, and a `Cross` spot pair let a paper strategy sell coins it never
/// bought (go short on spot), which the live engine refuses (`cash_oversell_denial`). A derivative keeps `Cross`.
#[test]
fn a_paper_spot_pair_books_cash_like_its_live_twin() {
    use vike_model::MarginMode;
    use vike_model::accounts::account_keys::AccountLabel;
    for (venue, symbol, mode) in [
        ("binance", "BTCUSDT", MarginMode::Cash),
        ("bybit", "DOGEUSDT", MarginMode::Cash),
        ("okx", "BTCUSDT", MarginMode::Cash),
        ("binance", "BTCUSDT.P", MarginMode::Cross),
        ("bybit", "BTCUSDT.P", MarginMode::Cross),
    ] {
        let (engine, _) =
            super::paper_engine(venue, symbol, &AccountLabel::Default, &[], None, None);
        assert_eq!(engine.account.default_margin_mode_of(symbol), mode, "{venue} {symbol}");
    }
}

/// **A linear paper engine keeps the empty grid it always had**: the coin-settled venues' linear books
/// included, so its journal snapshot and every number it books are what they were.
#[test]
fn a_linear_paper_engine_keeps_an_empty_grid() {
    use vike_model::accounts::account_keys::AccountLabel;
    for (venue, symbol) in
        [("deribit", "BTC_USDC-PERPETUAL"), ("bybit", "BTCUSDT"), ("binance", "BTCUSDT.P")]
    {
        let (engine, _) =
            super::paper_engine(venue, symbol, &AccountLabel::Default, &[], None, None);
        assert!(engine.account.contract_value_grid().is_empty(), "{venue} {symbol}");
    }
}

/// **A paper start on a coin-margined contract whose unit only its row states, and no kept row of it is
/// read, says, at `error`, that its book refuses it** (binance COIN-M, by its user name: decision 0290), as it says
/// a foreign-quoted symbol. One a kept row states is valued, not refused. (okx's inverse user name `ETHUSD.P` is no
/// longer read as inverse with no row: the catalog's name rule reads okx's own ids, the open item in
/// `C:/Projects/worktrees/DEFERRED.md`'s SYM-NAMES door row.)
#[tracing_test::traced_test]
#[test]
fn a_paper_start_on_an_inverse_contract_it_cannot_value_says_so() {
    use vike_model::accounts::account_keys::AccountLabel;
    vike_catalog::hold_recorded_kept_lists();
    let _ =
        super::paper_engine("binance", "SOLUSD_261225", &AccountLabel::Default, &[], None, None);
    let legs = ["ETHUSD_270326".to_string(), "BTCUSD_261225".to_string()];
    let _ = super::paper_engine("binance", "BTCUSDT.P", &AccountLabel::Default, &legs, None, None);
    assert!(logs_contain("paper-inverse-unit: SOLUSD_261225"));
    assert!(logs_contain("paper-inverse-unit: ETHUSD_270326"));
    assert!(!logs_contain("paper-inverse-unit: BTCUSD_261225"), "its kept row states it");
    assert!(!logs_contain("paper-inverse-unit: BTCUSDT.P"), "a linear perpetual is not refused");
}

/// A directory of one `oanda` demo account row `id` 1 (`active` as asked), the key NAMES it owns and
/// an EMPTY credential book: the shape of the tradehub's directory, which holds no venue VALUE
/// (decision 0146's second reopener).
fn names_only_directory(active: bool, names: &[&str]) -> AdapterPolicy {
    let row = vike_secrets::AccountRow {
        id: 1,
        venue: "oanda".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol("oanda").map(str::to_string),
        facts: Default::default(),
    };
    let keys = (!names.is_empty()).then(|| {
        std::collections::BTreeMap::from([(
            1,
            vike_secrets::AccountKeys {
                prefixes: vec![],
                names: names.iter().map(|n| n.to_string()).collect(),
            },
        )])
    });
    AdapterPolicy {
        accounts: vike_secrets::account_directory::AccountDirectory::from_rows(
            vike_secrets::AccountRows::Known(vec![row]),
            keys,
        ),
        ..AdapterPolicy::default()
    }
}

/// **`holds_credentials` answers from the key NAMES when the directory holds no book**: the tradehub
/// reads no venue value, so its book is empty by design, and without the names the `ARMING:` line of
/// an account that does hold keys would drop from `warn` to `debug`. The book half still answers for
/// a process that holds values. Drop the `keys` arm from `holds_credentials` -> the first assertion
/// is red; drop the book arm -> the third is.
#[test]
fn an_account_holds_credentials_by_its_key_names_when_the_book_is_empty() {
    use vike_model::accounts::account_keys::AccountLabel;
    let label = AccountLabel::Default;
    let named = names_only_directory(false, &["oanda_demo_api_key"]);
    assert!(holds_credentials(Some(&named), "oanda", &label), "a key name is a held credential");
    assert!(!holds_credentials(Some(&named), "ig", &label), "another venue's rows are not it");
    assert!(
        !holds_credentials(Some(&names_only_directory(false, &[])), "oanda", &label),
        "an account that owns no key name holds nothing"
    );
    // The book half: a planted value with no key names (a process that holds values).
    let mut book = vike_secrets::account_credentials::CredentialRegistry::new();
    book.insert_account(1, "API_KEY", "v");
    let mut valued = names_only_directory(false, &[]);
    valued.accounts = valued.accounts.with_credentials(book);
    assert!(holds_credentials(Some(&valued), "oanda", &label));
    assert!(!holds_credentials(None, "oanda", &label), "no policy, no directory, no credentials");
}

/// The LEVEL of the `ARMING:` line follows it: `warn` ("It has credentials") for an inactive account
/// whose keys are named, `debug` ("no credentials either") for one that owns none — with the book
/// empty in both.
#[tracing_test::traced_test]
#[test]
fn the_arming_line_of_a_named_key_account_is_a_warn_with_an_empty_book() {
    use vike_model::accounts::account_keys::AccountLabel;
    let block = vike_model::ArmingBlock::AccountInactive;
    report_table_decision(
        "oanda",
        &AccountLabel::Default,
        block,
        Some(&names_only_directory(false, &["oanda_demo_api_key"])),
    );
    assert!(logs_contain("WARN"), "the line is a warn");
    assert!(logs_contain("It has credentials"));
    assert!(!logs_contain("no credentials either"));
}

/// The other half of the pair, in its own test so its capture holds only its own line.
#[tracing_test::traced_test]
#[test]
fn the_arming_line_of_a_keyless_account_stays_a_debug_with_an_empty_book() {
    use vike_model::accounts::account_keys::AccountLabel;
    report_table_decision(
        "oanda",
        &AccountLabel::Default,
        vike_model::ArmingBlock::AccountInactive,
        Some(&names_only_directory(false, &[])),
    );
    assert!(logs_contain("no credentials either"));
    assert!(!logs_contain("It has credentials"));
    assert!(!logs_contain("WARN"));
}

/// **Every cause only the network or the box could produce is an `error`**, the price path's included: the
/// `ARMING:` line of an account that does not run because the DATAHUB is away used to be a `warn` while the
/// `PRICE FEEDS UNAVAILABLE` line beside it was an `error`, and its two siblings (`gatewayhub unavailable`, `adapter
/// failed`) were `error`s. An ordinary refusal (no credentials) stays a `warn`.
#[tracing_test::traced_test]
#[test]
fn the_arming_line_of_every_network_or_box_cause_is_an_error() {
    use vike_model::ArmingBlock;
    use vike_model::accounts::account_keys::AccountLabel;
    let louder = [
        ArmingBlock::PreflightFailed,
        ArmingBlock::AdapterFailed,
        ArmingBlock::GatewayUnavailable,
        ArmingBlock::FeedsUnavailable,
    ];
    for block in louder.into_iter().chain([ArmingBlock::NoCredentials]) {
        report_not_running(
            &crate::ArmingNotRunning {
                venue: "binance".to_string(),
                account: AccountLabel::Default,
                tier: Some(vike_model::AccountTier::Demo),
                block,
            },
            None,
        );
    }
    logs_assert(|lines: &[&str]| {
        let level_of = |block: ArmingBlock| -> Result<&str, String> {
            let needle = format!("does NOT RUN — {}:", block.as_str());
            lines
                .iter()
                .find(|l| l.contains(&needle))
                .map(|l| if l.contains(" ERROR ") { "ERROR" } else { "other" })
                .ok_or_else(|| format!("no line for {}: {lines:?}", block.as_str()))
        };
        for block in louder {
            if level_of(block)? != "ERROR" {
                return Err(format!("{} is not logged at error", block.as_str()));
            }
        }
        if level_of(ArmingBlock::NoCredentials)? == "ERROR" {
            return Err("an ordinary refusal must stay a warn".to_string());
        }
        Ok(())
    });
}

/// **A new paper account row starts at the ONE paper balance, and a 1000 EUR market buy fills at the ask** (the owner's
/// ruling of 2026-11-16). the CI box's paper accounts were seeded 1000 by the box's only mount (`seed_cash = 1000.0`), so
/// at 1x a 1000 EUR buy (~1085 USD) was denied `insufficient-margin`; a paper engine now starts on
/// [`crate::PAPER_SEED_CASH`] whatever the mounts state, while a demo engine keeps the figure its venue's balance
/// replaces. The quote is the paper book's clock: the buy fills at the CURRENT ask.
#[test]
fn a_new_paper_account_row_starts_at_the_one_paper_balance_and_fills_at_the_ask() {
    use std::time::Duration;
    use vike_execution::{Command, OrderIntent};
    use vike_log::poll::wait_until;

    let policy = AdapterPolicy { paper_slippage: Some(0.0), ..AdapterPolicy::default() };
    let build = || {
        super::paper_engine(
            "dukascopy",
            "EURUSD",
            &vike_model::accounts::account_keys::AccountLabel::parse("PAPER").expect("a label"),
            &[],
            None,
            Some(&policy),
        )
        .0
    };
    let mount_seed = 1_000.0;
    assert_eq!(crate::engine_seed(&build(), mount_seed), crate::PAPER_SEED_CASH);
    assert_eq!(crate::PAPER_SEED_CASH, 100_000.0);
    let mut demo = build();
    demo.set_tier(vike_model::AccountTier::Demo);
    assert_eq!(crate::engine_seed(&demo, mount_seed), mount_seed, "a demo engine is not paper");

    // What the order meets on each seed: the one paper balance fills it, the old mount seed denies it.
    let buy = |seed: f64| {
        let handle = vike_core::spawn_core(
            build(),
            vike_core::CoreConfig { seed_cash: seed, ..vike_core::CoreConfig::default() },
        );
        let sink = crate::CoreLaneSink::new(
            handle.bar_sender(),
            handle.market_sender(),
            handle.tick_sender(),
        );
        let quote = |ts: i64| {
            vike_data::DataClientSink::quote(
                &sink,
                "dukascopy",
                "EURUSD",
                vike_marketdata::TickQuote {
                    timestamp: ts,
                    local_timestamp: 0,
                    bid: 1.08495,
                    ask: 1.08505,
                    bid_size: 1_000_000.0,
                    ask_size: 1_000_000.0,
                    symbol: "EURUSD".into(),
                },
            );
        };
        quote(1_000);
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(
            vike_model::OrderRequest {
                order_client_id: "eur-1000".into(),
                venue: "dukascopy".into(),
                symbol: "EURUSD".into(),
                side: 1,
                qty: 1_000.0,
                order_type: vike_model::OrderType::Market,
                ..Default::default()
            },
        ))));
        let mut ts = 2_000;
        let settled = wait_until(Duration::from_secs(10), || {
            ts += 1_000;
            quote(ts);
            let snap = handle.snapshot();
            snap.order("eur-1000").is_some_and(|o| o.filled_qty > 0.0)
                || snap.recent_events.iter().any(|e| e.contains("insufficient-margin"))
        });
        assert!(settled, "the order neither filled nor was denied");
        let snap = handle.snapshot();
        let filled = snap.order("eur-1000").map(|o| (o.filled_qty, o.average_fill_price));
        handle.shutdown_and_join();
        filled
    };
    let (qty, price) = buy(crate::PAPER_SEED_CASH).expect("filled on the one paper balance");
    assert_eq!(qty, 1_000.0);
    assert!((price - 1.08505).abs() < 1e-12, "the ask, on the one paper balance: {price}");
    assert_eq!(buy(mount_seed), None, "denied insufficient-margin on the old 1000 seed");
}

/// **A polymarket paper account mounted on no token fills a token's market buy at the ask** (decision 0285's
/// amendment of 2026-11-16: a manual paper order fills at the current quote on every venue): the account-wide
/// engine's client opens the token's book as the order comes, still HALT-armed, the token's quote fills it at the
/// ask, never the mid, and the account folds the fill.
#[test]
fn an_account_wide_polymarket_paper_engine_fills_a_tokens_market_buy_at_the_ask() {
    use vike_execution::ExecutionClient;
    use vike_model::accounts::account_keys::AccountLabel;
    const TOKEN: &str =
        "71321045679252212594626385532706912750332728571942532289631379312455583992563";
    let policy = AdapterPolicy { paper_slippage: Some(0.0), ..AdapterPolicy::default() };
    let (mut engine, _) =
        super::paper_engine("polymarket", "", &AccountLabel::Default, &[], None, Some(&policy));
    engine.client.submit(&vike_model::OrderRequest {
        order_client_id: "buy".into(),
        venue: "polymarket".into(),
        symbol: TOKEN.into(),
        side: 1,
        qty: 100.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    });
    engine.client.on_quote(TOKEN, 0.48, 0.52, 1_000);
    let mut fills = Vec::new();
    while let Some(event) = engine.client.poll_events() {
        match event {
            vike_model::events::Event::Fill(fill) => fills.push(fill),
            vike_model::events::Event::OrderRejected(r) => panic!("rejected: {r:?}"),
            _ => {}
        }
    }
    assert_eq!(
        fills.iter().map(|f| (f.symbol.as_str(), f.last_qty, f.last_price)).collect::<Vec<_>>(),
        [(TOKEN, 100.0, 0.52)],
        "the ask, never the mid 0.50"
    );
    assert!(engine.account.apply_fill(&fills[0]).applied(), "the account folds it");
}
