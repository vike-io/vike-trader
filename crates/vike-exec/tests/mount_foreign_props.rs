//! Property suite for the MOUNTED symbol quoted in a currency other than the ledger's (decision 0183, the
//! mounted-symbol amendment): a EUR account mounted on `EURUSD`, whose mount is declared quoted in `USD`
//! (`ExecutionEngine::declare_mount_quote_ccy`) and rated by `AdmitSymbol` of the mounted symbol.
//!
//! The law, against a hand-written oracle that shares no code with the account: **realized PnL of a round
//! trip on the mount is the QUOTE PnL of each closing portion times the rate in force when THAT fill
//! folds** (the margin-account convention of `crates/vike-exec/src/account/fx.rs`'s module doc; no entry
//! rate is stored), whatever rate stood when the position was opened and however the rate moved between
//! the closing fills; and a commission stated in the ACCOUNT's currency is booked exactly as stated.
//!
//! No wall clock is read: every `now_ms` is passed in. FAILURE POLICY: a counterexample is a REAL finding
//! about the engine (never adjust a bound here to make it pass, decision 0021): commit the seed beside this
//! file as `mount_foreign_props.proptest-regressions`. Own test binary ON PURPOSE: a proptest sidecar
//! disqualifies a file from a grouped binary (`crates/vike-exec/CLAUDE.md`).

use proptest::prelude::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AdmitOutcome, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate,
    SymbolAdmission,
};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{RiskLimits, SymbolGrid};

const VENUE: &str = "oanda";
const MOUNT: &str = "EURUSD";
const NOW: i64 = 1_700_000_000_000;

/// The standard the sums agree to, relative to the SCALE of the cancelling terms (a few ULP of it).
const SCALE_TOL: f64 = 1e-12;

/// A EUR account on `EURUSD` with its mount declared quoted in USD and no rate yet.
fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        MOUNT,
    );
    e.set_account_ccy("EUR").expect("a currency code");
    e.declare_mount_quote_ccy("USD").expect("the mount is declared");
    e.now_ms = NOW;
    e
}

/// The mount's admission at `rate` (EUR per USD), observed at `at`; the engine's clock moves to `at`.
fn admit(e: &mut ExecutionEngine<RecordingClient>, rate: f64, at: i64) -> AdmitOutcome {
    e.now_ms = at;
    e.admit_symbol(&SymbolAdmission {
        route_key: VENUE.into(),
        symbol: MOUNT.into(),
        grid: SymbolGrid { tick_size: Some(1e-5), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some("USD".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(at),
        im_requirement: None,
        release: false,
        margin_mode: None,
    })
}

fn fold(
    e: &mut ExecutionEngine<RecordingClient>,
    id: usize,
    side: i32,
    qty: f64,
    px: f64,
    fee: f64,
) {
    let fill = FillEvent {
        trade_id: TradeId::prefixed("M", id),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: MOUNT.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: fee,
        commission_asset: "EUR".into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    e.on_event(&Event::Fill(fill), &mut Outbox::default());
}

proptest! {
    /// Open `total` units at `entry` while the rate is `r0`, then close them in 1 to 4 portions, each at
    /// its own price, after the rate has moved again (a refreshed admission, each step well inside the jump
    /// cap): realized PnL is the sum, over the closing portions, of `quote PnL x the rate in force at that
    /// fold`; the fees in EUR are the sum of the fees as stated; nothing remains open.
    #[test]
    fn a_round_trip_on_a_foreign_mount_realizes_the_quote_pnl_at_each_closing_rate(
        long in any::<bool>(),
        entry in 0.5f64..2.5,
        r0 in 0.3f64..2.0,
        open_fee in 0.0f64..5.0,
        legs in prop::collection::vec((1u32..=200_000, 0.5f64..2.5, 0.9f64..1.1, 0.0f64..5.0), 1..5),
    ) {
        let mut e = engine();
        prop_assert_eq!(admit(&mut e, r0, NOW), AdmitOutcome::Mounted);

        let side = if long { 1 } else { -1 };
        let total: f64 = legs.iter().map(|l| f64::from(l.0)).sum();
        fold(&mut e, 0, side, total, entry, open_fee);

        let (mut rate, mut at) = (r0, NOW);
        let (mut want, mut scale, mut fees) = (0.0_f64, 0.0_f64, 0.0_f64);
        fees += open_fee;
        for (i, (qty, exit, step, fee)) in legs.iter().enumerate() {
            at += 1_000;
            rate *= step;
            prop_assert_eq!(admit(&mut e, rate, at), AdmitOutcome::Mounted);
            fold(&mut e, i + 1, -side, f64::from(*qty), *exit, *fee);
            want += f64::from(side) * (exit - entry) * f64::from(*qty) * rate;
            scale += f64::from(*qty) * exit.max(entry) * rate;
            fees += fee;
        }

        let got = e.account.realized_pnl;
        prop_assert!(
            (got - want).abs() <= SCALE_TOL * scale,
            "realized {got} want {want} (scale {scale}, last rate {rate})"
        );
        let held = e.account.positions.get(&(VENUE.into(), MOUNT.into(), PositionSide::Both));
        prop_assert_eq!(held.map(|p| p.size), Some(0.0), "every unit was closed");
        prop_assert_eq!(e.account.fees_paid.to_bits(), fees.to_bits(), "EUR fees are booked as stated");
        prop_assert_eq!(e.account.fees_unconverted(), 0);
    }
}
