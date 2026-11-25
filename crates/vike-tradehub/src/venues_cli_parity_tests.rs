//! **THE `vike-backend venues` PARITY GOLDEN (drift register R1, wave 2 W9a): the screen's printed
//! lines, recorded before the venue seam and held byte-identical after it.**
//!
//! The sister of `tests/arming_parity.rs` (which records the arming rows and the `NotRunning` log
//! lines through the public API): this one records what the operator READS — the table, the
//! sentences under it, the tally — because the renderer is private and the seam changes where its
//! rows come from. The same fixtures (`src/arming_fixtures_tests.rs`), four account tables
//! (nothing; every venue armed at `demo`; the same plus a labelled dukascopy account, which makes
//! the default one yield its sidecar; every venue armed at `live`), each rendered in the two
//! shapes the verb prints (the whole table; `--blocked` with no credential store read).
//!
//! A mismatch NEVER rewrites the golden: the actual text is printed between
//! `VENUES-PARITY-ACTUAL` markers and the test fails (the sister's reason). The golden is the screen the shipped
//! daemon prints: this crate's dev edge asks for the gatewayhub as it ships, ibkr, fxcm and polymarket included
//! (decisions 0131 and 0132 as amended 2026-11-06), so it runs in every lane and has no skip. It
//! skipped and passed in such a build until that day, which is what left the shipped screen with no golden.

use std::fmt::Write as _;

use super::*;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;

use crate::arming_fixtures_tests::keyed_account;
use vike_gatewayhub::testkit::arming_fixtures::armed_fields;

const GOLDEN: &str = include_str!("../tests/arming_parity/venues_lines.txt");

/// Every roster venue armed at `tier` with its complete credential set, one default account each.
fn everything_at(tier: AccountTier) -> AdapterPolicy {
    vike_model::VENUES.iter().fold(AdapterPolicy::default(), |p, v| {
        keyed_account(p, v, &AccountLabel::Default, tier, armed_fields(v))
    })
}

/// The rows the screen prints for `policy`, sorted as `collect` sorts them.
fn rows_of(policy: &AdapterPolicy) -> Vec<ArmingAccount> {
    arming_rows(&std::collections::HashMap::new(), policy)
}

/// The three venues a gatewayhub build without their features would register `FeatureAbsent`.
const OPTIONAL_VENUES: [&str; 3] = ["ibkr", "fxcm", "polymarket"];

/// The golden is the shipped screen only while the table holds the shipped rows: each optional venue is an adapter, and
/// the screen prints it as one. Asserted on its own, before the golden, so a build that lost a feature fails here by
/// name, and not as a screen that differs on some line.
fn assert_the_optional_venues_print_as_shipped(rows: &[ArmingAccount]) {
    use vike_gatewayhub::trade::adapters::registry::VenueAdapterRow;
    for v in OPTIONAL_VENUES {
        let row = vike_gatewayhub::registry::trade::REGISTRY
            .iter()
            .find(|r| r.venue() == v)
            .unwrap_or_else(|| panic!("the gatewayhub's REGISTRY has no row for `{v}`"));
        assert!(
            matches!(row, VenueAdapterRow::Adapter(_)),
            "the gatewayhub registers `{v}` absent in this build: its feature is off in a test build"
        );
        let mine: Vec<_> = rows.iter().filter(|r| r.venue == v).collect();
        assert!(!mine.is_empty(), "the screen prints no `{v}` row");
        assert!(
            mine.iter().all(|r| r.block != vike_model::ArmingBlock::FeatureAbsent),
            "the registry holds `{v}` as an adapter but the screen prints it absent: {mine:?}"
        );
    }
}

#[test]
fn the_printed_venues_screen_is_the_recorded_one() {
    assert_the_optional_venues_print_as_shipped(&rows_of(&everything_at(AccountTier::Demo)));
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
