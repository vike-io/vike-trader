//! **THE ARMING PARITY GOLDEN (drift register R1, wave 2 W9a): what every roster venue says about
//! every account, at every tier, in every credential state — recorded BEFORE the venue seam, and
//! held byte-identical AFTER it.**
//!
//! The seam moves the venue half of `vike-tradehub-core` behind a `VenueBackend` (the arming table,
//! the preflight reads, the adapter start). Its one promise is that NO arming outcome moves. This
//! binary is that promise made checkable: it renders, as text, the matrix
//!
//! * `[projection]` — roster venue x credential state x account tier: the policy-driven row
//!   `(tier, effective, block)` (what `vike-backend venues` renders and `make_engine_accounts`
//!   selects with) and the policy-less probe `(effective, block)` handed the SAME fields;
//! * `[scenario]` — the account-table shapes a flat matrix cannot reach: no row, an inactive row, a
//!   tier conflict, a labelled account beside the default (dukascopy's one-sidecar override), a
//!   labelled account alone, a venue-scoped conflict, and the startup preflight's stop
//!   (`PreflightFailed` above `SidecarHeldElsewhere`, `PaperTier` untouched);
//! * `[start]` — each of those, driven through the REAL fan-out (`make_engine_accounts`), reduced to
//!   what an offline run can observe (the pre-connect budget refusal names the venue; a stopped
//!   account carries its block), plus EVERY event it logged (the `ARMING:` lines — the
//!   `NotRunning` log lines — at their level, with their structured fields);
//! * `[box]` — the wired-market list, the armed live set the lock claims from, the accounts the
//!   `flags.tradehub_live` switch stops, and the unaddressable-account report.
//!
//! The text is compared with `tests/arming_parity/golden_*.txt`. The FIXTURES and the golden are
//! the contract; the small `Api` block below is the only part that names how the answer is reached,
//! and it is the one block the seam edits (from the registry-taking functions to a `LocalBackend`).
//! The matrix and its render are `vike_tradehub_core::test_support::arming_parity`'s, shared with the
//! wire twin (`crates/vike-tradehub/tests/hop/venue_backend_parity.rs`).
//! ⚠ A mismatch NEVER rewrites the golden: the whole actual text is printed between
//! `ARMING-PARITY-ACTUAL` markers and the test fails. Re-recording is a reviewed decision, and a
//! test that blessed itself could not fail.
//!
//! ⚠ **Two goldens, one per table.** The table the daemon serves records `OrdersHeld`, `SdkAbsent` and the exec-flag
//! arm for the three optional venues; [`crate::optional_venues_absent`]'s records the three as `FeatureAbsent`.
//!
//! ⚠ The captured events leave out the `vike_bridge_core::halt` target: that resolver is
//! process-global and prints the box's executable directory, which differs per machine. Every
//! other event is kept — level, message and structured fields; the module-path target is not
//! (a moved module is the same line).

use std::collections::{HashMap, HashSet};

use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, ArmingBlock};
use vike_secrets::account_credentials::AccountCredentials;
use vike_tradehub_core::AdapterPolicy;
// The matrix, the account-table scenarios and the render are the tradehub's wire twin's too, so they are
// compiled ONCE in tradehub-core's lib (`test-support`); the credential sets stay in this crate's testkit,
// because a crate below the gatewayhub may spell no venue.
use vike_gatewayhub::trade::adapters::registry::VenueAdapterRow;
use vike_tradehub_core::test_support::arming_parity::{
    ParityApi, render_everything, start_outcome, tw,
};

use crate::{account_book, arming_fixtures, bridge_backend};

use account_book::{book, creds};
use arming_fixtures::{armed_fields, live_backend_settings, start_symbol};

const GOLDEN_ALL_VENUES: &str = include_str!("arming_parity/golden_all_venues.txt");
const GOLDEN_OPTIONAL_VENUES_ABSENT: &str = include_str!("arming_parity/golden_default.txt");

