//! **A fill's wallet statement must not book the fill twice.** The frames below are the ones the CI box's core journal folded
//! on 2026-11-07 around 16:29:49 and 16:30:01 UTC (demo binance perp, ETHUSDT, a market buy and the close of it), in the
//! order it folded them: the `ACCOUNT_UPDATE` first, its `ORDER_TRADE_UPDATE` behind it, with the wallet frame's `T` the
//! later one. The venue's figure after them was 4986.87123728; the engine read 4986.83267949.

use serde_json::{Value, json};
use vike_binance::execution::event_mapper::perp::{map_binance_perp, map_binance_perp_stream};
use vike_binance::family::execution::event_mapper::perp::{
    is_fill_wallet_frame, map_perp, map_perp_stream,
};
use vike_execution::{Account, BalanceMode};
use vike_model::events::Event;

/// The wallet before the buy: the first frame's figure plus the buy's fee.
const WALLET_BEFORE: f64 = 4986.90979507 + 0.0125282;
const WALLET_AFTER: f64 = 4986.87123728;

fn wallet(reason: &str, t: i64, wb: &str) -> Value {
    json!({ "e": "ACCOUNT_UPDATE", "T": t, "a": { "m": reason, "B": [{ "a": "USDT", "wb": wb, "bc": "0" }] } })
}

fn trade(side: &str, t: i64, trade_id: i64, px: &str, fee: &str, order_client_id: &str) -> Value {
    json!({ "e": "ORDER_TRADE_UPDATE", "T": t, "o": {
        "s": "ETHUSDT", "c": order_client_id, "S": side, "o": "MARKET", "x": "TRADE", "X": "FILLED", "q": "0.014", "z": "0.014",
        "l": "0.014", "L": px, "n": fee, "N": "USDT", "t": trade_id, "m": false, "ps": "BOTH", "i": 16853819072_i64,
    }})
}

/// Fold `frames` as the engine folds a stream's events, over an account that holds the wallet before the buy, and answer
/// what the next reconcile pass rolls its balance to (`vike_execution::recon`'s `diff_balance`: realized PnL enters an anchored
/// `balance` there, not at the fill). The engine's 4986.83267949 was this figure.
fn balance_after(frames: &[Value], map: impl Fn(&Value) -> Vec<Event>) -> f64 {
    let mut account = Account::new(1.0, "binance", None, BalanceMode::Delta);
    let seed = wallet("DEPOSIT", 1, &WALLET_BEFORE.to_string());
    let folded = std::iter::once(&seed).chain(frames);
    for frame in folded {
        for event in map(frame) {
            match event {
                Event::AccountState(state) => account.apply_account_state(&state, "USDT"),
                Event::Fill(fill) => {
                    account.apply_fill(&fill);
                }
                _ => {}
            }
        }
    }
    let anchored_at = account.realized_pnl_at_balance_sync.expect("the seed frame anchored it");
    account.balance + (account.realized_pnl - anchored_at)
}

fn recorded() -> Vec<Value> {
    vec![
        wallet("ORDER", 1794068989855, "4986.90979507"),
        trade("BUY", 1794068989811, 348492001, "2237.18", "0.0125282", "vtalgoO68989084"),
        wallet("ORDER", 1794069001636, "4986.87123728"),
        trade("SELL", 1794069001623, 348492255, "2235.32", "0.01251779", "rug1MdGyLf6N77mhJ7atYn"),
    ]
}

#[test]
fn the_recorded_order_of_frames_leaves_the_venues_balance() {
    let close_enough = |balance: f64| (balance - WALLET_AFTER).abs() < 1e-7;
    let family = balance_after(&recorded(), |f| map_perp_stream(f, "binance", "ETHUSDT.P", None));
    assert!(close_enough(family), "the family's stream read {family}, the venue {WALLET_AFTER}");
    let binance = balance_after(&recorded(), |f| {
        map_binance_perp_stream(f, "binance", "ETHUSDT.P", false, None)
    });
    assert!(close_enough(binance), "binance's stream read {binance}, the venue {WALLET_AFTER}");
}

