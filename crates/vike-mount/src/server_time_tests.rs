use super::*;

use vike_bridge_core::venue_mount::ClockAuth;

/// The canary venues warn LATER than the recv-window ones, because what a reading at them
/// measures is dominated by the venue's own clock (hyperliquid: -220..-424 ms across 40
/// samples from an NTP-disciplined box).
#[test]
fn the_canary_threshold_is_looser_than_the_recv_window_one() {
    let signed = clock_policy_of(ClockRisk::SignedTimestamp);
    let nonce = clock_policy_of(ClockRisk::NonceWindow);
    let none = clock_policy_of(ClockRisk::NoTimestamp);
    assert!(nonce.warn_ms > signed.warn_ms);
    // The two canary risks share THRESHOLDS and differ only in words — a nonce window and an
    // absent timestamp cost the same nothing, but an operator is told which one they have.
    assert_eq!((nonce.warn_ms, nonce.fail_ms), (none.warn_ms, none.fail_ms));
    assert_ne!(nonce.remedy, none.remedy, "…and they must not say the same thing");
    assert_eq!(signed.fail_ms, Some(DEFAULT_CLOCK_FAIL_MS));
    assert_eq!(nonce.fail_ms, None);
}

/// ④ AND ITS ONE SHAPE: a declared venue whose clock IS on the order path answers the OTHER
/// declaration, so the preflight can warn about it instead of printing "not applicable" over the
/// roster's only order-affecting gap. Also pure — no network.
///
/// ⚠ PLANTED since polymarket's port (docs/decisions/0096): the transitional registry this crate
/// carried then registered polymarket `FeatureAbsent`, whose generic clock row declares no risk,
/// and since the contract finished this crate holds no registry of real venues at all, so no REAL
/// at-risk row is reachable here. It reads the planted `AT_RISK_REG` below. The real row is pinned
/// through `vike-tradehub`'s registry by
/// `crates/vike-tradehub/tests/polymarket_mount.rs`'s
/// `polymarket_answers_the_at_risk_clock_outcome_through_the_registry`, and in the bridge by
/// `crates/bridges/polymarket/src/exec_plane/mount_contract_tests.rs`'s
/// `the_polymarket_row_is_at_risk_because_that_venue_signs_a_timestamp`.
#[test]
fn a_declared_venue_with_orders_at_stake_reports_the_risk_not_a_shrug() {
    let Some(ClockDecl::NotWired { reason, unmeasured_risk: Some(at_stake) }) =
        clock_decl(&AT_RISK_REG, "planted")
    else {
        panic!("the planted row declares the risk it leaves unmeasured");
    };
    let gap = venue_server_time_ms(&AT_RISK_REG, "planted", &HashMap::new(), false)
        .expect_err("declared venues never measure");
    assert_eq!(gap, ServerTimeGap::UnmeasuredRisk { reason, at_stake });
}

/// An unknown venue string is NOT silently "declared" — it is an error that names itself.
#[test]
fn an_unknown_venue_is_unreachable_not_declared() {
    let gap = venue_server_time_ms(&[], "not-a-venue", &HashMap::new(), false)
        .expect_err("unknown venue");
    match gap {
        ServerTimeGap::Unreachable(e) => assert!(e.contains("not-a-venue"), "{e}"),
        other => panic!("an unknown venue must not read as declared: {other:?}"),
    }
}

// ---- a CONTRACT row's clock is its bridge's declaration -----------------------------------------

/// The server time a wired [`DeclaredClock`] answers.
const DECLARED_CLOCK_MS: i64 = 1_700_000_000_123;
const NOT_WIRED_REASON: &str = "a planted venue publishes no server time";
const AT_STAKE: &str = "a planted venue signs a timestamp into every order";

/// A planted contract row declaring one clock row, whose wired read answers [`DECLARED_CLOCK_MS`].
struct DeclaredClock {
    clock: ClockDecl,
}

