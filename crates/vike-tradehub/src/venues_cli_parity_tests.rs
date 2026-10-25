//! **THE `vike-backend venues` PARITY GOLDEN (drift register R1, wave 2 W9a): the screen's printed
//! lines, recorded before the venue seam and held byte-identical after it.**
//!
//! The sister of `tests/arming_parity.rs` (which records the arming rows and the `NotRunning` log
//! lines through the public API): this one records what the operator READS — the table, the
//! sentences under it, the tally — because the renderer is private and the seam changes where its
//! rows come from. The same fixtures (`tests/support/arming_fixtures.rs`), four account tables
//! (nothing; every venue armed at `demo`; the same plus a labelled dukascopy account, which makes
//! the default one yield its sidecar; every venue armed at `live`), each rendered in the two
//! shapes the verb prints (the whole table; `--blocked` with no credential store read).
//!
//! A mismatch NEVER rewrites the golden: the actual text is printed between
//! `VENUES-PARITY-ACTUAL` markers and the test fails (the sister's reason). The golden is the DEFAULT
//! gatewayhub build's (ibkr, fxcm and polymarket absent); the all-venues arming outcomes are held by the
//! gatewayhub's own `arming_parity`. This crate has no venue feature of its own to cfg on any more (W15),
//! so the test asks the gatewayhub's own table which build it drives and says loudly when a unified build
//! turned those features on.
//!
//! **Which lane runs it FOR REAL:** the default roster lane (`ci.yml`'s `test` job, `just test`: this
//! package without `vike-backend/full` beside it). It SKIPS, loudly and by design, in the `tradehub-all`
//! job (`vike-backend/full` unifies the gatewayhub's `ibkr`/`fxcm`/`polymarket` into this crate's dev build,
//! and the all-venues outcomes are the gatewayhub's `arming_parity` golden, which that job runs). A lane
//! where the table cannot be read, or names no row for an optional venue, FAILS: it never skips on that.

use std::fmt::Write as _;

use super::*;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;

use crate::arming_fixtures_tests::{armed_fields, keyed_account};

const GOLDEN: &str = include_str!("../tests/arming_parity/venues_lines_default.txt");

/// Every roster venue armed at `tier` with its complete credential set, one default account each.
fn everything_at(tier: AccountTier) -> AdapterPolicy {
    vike_model::VENUES.iter().fold(AdapterPolicy::default(), |p, v| {
        keyed_account(p, v, &AccountLabel::Default, tier, armed_fields(v))
    })
}

/// The rows the screen prints for `policy`, sorted as `collect` sorts them.
fn rows_of(policy: &AdapterPolicy) -> Vec<VenueArming> {
    arming_rows(&std::collections::HashMap::new(), policy)
}

/// The three venues the DEFAULT gatewayhub build registers `FeatureAbsent`.
const OPTIONAL_VENUES: [&str; 3] = ["ibkr", "fxcm", "polymarket"];

/// Whether the gatewayhub lane this test drives is the DEFAULT build's: ibkr, fxcm and polymarket all
/// registered `FeatureAbsent` by the gatewayhub's own table (`registry::trade::REGISTRY`, a `#[cfg]` pair
/// per feature). A cargo invocation that unifies the gatewayhub's `ibkr`/`fxcm`/`polymarket` features into
/// this crate's dev build (`vike-backend/full` beside this package: the `tradehub-all` job) turns those
/// rows into adapters, and the golden is not that build's.
///
/// ⚠ The precondition is read off the REGISTRY, and a venue the table does not name is a PANIC. The earlier
/// guard asked the printed rows (`peek().is_some() && all(FeatureAbsent)`), so a renamed venue or a row that
/// stopped being printed made it answer "not the default build", and the golden skipped and PASSED for
/// ever. The only remaining way to skip is the table itself saying a feature is on.
fn the_lane_is_the_default_gatewayhub_build() -> bool {
    use vike_gatewayhub::trade::adapters::registry::VenueRow;
    let table = vike_gatewayhub::registry::trade::REGISTRY;
    OPTIONAL_VENUES.iter().all(|v| {
        let row = table.iter().find(|r| r.venue() == *v).unwrap_or_else(|| {
            panic!(
                "the gatewayhub's REGISTRY has no row for `{v}`: this golden's precondition is gone"
            )
        });
        matches!(row, VenueRow::FeatureAbsent { .. })
    })
}