/// The `account.id` a policy-less probe's credentials are filed under — the one row
/// `keyed_policy` plants.
const ACCOUNT: i64 = 1;

/// The venues whose arm resolves the account's ROW out of the `account` table, so a policy-less
/// probe (which carries none) answers them `AccountNotInStore` whatever it is handed.
const ROW_RESOLVED: &[&str] = &["dukascopy"];

// ============================================================================================
// THE ONE BLOCK THE SEAM EDITS: how each answer is reached. Everything else is fixtures.
// ============================================================================================

mod api {
    use super::bridge_backend::BridgeBackend;
    use super::*;
    use vike_tradehub_core::VenueBackend as _;

    /// The in-process backend over the table: the real venue half behind the fold's port. No deployment
    /// row: the matrix states only flags.
    fn backend(
        table: &'static [VenueAdapterRow],
        flags: vike_config::Flags,
        policy: &AdapterPolicy,
    ) -> BridgeBackend {
        BridgeBackend::new(table, HashMap::new(), flags, policy)
    }

    /// The policy-less probe: the DEFAULT account's `(effective, block)` handed `credentials`.
    fn probe_under(
        table: &'static [VenueAdapterRow],
        venue: &str,
        flags: vike_config::Flags,
        credentials: AccountCredentials<'_>,
        tier: AccountTier,
    ) -> (Option<AccountTier>, ArmingBlock) {
        vike_gatewayhub::trade::adapters::arming::venue_arming_under(
            table,
            venue,
            &HashMap::new(),
            &flags,
            credentials,
            tier,
        )
    }

    /// The fan-out of one venue through the REAL start.
    fn fan_out(
        table: &'static [VenueAdapterRow],
        venue: &str,
        flags: vike_config::Flags,
        policy: &AdapterPolicy,
        account_symbols: &[(AccountLabel, String)],
    ) -> Result<vike_tradehub_core::EngineAccounts, vike_tradehub_core::EngineError> {
        let (tx, _rx) = vike_execution::event_channel(16);
        let mut live = HashSet::new();
        let backend = backend(table, flags, policy);
        let table = backend.arming_table().expect("the local backend always answers");
        let mut env = vike_tradehub_core::AdapterEnv::new(&backend, &table, &tx, &mut live);
        env.reconcile_enabled = true;
        env.policy = Some(policy);
        vike_tradehub_core::make_engine_accounts(&mut env, venue, account_symbols, &[])
    }

