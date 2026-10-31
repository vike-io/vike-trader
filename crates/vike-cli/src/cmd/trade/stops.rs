//! `vike-cli trade order stops` - the operator's armed CORE-HELD reduce-only stops, read off the node's
//! OBSERVE snapshot (`WireSnapshot::core_held_stops`, decision 0199's amendment).
//!
//! A reduce-only `--type stop` on a venue whose capability row has no reduce-only stop (polymarket, ig) is
//! not an order at the venue: THE NODE holds it and fires a capped market at the trigger. So `order ls`
//! (the venue's orders) never shows it, and until this verb the only trace was a line on the recent
//! events. A restored stop that waits for its account's first reconcile pass says `HELD`; one re-armed
//! after a refused or part-filled close says how many times. `trade order cancel <ID>` disarms a row by
//! the id printed here.
//!
//! A read: it needs the observe key only, and sends nothing.

use std::process::ExitCode;

use vike_tradehub_client::wire::{CORE_HELD_STOP_MEANING, WireCoreHeldStop};

use crate::cmd::args;
use crate::cmd::hubkeys::{self, HubKeyring};
use crate::exit::Exit;

const COMMAND: &str = "trade order stops";

/// The program name a refusal of this verb's own rules prints under.
const BIN: &str = "vike-cli trade order stops";

/// `--help`'s prose for the verb.
pub(super) const PROSE: &str = "\
A reduce-only `trade order submit ... --type stop --reduce-only` on a venue whose capability row has no \
reduce-only stop (polymarket, ig) is not an order at the venue: THE NODE holds it and sends a market for \
at most the open position at the trigger. `trade order ls` (the venue's orders) never shows it; this does.

STATE: `armed` is watching the price. `HELD until its account's first reconcile pass` is a stop restored \
from the journal after a restart: it fires only after that pass (or after a lapse, when no pass comes). \
`re-armed Nx` is a stop armed again after the close it sent was refused or filled only in part.

`trade order cancel <ID>` disarms a stop by the ID printed here. It protects only while the node runs, \
and not through an outage, a restart gap or a lost price feed.

--node is the node's OBSERVE address (a read).";

/// `stops`'s command line: the observe address and the output shape.
#[derive(clap::Args, Debug)]
pub(super) struct StopsCmd {
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address (required)"
    )]
    node: Option<String>,
    #[arg(long, overrides_with = "json", help = "print the rows as one JSON array")]
    json: bool,
}

pub(super) fn run_stops(c: StopsCmd, keys: &HubKeyring) -> ExitCode {
    let Some(node) = c.node else {
        return args::refuse(BIN, "--node <host:port> is required");
    };
    // The OBSERVE key specifically: this is a read, and a control key cannot authenticate one.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND));
        return ExitCode::FAILURE;
    };
    let snap = match crate::cmd::trade::connect_observe(
        &node,
        key.as_bytes(),
        keys.patience().observe_stall,
    ) {
        Ok((_handle, snap)) => snap,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            for line in hubkeys::observe_permission_denied_lines(COMMAND, &node, &e) {
                eprintln!("{line}");
            }
            return Exit::Failed.into();
        }
        Err(e) => {
            eprintln!("vike-cli {COMMAND}: cannot open observe connection to {node}: {e}");
            return Exit::Connect.into();
        }
    };
    // A frame with nothing built is a placeholder, not "nothing is armed" - said on STDERR, so a
    // `--json` stdout stays one bare document.
    if let Some(line) = crate::cmd::trade::pre_fold_line(&snap) {
        eprintln!("{line}");
    }
    if c.json {
        println!("{}", stops_json(&snap.core_held_stops));
    } else {
        println!("{}", stops_table(&snap.core_held_stops));
    }
    ExitCode::SUCCESS
}

/// The human table: one header, a row per stop, then what a core-held stop is.
fn stops_table(rows: &[WireCoreHeldStop]) -> String {
    let width = |header: &str, cell: fn(&WireCoreHeldStop) -> &str| {
        rows.iter().map(|r| cell(r).len()).chain(std::iter::once(header.len())).max().unwrap_or(0)
    };
    let w_id = width("ID", |r| r.arm_id.as_str());
    let w_account = width("ACCOUNT", |r| r.account.as_str());
    let w_symbol = width("SYMBOL", |r| r.symbol.as_str());
    let mut out = format!(
        "{:<w_id$} {:<w_account$} {:<4} {:>10} {:<w_symbol$} {:>12} STATE",
        "ID", "ACCOUNT", "SIDE", "QTY", "SYMBOL", "TRIGGER"
    );
    if rows.is_empty() {
        out.push_str(
            "\n  (none armed; a node that predates the `core-held-stops` capability lists none \
             either, and `trade order ls` never shows these: they are not orders at the venue)",
        );
        return out;
    }
    for r in rows {
        out.push_str(&format!(
            "\n{:<w_id$} {:<w_account$} {:<4} {:>10} {:<w_symbol$} {:>12} {}",
            r.arm_id,
            r.account,
            side_word(r.side),
            // `fmt_num`, like every engine number of the human tables: a quantity capped to the position
            // is `n * step`, one ULP off its decimal. `--json` keeps the exact value.
            crate::cmd::verbs::fmt_num(r.qty),
            r.symbol,
            crate::cmd::verbs::fmt_num(r.trigger_price),
            state_of(r),
        ));
    }
    out.push_str(&format!(
        "\n  each is {CORE_HELD_STOP_MEANING}; `trade order cancel <ID>` disarms one"
    ));
    out
}

