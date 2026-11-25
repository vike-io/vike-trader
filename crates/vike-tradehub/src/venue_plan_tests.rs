//! The plans' shared refusals (`venue_plan.rs`'s `wired_mount`), pinned word for word for EVERY plan that
//! performs them: the six bodies were one copy each until they shared one, and an operator reads these
//! sentences at a refused start.

use vike_secrets::fixture_account_symbol;
use vike_tradehub_core::MountPlacement;

use super::{alpaca_plan, cex_plan, ctrader_plan, deribit_plan, ig_plan, oanda_plan};
use crate::{StrategyLanePlan, StrategyLanePlanCex};

type Plan = Box<dyn Fn(&MountPlacement) -> Result<StrategyLanePlan, String>>;

/// Every plan, by the slug its refusals name, with the parenthesis its tick-size refusal carries.
fn plans() -> Vec<(&'static str, Plan, &'static str)> {
    let cex = "it sizes both the L2Book price grid the quote pump folds into and the maker's own \
               quoting grid";
    let maker = "it is the maker's own quoting grid";
    let mut out: Vec<(&'static str, Plan, &'static str)> = StrategyLanePlanCex::ALL
        .iter()
        .map(|&v| {
            (v.slug(), Box::new(move |c: &MountPlacement| cex_plan(v, c, false)) as Plan, cex)
        })
        .collect();
    out.push(("alpaca", Box::new(alpaca_plan) as Plan, maker));
    out.push(("ctrader", Box::new(ctrader_plan) as Plan, maker));
    out.push(("oanda", Box::new(oanda_plan) as Plan, maker));
    out.push(("deribit", Box::new(deribit_plan) as Plan, maker));
    out.push(("ig", Box::new(ig_plan) as Plan, maker));
    out
}

/// **A foreign symbol is refused naming the default account's symbol, before the tick size is looked at; a bad
/// tick size on that symbol is refused naming what the tick sizes; that symbol on a good tick plans.** The symbol
/// refusal is the plan dispatcher's (`crate::feeds::venue_feed_plan`, over the account rows), asked before any plan.
#[test]
fn every_plan_refuses_a_foreign_symbol_then_a_bad_tick_in_the_same_words() {
    let policy = vike_tradehub_core::AdapterPolicy {
        accounts: crate::testutil::live_box().accounts.clone(),
        ..vike_tradehub_core::AdapterPolicy::default()
    };
    let table =
        crate::scripted_arming_tests::plan_table(&std::collections::HashMap::new(), &policy);
    for (slug, plan, grid) in plans() {
        let wired = fixture_account_symbol(slug)
            .unwrap_or_else(|| panic!("a {slug} account states a symbol"));

        // A foreign symbol AND a bad tick: the symbol is the refusal (the order of the checks).
        let foreign = MountPlacement::new(slug, "NOT-WIRED", 100_000.0, 0.0);
        assert_eq!(
            crate::feeds::venue_feed_plan(&foreign, &table, &policy).expect_err("a foreign symbol"),
            format!(
                "{slug}'s default account is mounted on {wired:?} (the symbol its row states, \
                 `account.symbol`), but this mount names \"NOT-WIRED\": that engine accepts exactly its \
                 mounted symbol (`Engine::accepts_symbol` \u{2014} no `extra_symbols` are wired), so every \
                 order and fill would be SILENTLY DROPPED"
            ),
            "{slug}"
        );

        for tick in [0.0, -0.01, f64::NAN, f64::INFINITY] {
            let bad = MountPlacement::new(slug, wired, 100_000.0, tick);
            assert_eq!(
                plan(&bad).expect_err("a bad tick"),
                format!("{slug} mount needs a POSITIVE finite tick_size ({grid}), got {tick}"),
                "{slug} at tick {tick}"
            );
        }

        let good = MountPlacement::new(slug, wired, 100_000.0, 0.01);
        let planned = plan(&good).unwrap_or_else(|e| panic!("{slug} plans its wired symbol: {e}"));
        let named = match planned {
            StrategyLanePlan::Cex { venue, mainnet } => {
                assert!(!mainnet, "{slug}: the plan carries the mainnet answer it was handed");
                venue.slug()
            }
            StrategyLanePlan::Alpaca => "alpaca",
            StrategyLanePlan::Ctrader => "ctrader",
            StrategyLanePlan::Oanda => "oanda",
            StrategyLanePlan::Deribit => "deribit",
            StrategyLanePlan::Ig => "ig",
            other => panic!("{slug} planned {other:?}"),
        };
        assert_eq!(named, slug, "each plan yields its own venue's plan");
    }
}

/// alpaca's own refusal stays AFTER the shared three: a wired symbol on a good tick at `5m` is refused for
/// the interval, and a bad tick at `5m` is refused for the tick.
#[test]
fn alpacas_interval_refusal_runs_after_the_shared_ones() {
    let wired = fixture_account_symbol("alpaca").expect("an alpaca account states a symbol");
    let mut five = MountPlacement::new("alpaca", wired, 100_000.0, 0.01);
    five.interval = "5m".to_string();
    assert_eq!(
        alpaca_plan(&five).expect_err("5m"),
        "alpaca's market-data WS serves 1m bars ONLY (`DataClientSpecAlpaca::refuse_bars`), and bars drive \
         the live watchdogs \u{2014} set interval = \"1m\", got \"5m\""
    );
    five.tick_size = 0.0;
    assert!(alpaca_plan(&five).expect_err("bad tick").contains("POSITIVE finite tick_size"));
}