/// The other order, which a venue may also send, reads the same: nothing here depends on who comes first.
#[test]
fn the_fill_ahead_of_its_wallet_frame_reads_the_same() {
    let mut frames = recorded();
    frames.swap(0, 1);
    frames.swap(2, 3);
    let balance = balance_after(&frames, |f| map_perp_stream(f, "binance", "ETHUSDT.P", None));
    assert!((balance - WALLET_AFTER).abs() < 1e-7, "read {balance}, the venue {WALLET_AFTER}");
}

/// Equity reads the venue's figure straight after the round trip, in either order of the frames, with no pass in between:
/// the margin sweep runs every 30 s and the pass only every minute. Nothing is counted twice.
#[test]
fn equity_reads_the_venues_figure_before_any_pass_in_either_order() {
    let equity_after = |frames: &[Value]| {
        let mut account = Account::new(1.0, "binance", None, BalanceMode::Delta);
        let seed = wallet("DEPOSIT", 1, &WALLET_BEFORE.to_string());
        for frame in std::iter::once(&seed).chain(frames) {
            for event in map_perp_stream(frame, "binance", "ETHUSDT.P", None) {
                match event {
                    Event::AccountState(state) => account.apply_account_state(&state, "USDT"),
                    Event::Fill(fill) => {
                        account.apply_fill(&fill);
                    }
                    _ => {}
                }
            }
        }
        account.equity_all(0.0)
    };
    let mut swapped = recorded();
    swapped.swap(0, 1);
    swapped.swap(2, 3);
    for (name, frames) in [("recorded", recorded()), ("swapped", swapped)] {
        let equity = equity_after(&frames);
        assert!((equity - WALLET_AFTER).abs() < 1e-7, "{name}: {equity}, the venue {WALLET_AFTER}");
    }
}

/// Only a fill's statement is left out, and on binance's stream a funding payment's (its funding is the income poll's,
/// `vike_binance::execution::funding`). A wallet frame with no fill behind it still sets the balance, and the mapper the frozen
/// fixture pins still answers for the fill's.
#[test]
fn every_other_reason_stays_authoritative_and_the_pinned_mapper_is_untouched() {
    for reason in ["DEPOSIT", "WITHDRAW", "ASSET_TRANSFER", "FUNDING_FEE", "ADMIN_DEPOSIT"] {
        let frame = wallet(reason, 7, "100.5");
        assert!(!is_fill_wallet_frame(&frame), "{reason}");
        let family = map_perp_stream(&frame, "binance", "ETHUSDT.P", None);
        assert!(family.iter().any(|e| matches!(e, Event::AccountState(_))), "{reason}");
        let binance = map_binance_perp_stream(&frame, "binance", "ETHUSDT.P", false, None);
        if reason == "FUNDING_FEE" {
            assert!(binance.is_empty(), "binance's stream leaves a funding frame to the poll");
        } else {
            assert!(binance.iter().any(|e| matches!(e, Event::AccountState(_))), "{reason}");
        }
    }
    let of_a_fill = wallet("ORDER", 7, "100.5");
    assert!(is_fill_wallet_frame(&of_a_fill));
    assert!(map_perp_stream(&of_a_fill, "binance", "ETHUSDT.P", None).is_empty());
    for pinned in [
        map_perp(&of_a_fill, "binance", "ETHUSDT.P", None),
        map_binance_perp(&of_a_fill, "binance", "X", None),
    ] {
        assert!(pinned.iter().any(|e| matches!(e, Event::AccountState(_))));
    }
    // An order's own reports are not wallet frames: the stream still books them.
    let fill = trade("SELL", 7, 9, "2235.32", "0.01", "c");
    assert!(!is_fill_wallet_frame(&fill));
    assert!(
        map_perp_stream(&fill, "binance", "ETHUSDT.P", None)
            .iter()
            .any(|e| matches!(e, Event::Fill(_)))
    );
}