fn side_word(side: i32) -> &'static str {
    if side > 0 { "BUY" } else { "SELL" }
}

/// The STATE column: armed, held for the first reconcile pass, and/or re-armed after a failed close.
fn state_of(r: &WireCoreHeldStop) -> String {
    let mut state = if r.held {
        "HELD until its account's first reconcile pass".to_string()
    } else {
        "armed".to_string()
    };
    if r.rearms > 0 {
        state.push_str(&format!(", re-armed {}x after a refused or part-filled close", r.rearms));
    }
    state
}

/// The flat JSON form: an array of self-describing rows.
fn stops_json(rows: &[WireCoreHeldStop]) -> String {
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.arm_id,
                "venue": r.venue,
                "symbol": r.symbol,
                "account": r.account,
                "side": side_word(r.side).to_lowercase(),
                "qty": r.qty,
                "trigger_price": r.trigger_price,
                "held": r.held,
                "rearms": r.rearms,
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(held: bool, rearms: u8) -> WireCoreHeldStop {
        WireCoreHeldStop {
            arm_id: "s0a1".into(),
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            account: "polymarket#ALT".into(),
            side: -1,
            qty: 5.0,
            trigger_price: 0.4,
            held,
            rearms,
        }
    }

    /// An empty list keeps its header and says what silence can mean (an older node lists none either),
    /// so it is never read as a proof that nothing is armed.
    #[test]
    fn an_empty_listing_keeps_its_header_and_does_not_claim_a_proof() {
        let table = stops_table(&[]);
        assert!(table.starts_with("ID "), "{table}");
        assert!(table.contains("none armed") && table.contains("predates"), "{table}");
        assert_eq!(stops_json(&[]), "[]");
    }

    #[test]
    fn a_row_names_the_id_account_side_trigger_and_state_and_the_table_says_what_a_core_held_stop_is()
     {
        let table = stops_table(&[stop(true, 2)]);
        for part in [
            "s0a1",
            "polymarket#ALT",
            "SELL",
            "TOKEN",
            "0.4",
            "HELD until its account's first reconcile pass",
            "re-armed 2x",
            "NODE's memory",
            "trade order cancel <ID>",
        ] {
            assert!(table.contains(part), "{part:?} missing from\n{table}");
        }
        assert!(stops_table(&[stop(false, 0)]).contains("armed"));
        assert!(!stops_table(&[stop(false, 0)]).contains("re-armed"));
    }

    /// A quantity capped to the position and a trigger are `n * step` products: the human table spells
    /// them as a person would (as the order tables do), while `--json` keeps the engine's exact value.
    #[test]
    fn the_table_does_not_print_the_float_tail_of_a_quantity_or_a_trigger() {
        let mut noisy = stop(false, 0);
        noisy.qty = 7.0_f64 * 0.01 * 0.01;
        noisy.trigger_price = 8_092_593.0_f64 * 0.01;
        assert_ne!(noisy.qty.to_string(), "0.0007", "the fixture is off by an ULP");
        assert_eq!(noisy.trigger_price.to_string(), "80925.93000000001");
        let table = stops_table(std::slice::from_ref(&noisy));
        let row = table.lines().nth(1).expect("one stop row");
        for cell in ["0.0007", "80925.93"] {
            assert!(row.split_whitespace().any(|c| c == cell), "{cell} in:\n{table}");
        }
        assert!(!table.contains("0000000"), "{table}");
        let json: Vec<serde_json::Value> =
            serde_json::from_str(&stops_json(std::slice::from_ref(&noisy)))
                .expect("one JSON array");
        assert_eq!(json[0]["qty"].as_f64(), Some(noisy.qty), "--json is the engine's value");
    }

    #[test]
    fn the_json_row_is_flat_and_carries_both_flags() {
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&stops_json(&[stop(true, 1)])).expect("one JSON array");
        assert_eq!(
            rows[0],
            serde_json::json!({
                "id": "s0a1", "venue": "polymarket", "symbol": "TOKEN", "account": "polymarket#ALT",
                "side": "sell", "qty": 5.0, "trigger_price": 0.4, "held": true, "rearms": 1
            })
        );
    }
}
