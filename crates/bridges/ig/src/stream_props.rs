//! "Arbitrary input never panics" harness for the PRIVATE wire decoders of the exec trade stream:
//! the TLCP update-line parse (`event_mapper::parse_update_line` — note its PIPE-after-item grammar,
//! unlike the market-data codec's), the `CONOK` / `CONERR` line readers [`parse_conok`] /
//! [`is_conerr`], the control-link rewrite [`rebase_host`], the confirm identity helpers
//! (`event_mapper::is_close_confirm` / `confirm_trade_id`), and the `DealRefs` correlation map the
//! read loop in `run_once` consults. The public decoders are covered in
//! `crates/bridges/ig/tests/decoder_never_panics.rs`; these are not reachable from outside the
//! crate, so they get a sibling unit file in the `stream_tests.rs` style.
//!
//! The pipeline test replays `run_once`'s read loop body line for line (parse, sub-id filter, first
//! field, JSON, `dealReference` correlation, decode) because that loop is welded to a live HTTP
//! body and cannot be driven directly.
//!
//! The property is TOTALITY: a hostile line may be skipped, but it must never panic the trade
//! stream thread — a dead thread is delayed working-order fills that never arrive.

use super::*;
use crate::event_mapper::{confirm_trade_id, is_close_confirm};
use proptest::prelude::*;
use serde_json::{Map, Value};

/// Raw bytes -> text the way a lossy socket read would produce it.
fn arb_noise() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<u8>(), 0..512)
        .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        prop_oneof![Just(0i64), Just(-1), Just(i64::MAX), Just(i64::MIN), any::<i64>()]
            .prop_map(Value::from),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        prop::sample::select(vec![
            "BUY",
            "SELL",
            "ACCEPTED",
            "REJECTED",
            "OPEN",
            "CLOSED",
            "PARTIALLY_CLOSED",
            "DELETED",
            "REF1",
            "REF2",
            "DIAAA1",
            "",
            "NaN",
        ])
        .prop_map(|s| Value::String(s.to_string())),
        any::<String>().prop_map(Value::String),
        Just(Value::Array(Vec::new())),
        Just(Value::Object(Map::new())),
    ]
}

/// A confirm-shaped object (the keys `decode_trade_confirm` / `confirm_trade_id` read), over
/// hostile leaves, with `dealReference` mostly one of the two refs the test records.
fn arb_confirm() -> impl Strategy<Value = Value> {
    let keys: &'static [&'static str] =
        &["dealStatus", "status", "dealId", "epic", "direction", "size", "level", "reason", "date"];
    (
        prop::collection::vec(prop::option::weighted(0.8, arb_leaf()), keys.len()..=keys.len()),
        prop_oneof![
            4 => prop::sample::select(vec!["REF1", "REF2"])
                .prop_map(|s| Value::String(s.to_string())),
            1 => arb_leaf(),
        ],
    )
        .prop_map(move |(vals, deal_ref)| {
            let mut m: Map<String, Value> = keys
                .iter()
                .zip(vals)
                .filter_map(|(k, v)| v.map(|v| ((*k).to_string(), v)))
                .collect();
            m.insert("dealReference".to_string(), deal_ref);
            Value::Object(m)
        })
}