    /// Every answer reached in-process, over the registry-taking functions and this table.
    pub struct InProcess(pub &'static [VenueAdapterRow]);

    impl ParityApi for InProcess {
        /// The arming snapshot of one assembly: the rows `vike-backend venues` renders and
        /// `make_engine_accounts` selects with are read off it.
        fn table(
            &self,
            flags: vike_config::Flags,
            policy: &AdapterPolicy,
        ) -> vike_tradehub_core::ArmingTable {
            backend(self.0, flags, policy).arming_table().expect("the local backend always answers")
        }

        /// The policy-less probe handed the SAME fields (skipped for a row-resolved venue).
        fn probe(
            &self,
            venue: &str,
            flags: vike_config::Flags,
            fields: &[(&'static str, &'static str)],
            tier: AccountTier,
        ) -> Option<String> {
            Some(if ROW_RESOLVED.contains(&venue) {
                "probe skipped (row-resolved)".to_string()
            } else {
                let held = book(venue, ACCOUNT, fields);
                let (effective, block) =
                    probe_under(self.0, venue, flags, creds(&held, venue, ACCOUNT), tier);
                format!("probe effective={} block={block:?}", tw(effective))
            })
        }

        /// The fan-out through the REAL start, and EVERY event it logged.
        fn start(
            &self,
            venue: &str,
            flags: vike_config::Flags,
            policy: &AdapterPolicy,
            account_symbols: &[(AccountLabel, String)],
        ) -> (String, Vec<String>) {
            let (result, events) = vike_log::capture::captured(|| {
                fan_out(self.0, venue, flags, policy, account_symbols)
            });
            (start_outcome(result), event_lines(&events))
        }

        /// The wired markets a build mounts.
        fn wired(&self) -> Vec<vike_tradehub_core::RuntimeMarket> {
            self.table(vike_config::Flags::default(), &AdapterPolicy::default()).wired_markets()
        }
    }
}

// ============================================================================================
// The captured log: every event the fan-out emitted, level + message + structured fields, read
// through the workspace's one scoped capture (`vike_log::capture::captured`).
// ============================================================================================

/// The events of `captured`, one text line each, the box-specific halt resolver left out.
fn event_lines(events: &[vike_log::capture::CapturedEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.target != "vike_bridge_core::halt")
        .map(|e| {
            let fields: Vec<String> = e.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("{} {} {{{}}}", e.level, e.message, fields.join("; "))
        })
        .collect()
}

/// The whole matrix over `table`, rendered in-process, with the kept rows its bybit and ibkr starts read their user
/// names through held (`start_symbol`; the two venues that say when a fee lane is read off a name with no row).
fn render_everything_in_process(table: &'static [VenueAdapterRow]) -> String {
    vike_gatewayhub::testkit::fixtures::hold_bybit_kept_rows();
    vike_ibkr::testing::hold_operator_rows();
    render_everything(
        &api::InProcess(table),
        armed_fields,
        live_backend_settings,
        start_symbol,
        "dukascopy",
    )
}

/// The table the daemon serves, with the classes only the three optional venues reach.
#[test]
fn every_arming_outcome_is_the_recorded_one() {
    assert_is_the_recorded_one(
        vike_gatewayhub::registry::trade::REGISTRY,
        GOLDEN_ALL_VENUES,
        &["block=OrdersHeld", "block=SdkAbsent", "block=ExecFlagUnset"],
    );
}

/// The table without the three optional venues, whose rows arm nothing whatever they are handed.
#[test]
fn every_arming_outcome_without_the_optional_venues_is_the_recorded_one() {
    assert_is_the_recorded_one(
        crate::optional_venues_absent::REGISTRY,
        GOLDEN_OPTIONAL_VENUES_ABSENT,
        &["block=FeatureAbsent"],
    );
}

fn assert_is_the_recorded_one(
    table: &'static [VenueAdapterRow],
    golden: &str,
    table_needles: &[&str],
) {
    let actual = render_everything_in_process(table);

    // ANTI-VACUITY: the matrix must reach every outcome class it exists to hold, or equality below
    // is between two renders that both say "nothing runs".
    let needles = [
        "block=None",
        "block=PaperTier",
        "block=NoCredentials",
        "block=NoAccountRow",
        "block=AccountInactive",
        "block=TierConflict",
        "block=SidecarHeldElsewhere",
        "block=PreflightFailed",
        "block=LiveCredentialsAbsent",
        "refused MissingRiskBudget",
        "ARMING:",
    ];
    for needle in needles.iter().chain(table_needles) {
        assert!(actual.contains(needle), "the parity matrix never reaches `{needle}`");
    }

    if actual != golden {
        println!("-----BEGIN ARMING-PARITY-ACTUAL-----");
        print!("{actual}");
        println!("-----END ARMING-PARITY-ACTUAL-----");
        std::io::Write::flush(&mut std::io::stdout()).expect("flush stdout");
        // The lane merges this stream with the build tool's chatter on stderr; let the text drain
        // so a re-recording is not cut mid-line.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let first = actual
            .lines()
            .zip(golden.lines())
            .position(|(a, g)| a != g)
            .unwrap_or_else(|| actual.lines().count().min(golden.lines().count()));
        panic!(
            "the arming outcomes differ from the recorded golden, first at line {}:\n  actual:   \
             {:?}\n  recorded: {:?}\n(lines: actual {}, recorded {})",
            first + 1,
            actual.lines().nth(first),
            golden.lines().nth(first),
            actual.lines().count(),
            golden.lines().count(),
        );
    }
}