/// In the default build the SCREEN prints a `FeatureAbsent` row for each optional venue. Asserted on its
/// own, before the golden, so a screen that stopped printing them cannot pass by the golden skipping.
fn assert_the_optional_venues_print_as_absent(rows: &[VenueArming]) {
    for v in OPTIONAL_VENUES {
        let mine: Vec<_> = rows.iter().filter(|r| r.venue == v).collect();
        assert!(!mine.is_empty(), "the screen prints no `{v}` row in the default gatewayhub build");
        assert!(
            mine.iter().all(|r| r.block == vike_config::ArmingBlock::FeatureAbsent),
            "the registry registers `{v}` FeatureAbsent but the screen prints another block: {mine:?}"
        );
    }
}

#[test]
fn the_printed_venues_screen_is_the_recorded_one() {
    if !the_lane_is_the_default_gatewayhub_build() {
        eprintln!(
            "SKIPPED the_printed_venues_screen_is_the_recorded_one: the gatewayhub's REGISTRY carries ibkr, fxcm or polymarket (a unified feature build, the `tradehub-all` job); the golden is the default build's and runs for real in the default roster lane"
        );
        return;
    }
    assert_the_optional_venues_print_as_absent(&rows_of(&everything_at(AccountTier::Demo)));
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let tables = [
        ("nothing", AdapterPolicy::default()),
        ("everything-demo", everything_at(AccountTier::Demo)),
        (
            "everything-demo+dukascopy-alt",
            keyed_account(
                everything_at(AccountTier::Demo),
                "dukascopy",
                &alt,
                AccountTier::Demo,
                armed_fields("dukascopy"),
            ),
        ),
        ("everything-live", everything_at(AccountTier::Live)),
    ];
    let mut actual = String::new();
    for (name, policy) in &tables {
        let rows = rows_of(policy);
        writeln!(actual, "===== {name}: the whole table =====").unwrap();
        actual.push_str(&render_table(Some(Path::new("/project/settings")), true, &rows, false));
        writeln!(actual, "===== {name}: --blocked, no credential store read =====").unwrap();
        actual.push_str(&render_table(None, false, &rows, true));
    }
    // ANTI-VACUITY: the screen reached rows that run, rows that do not, and a sentence.
    for needle in ["demo", "NoAccountRow", "SidecarHeldElsewhere", "LiveTierNotWired", "-- why"] {
        assert!(actual.contains(needle), "the screen golden never reaches `{needle}`");
    }
    if actual != GOLDEN {
        println!("-----BEGIN VENUES-PARITY-ACTUAL-----");
        print!("{actual}");
        println!("-----END VENUES-PARITY-ACTUAL-----");
        std::io::Write::flush(&mut std::io::stdout()).expect("flush stdout");
        // The lane merges this stream with the build tool's chatter on stderr; let the text drain
        // so a re-recording is not cut mid-line.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let first = actual
            .lines()
            .zip(GOLDEN.lines())
            .position(|(a, g)| a != g)
            .unwrap_or_else(|| actual.lines().count().min(GOLDEN.lines().count()));
        panic!(
            "the printed `venues` screen differs from the recorded golden, first at line {}:\n  \
             actual:   {:?}\n  recorded: {:?}\n(lines: actual {}, recorded {})",
            first + 1,
            actual.lines().nth(first),
            GOLDEN.lines().nth(first),
            actual.lines().count(),
            GOLDEN.lines().count(),
        );
    }
}