/// One line off the trade stream: `U,<sub>,<item>|<CONFIRMS>|<OPU>|<WOU>` carrying a confirm JSON
/// as field 0 (the layout `run_once` reads), control lines, and plain noise.
fn arb_stream_line() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => (
            prop_oneof![3 => Just("1".to_string()), 1 => "[0-9]{1,12}"],
            arb_confirm(),
            "[^\\r\\n]{0,8}",
        )
            .prop_map(|(sub, confirm, tail)| format!("U,{sub},1|{confirm}|{tail}|#")),
        2 => ("[0-9]{0,4}", "[0-9]{0,4}", "[^|\\r\\n]{0,12}", "[0-9]{0,4}")
            .prop_map(|(sub, item, f0, f1)| format!("U,{sub},{item}|{f0}|{f1}")),
        1 => Just("PROBE".to_string()),
        1 => ("[^,]{0,12}", "[0-9]{0,22}", "[0-9]{0,22}", "[^,]{0,20}")
            .prop_map(|(a, b, c, d)| format!("CONOK,{a},{b},{c},{d}")),
        1 => any::<String>(),
        1 => arb_noise(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The handshake line readers are total over arbitrary text; a `CONOK` needs its comma-separated
    /// head, and `rebase_host` always keeps the scheme it was handed (or defaults to https).
    #[test]
    fn handshake_line_readers_survive_arbitrary_text(
        line in prop_oneof![any::<String>(), arb_noise()],
        endpoint in any::<String>(),
        host in any::<String>(),
    ) {
        if parse_conok(&line).is_some() {
            prop_assert!(line.starts_with("CONOK,"));
        }
        let _ = is_conerr(&line);
        let rebased = rebase_host(&endpoint, &host);
        prop_assert!(rebased.ends_with(host.as_str()), "{rebased:?}");
    }

    /// `parse_update_line` is total over arbitrary text and grammar-shaped lines; an accepted line
    /// always carries at least one field slot (`split('|')` never yields none).
    #[test]
    fn update_line_parser_survives_arbitrary_text(line in arb_stream_line()) {
        if let Some(u) = parse_update_line(&line) {
            prop_assert!(!u.fields.is_empty());
        }
    }

    /// The confirm identity helpers are total over arbitrary JSON and confirm-shaped objects.
    #[test]
    fn confirm_identity_survives_structured_json(
        shaped in arb_confirm(),
        free in prop_oneof![arb_leaf(), arb_confirm()],
    ) {
        for v in [&shaped, &free] {
            let _ = is_close_confirm(v);
            let _ = confirm_trade_id(v);
        }
    }

    /// `run_once`'s read loop, replayed over a run of lines against ONE `DealRefs` map: never a
    /// panic, never more than the dual-publish pair per line.
    #[test]
    fn the_read_loop_survives_a_line_sequence(
        lines in prop::collection::vec(arb_stream_line(), 1..10),
    ) {
        let mut refs = DealRefs::default();
        refs.record("coid-1", "REF1", false);
        refs.record("coid-2", "REF2", true);
        let deal_refs: DealRefMap = Arc::new(Mutex::new(refs));
        for line in &lines {
            let line = line.trim();
            if line.is_empty() || line == "PROBE" {
                continue;
            }
            if line.starts_with("LOOP") || line.starts_with("END") {
                break;
            }
            let Some(update) = parse_update_line(line) else { continue };
            if update.sub_id != SUB_ID {
                continue;
            }
            let Some(Some(confirms)) = update.fields.first() else { continue };
            let Ok(v) = serde_json::from_str::<Value>(confirms) else { continue };
            let deal_ref = v.get("dealReference").and_then(|d| d.as_str()).unwrap_or_default();
            let Some(coid) = deal_refs.lock().unwrap().coid_for(deal_ref) else { continue };
            let ts = v.get("date").and_then(|d| d.as_i64()).unwrap_or(0);
            let events = decode_trade_confirm(&v, &coid, ts);
            prop_assert!(events.len() <= 2, "event flood: {} events", events.len());
        }
    }

    /// The correlation map is total over arbitrary ids, and the latest `record` wins in both
    /// directions.
    #[test]
    fn deal_refs_survive_arbitrary_ids(
        ops in prop::collection::vec((any::<String>(), any::<String>(), any::<bool>()), 1..8),
    ) {
        let mut refs = DealRefs::default();
        for (coid, deal_ref, market) in &ops {
            refs.record(coid, deal_ref, *market);
            let resolved = refs.coid_for(deal_ref);
            prop_assert_eq!(resolved.as_deref(), Some(coid.as_str()));
            let pending = refs.pending_for(coid).expect("recorded");
            prop_assert_eq!(pending, Pending { deal_ref: deal_ref.clone(), market: *market });
        }
    }
}
