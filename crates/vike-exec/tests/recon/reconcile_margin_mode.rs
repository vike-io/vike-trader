//! Margin-mode step-2 (read-side only): `ExecutionEngine::apply_snapshot` writes the
//! venue-REPORTED margin mode + isolated wallet from `ReconcileSnapshot.position_margin`
//! (index-aligned with `positions`, like `position_sides`) onto the seeded `PositionEntry`.
//! An EMPTY `position_margin` (every pre-step-2 caller / a venue that reports no margin info)
//! carries the prior entry's carrier forward — the #487 behavior, byte-identical. A venue that
//! DOES report is the mode authority: a Cross report flips a stale local Isolated back to Cross.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, PositionEntry, ReconcileSnapshot, RiskGate, RiskLimits,
};
use vike_model::MarginMode;

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn pos(e: &ExecutionEngine<RecordingClient>, side: &str) -> PositionEntry {
    *e.account
        .positions
        .get(&("sim".into(), "BTCUSDT".into(), side.into()))
        .expect("position seeded")
}

/// A reported Isolated mode + wallet lands on the entry; a same-snapshot Cross row stays Cross/None.
#[test]
fn reported_margin_mode_is_written() {
    let mut e = engine();
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.5)],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        position_margin: vec![("BTCUSDT".to_string(), MarginMode::Isolated, Some(75.0))],
        ..Default::default()
    };
    e.apply_snapshot(&snap);
    let p = pos(&e, "BOTH");
    assert_eq!(p.size, 1.5);
    assert_eq!(p.margin_mode, MarginMode::Isolated);
    assert_eq!(p.isolated_margin, Some(75.0));
}

/// No margin info in the snapshot (empty `position_margin`) carries the PRIOR mode forward — the
/// #487 law: an overwrite must not silently flip an isolated position back to cross.
#[test]
fn absent_margin_report_carries_prior_forward() {
    let mut e = engine();
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry {
            size: 1.0,
            avg_px: 90.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(40.0),
        },
    );
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 2.0)],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default() // position_margin EMPTY = venue reported nothing
    };
    e.apply_snapshot(&snap);
    let p = pos(&e, "BOTH");
    assert_eq!(p.size, 2.0, "qty overwrite unchanged");
    assert_eq!(p.margin_mode, MarginMode::Isolated, "prior mode carried forward");
    assert_eq!(p.isolated_margin, Some(40.0), "prior wallet carried forward");
}

/// A venue that reports Cross OVERWRITES a stale local Isolated — venue truth is the authority
/// (e.g. the operator switched the mode on the venue UI between passes).
#[test]
fn reported_cross_flips_stale_isolated_back() {
    let mut e = engine();
    e.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        PositionEntry {
            size: 1.0,
            avg_px: 90.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(40.0),
        },
    );
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0)],
        position_avg_px: vec![("BTCUSDT".to_string(), 90.0)],
        position_margin: vec![("BTCUSDT".to_string(), MarginMode::Cross, None)],
        ..Default::default()
    };
    e.apply_snapshot(&snap);
    let p = pos(&e, "BOTH");
    assert_eq!(p.margin_mode, MarginMode::Cross, "venue-reported cross wins");
    assert_eq!(p.isolated_margin, None);
}

/// Hedge-mode index alignment: each (LONG/SHORT) row gets ITS OWN reported mode, same `sides.get(i)`
/// law as `position_sides`/`position_avg_px`.
#[test]
fn hedge_rows_get_their_own_modes() {
    let mut e = engine();
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 0.5), ("BTCUSDT".to_string(), -0.25)],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0), ("BTCUSDT".to_string(), 110.0)],
        position_sides: vec![
            ("BTCUSDT".to_string(), "LONG".to_string()),
            ("BTCUSDT".to_string(), "SHORT".to_string()),
        ],
        position_margin: vec![
            ("BTCUSDT".to_string(), MarginMode::Cross, None),
            ("BTCUSDT".to_string(), MarginMode::Isolated, Some(30.0)),
        ],
        ..Default::default()
    };
    e.apply_snapshot(&snap);
    assert_eq!(pos(&e, "LONG").margin_mode, MarginMode::Cross);
    assert_eq!(pos(&e, "SHORT").margin_mode, MarginMode::Isolated);
    assert_eq!(pos(&e, "SHORT").isolated_margin, Some(30.0));
}
