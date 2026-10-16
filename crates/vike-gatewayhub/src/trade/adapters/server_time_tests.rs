//! `server_time`'s unit tests: a venue's clock row, risk and read come from its declaration.

use super::*;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, ClockAuth, PaperCause, Resolution, VenueDeclaration,
};
use vike_bridge_core::venue_adapter_fixture::{PLANTED_DECLARATION, PlantedAdapter};

/// ④: a declared venue whose clock IS on the order path answers the OTHER declaration, so the
/// preflight warns instead of printing "not applicable" over the roster's only order-affecting
/// gap. Pure, no network.
///
/// ⚠ PLANTED (docs/decisions/0096): this crate holds no registry of real venues, so no REAL
/// at-risk row is reachable here; it reads `AT_RISK_REG` below. The real row is pinned through
/// `vike-tradehub`'s registry by
/// `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`'s
/// `polymarket_answers_the_at_risk_clock_outcome_through_the_registry`, and in the bridge by
/// `crates/bridges/polymarket/src/exec_plane/adapter_contract_tests.rs`'s
/// `the_polymarket_row_is_at_risk_because_that_venue_signs_a_timestamp`.
#[test]
fn a_declared_venue_with_orders_at_stake_reports_the_risk_not_a_shrug() {
    let Some(ClockDecl::NotWired { reason, unmeasured_risk: Some(at_stake) }) =
        clock_decl(&AT_RISK_REG, "planted")
    else {
        panic!("the planted row declares the risk it leaves unmeasured");
    };
    let gap = venue_server_time_ms(&AT_RISK_REG, "planted", &HashMap::new(), AccountTier::Demo)
        .expect_err("declared venues never measure");
    assert_eq!(gap, ServerTimeGap::UnmeasuredRisk { reason, at_stake });
}

/// An unknown venue string is NOT silently "declared" — it is an error that names itself.
#[test]
fn an_unknown_venue_is_unreachable_not_declared() {
    let gap = venue_server_time_ms(&[], "not-a-venue", &HashMap::new(), AccountTier::Demo)
        .expect_err("unknown venue");
    match gap {
        ServerTimeGap::Unreachable(e) => assert!(e.contains("not-a-venue"), "{e}"),
        other => panic!("an unknown venue must not read as declared: {other:?}"),
    }
}

// ---- a CONTRACT row's clock is its bridge's declaration -----------------------------------------

/// The server time a wired planted clock row answers.
const DECLARED_CLOCK_MS: i64 = 1_700_000_000_123;
const NOT_WIRED_REASON: &str = "a planted venue publishes no server time";
const AT_STAKE: &str = "a planted venue signs a timestamp into every order";

/// The read every planted clock row offers, wired or not (a declared-only row is never asked).
fn declared_clock_read(_inputs: &AdapterInputs<'_>) -> Result<i64, String> {
    Ok(DECLARED_CLOCK_MS)
}

/// A planted contract row declaring one clock row, whose wired read answers [`DECLARED_CLOCK_MS`].
const fn clock_row(clock: ClockDecl) -> PlantedAdapter {
    PlantedAdapter {
        declaration: VenueDeclaration { clock, ..PLANTED_DECLARATION },
        server_time: Some(declared_clock_read),
        ..PlantedAdapter::new("planted", Resolution::Paper(PaperCause::NoLiveArm))
    }
}

static WIRED_CLOCK: PlantedAdapter = clock_row(ClockDecl::Wired {
    endpoint: "GET /planted/time (public)",
    auth: ClockAuth::Public,
    risk: ClockRisk::SignedTimestamp,
});
static WIRED_CLOCK_REG: [crate::trade::adapters::registry::VenueRow; 1] =
    [crate::trade::adapters::registry::VenueRow::Adapter(&WIRED_CLOCK)];
static DECLARED_ONLY: PlantedAdapter =
    clock_row(ClockDecl::NotWired { reason: NOT_WIRED_REASON, unmeasured_risk: None });
static DECLARED_ONLY_REG: [crate::trade::adapters::registry::VenueRow; 1] =
    [crate::trade::adapters::registry::VenueRow::Adapter(&DECLARED_ONLY)];
static AT_RISK: PlantedAdapter =
    clock_row(ClockDecl::NotWired { reason: NOT_WIRED_REASON, unmeasured_risk: Some(AT_STAKE) });
static AT_RISK_REG: [crate::trade::adapters::registry::VenueRow; 1] =
    [crate::trade::adapters::registry::VenueRow::Adapter(&AT_RISK)];

/// `clock_decl` and `clock_risk` answer a contract row from its bridge's declaration: the row it
/// declares, the risk it declares (the tradehub turns that into thresholds), and nothing to judge where
/// it is not wired.
#[test]
fn a_contract_rows_clock_row_and_risk_come_from_its_declaration() {
    assert_eq!(clock_decl(&WIRED_CLOCK_REG, "planted"), Some(WIRED_CLOCK.declaration.clock));
    assert_eq!(clock_decl(&AT_RISK_REG, "planted"), Some(AT_RISK.declaration.clock));
    assert_eq!(
        clock_risk(&WIRED_CLOCK_REG, "planted"),
        Some(ClockRisk::SignedTimestamp),
        "a measured reading is judged against the declared risk"
    );
    assert_eq!(clock_risk(&DECLARED_ONLY_REG, "planted"), None, "nothing measured, nothing judged");
}

/// `venue_server_time_ms` over a contract row: a WIRED row reads through its bridge's
/// `server_time_ms`, and a declared one answers its own reason — ③ where nothing is at stake, ④
/// where the venue signs the clock into the order path.
#[test]
fn a_contract_rows_clock_reads_through_its_bridge_or_answers_its_declaration() {
    let vars = HashMap::new();
    assert_eq!(
        venue_server_time_ms(&WIRED_CLOCK_REG, "planted", &vars, AccountTier::Demo),
        Ok(DECLARED_CLOCK_MS)
    );
    assert_eq!(
        venue_server_time_ms(&DECLARED_ONLY_REG, "planted", &vars, AccountTier::Demo),
        Err(ServerTimeGap::NotChecked(NOT_WIRED_REASON))
    );
    assert_eq!(
        venue_server_time_ms(&AT_RISK_REG, "planted", &vars, AccountTier::Demo),
        Err(ServerTimeGap::UnmeasuredRisk { reason: NOT_WIRED_REASON, at_stake: AT_STAKE })
    );
}