impl vike_bridge_core::venue_mount::VenueMount for DeclaredClock {
    fn venue(&self) -> &'static str {
        "planted"
    }
    fn declaration(&self) -> vike_bridge_core::venue_mount::VenueDeclaration {
        vike_bridge_core::venue_mount::VenueDeclaration {
            addresses_accounts: false,
            process_exclusive: None,
            takes_recon_trigger: false,
            grid_source: vike_bridge_core::venue_mount::DeclaredGridSource::NoGrid,
            book_identity: vike_bridge_core::venue_mount::BookIdentity::Undeterminable {
                why: "a planted venue names no book",
            },
            clock: self.clock,
        }
    }
    fn resolve(
        &self,
        _inputs: &vike_bridge_core::venue_mount::MountInputs<'_>,
    ) -> vike_bridge_core::venue_mount::Resolution {
        vike_bridge_core::venue_mount::Resolution::Paper(
            vike_bridge_core::venue_mount::PaperCause::NoLiveArm,
        )
    }
    fn mount(
        &self,
        _req: vike_bridge_core::venue_mount::MountRequest<'_>,
    ) -> vike_bridge_core::venue_mount::MountOutcome {
        vike_bridge_core::venue_mount::MountOutcome::paper()
    }
    fn server_time_ms(
        &self,
        _inputs: &vike_bridge_core::venue_mount::MountInputs<'_>,
        _timeout: Duration,
    ) -> Result<i64, String> {
        Ok(DECLARED_CLOCK_MS)
    }
}

static WIRED_CLOCK: DeclaredClock = DeclaredClock {
    clock: ClockDecl::Wired {
        endpoint: "GET /planted/time (public)",
        auth: ClockAuth::Public,
        risk: ClockRisk::SignedTimestamp,
    },
};
static WIRED_CLOCK_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&WIRED_CLOCK)];
static DECLARED_ONLY: DeclaredClock = DeclaredClock {
    clock: ClockDecl::NotWired { reason: NOT_WIRED_REASON, unmeasured_risk: None },
};
static DECLARED_ONLY_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&DECLARED_ONLY)];
static AT_RISK: DeclaredClock = DeclaredClock {
    clock: ClockDecl::NotWired { reason: NOT_WIRED_REASON, unmeasured_risk: Some(AT_STAKE) },
};
static AT_RISK_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&AT_RISK)];

/// `clock_decl` and `clock_policy` answer a contract row from its bridge's declaration: the row it
/// declares, the thresholds of its declared risk, and nothing to judge where it is not wired.
#[test]
fn a_contract_rows_clock_row_and_thresholds_come_from_its_declaration() {
    assert_eq!(clock_decl(&WIRED_CLOCK_REG, "planted"), Some(WIRED_CLOCK.clock));
    assert_eq!(clock_decl(&AT_RISK_REG, "planted"), Some(AT_RISK.clock));
    assert_eq!(
        clock_policy(&WIRED_CLOCK_REG, "planted"),
        Some(clock_policy_of(ClockRisk::SignedTimestamp)),
        "a measured reading is judged against the declared risk's thresholds"
    );
    assert_eq!(
        clock_policy(&DECLARED_ONLY_REG, "planted"),
        None,
        "nothing measured, nothing judged"
    );
}

/// `venue_server_time_ms` over a contract row: a WIRED row reads through its bridge's
/// `server_time_ms`, and a declared one answers its own reason — ③ where nothing is at stake, ④
/// where the venue signs the clock into the order path.
#[test]
fn a_contract_rows_clock_reads_through_its_bridge_or_answers_its_declaration() {
    let vars = HashMap::new();
    assert_eq!(
        venue_server_time_ms(&WIRED_CLOCK_REG, "planted", &vars, false),
        Ok(DECLARED_CLOCK_MS)
    );
    assert_eq!(
        venue_server_time_ms(&DECLARED_ONLY_REG, "planted", &vars, false),
        Err(ServerTimeGap::NotChecked(NOT_WIRED_REASON))
    );
    assert_eq!(
        venue_server_time_ms(&AT_RISK_REG, "planted", &vars, false),
        Err(ServerTimeGap::UnmeasuredRisk { reason: NOT_WIRED_REASON, at_stake: AT_STAKE })
    );
}
