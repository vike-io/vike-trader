//! Row 11, the ACTIVE confirm (`bridge_conformance.rs`'s module doc; decision 0171 is the design record). The core's
//! stuck-order ladder (`crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders`) asks a venue about an
//! order by its order client id, and 0171 rules what the answer may be. Every ROSTER venue states its shape here, the
//! deferred venues of the exec table too (a confirm is no frame decoder, so the stateless seam does not bound it):
//!
//! * [`ConfirmForm::None`]: 0171's table names no lookup; `confirm` is the `ExecutionClient` default, a clean no-op.
//! * [`ConfirmForm::Shared`]: the ONE form, `crate::confirm` of vike-bridge-core (`ConfirmByOrderClientId`). Its three
//!   answers are run by the venue's own suite over a stub of its real lookup: found is read by STATE, absent rejects
//!   only an order client id provably dispatched inside the window, a failed query says nothing.
//! * [`ConfirmForm::Own`]: a lookup of the venue's own that 0171 leaves untouched (oanda, ig, fxcm, polymarket: "not
//!   consumers"). Its tests hold 0171's reopener false: absent with no dispatch proof rejects nothing, and a failed
//!   query accepts nothing. A venue shown to do either joins the shared form by a new record (0171's reopener).
//!
//! What is checked against what: the shared-or-own claim against the venue table
//! (`vike_venues::venue_order_client_id_budget::order_client_id_lookup_for`, itself pinned to `with_confirm(` in the
//! bridge sources by `crates/vike-gatewayhub/tests/trade_order_client_id_lookup.rs`); the shared claim against the
//! bridge sources (an implementation of `ConfirmByOrderClientId`); each proof against the file it names.

use std::collections::BTreeSet;
use std::path::Path;

use vike_venues::venue_order_client_id_budget::order_client_id_lookup_for;

/// A test of a venue's own suite that proves a row, by its file (from the repository root) and its name.
pub(super) struct Proof {
    file: &'static str,
    test: &'static str,
}

/// What a venue's exec client answers the core's confirm with (the module doc).
pub(super) enum ConfirmForm {
    /// No lookup by order client id, with 0171's reason: the ladder's last resort decides.
    None(&'static str),
    /// The shared form, proven by these tests.
    Shared(&'static [Proof]),
    /// Its own form, 0171's reopener held false by these tests.
    Own(&'static [Proof]),
}

impl ConfirmForm {
    /// The line `coverage_matrix` prints.
    pub(super) fn describe(&self) -> String {
        let named = |by: &[Proof]| {
            by.iter().map(|p| format!("`{}`", p.test)).collect::<Vec<_>>().join(", ")
        };
        match self {
            ConfirmForm::None(why) => format!("no active confirm: {why}"),
            ConfirmForm::Shared(by) => format!("the shared form, its answers by {}", named(by)),
            ConfirmForm::Own(by) => format!("its own form, the reopener held by {}", named(by)),
        }
    }

    fn proofs(&self) -> &'static [Proof] {
        match self {
            ConfirmForm::None(_) => &[],
            ConfirmForm::Shared(by) | ConfirmForm::Own(by) => by,
        }
    }
}

/// The ladder's last resort, for every venue 0171's table gives no lookup.
const NO_LOOKUP: &str = "0171's table names no lookup by order client id for it; its `confirm` is the \
     `ExecutionClient` default and the stuck-order ladder's last resort decides";

const BINANCE: &str = "crates/bridges/binance/tests/offline/confirm_by_order_client_id.rs";
const ASTER: &str = "crates/bridges/aster/tests/offline/confirm_by_order_client_id.rs";
const BYBIT: &str = "crates/bridges/bybit/tests/offline/confirm_by_order_client_id.rs";
const OKX: &str = "crates/bridges/okx/tests/offline/confirm_by_order_client_id.rs";
const HYPERLIQUID: &str = "crates/bridges/hyperliquid/src/execution/confirm_tests.rs";

/// Every roster venue's row. ⚠ The table is the authority; the module doc names no venue a row does not.
#[rustfmt::skip]
pub(super) const CONFIRM_ROWS: &[(&str, ConfirmForm)] = &[
    ("binance", ConfirmForm::Shared(&[
        Proof { file: BINANCE, test: "a_found_order_is_read_by_status_not_by_existing" },
        Proof { file: BINANCE, test: "perp_confirm_absent_is_the_venue_confirms_absent_reject" },
        Proof { file: BINANCE, test: "perp_confirm_failed_query_emits_nothing" },
        Proof { file: BINANCE, test: "the_confirm_closure_trusts_absent_only_after_dispatch_and_backs_off" },
    ])),
    ("aster", ConfirmForm::Shared(&[
        Proof { file: ASTER, test: "perp_confirm_found_is_accepted_with_the_venue_id_and_hits_fapi_order" },
        Proof { file: ASTER, test: "perp_confirm_absent_is_the_venue_confirms_absent_reject" },
        Proof { file: ASTER, test: "perp_confirm_failed_query_emits_nothing" },
        Proof { file: ASTER, test: "spot_confirm_reads_status_and_trusts_absent_only_after_dispatch" },
    ])),
    ("bybit", ConfirmForm::Shared(&[
        Proof { file: BYBIT, test: "confirm_found_closed_is_the_accept_then_its_terminal" },
        Proof { file: BYBIT, test: "confirm_absent_on_both_endpoints_is_the_venue_confirms_absent_reject" },
        Proof { file: BYBIT, test: "confirm_failed_query_emits_nothing" },
        Proof { file: BYBIT, test: "the_confirm_closure_rejects_absent_only_after_dispatch_and_stays_silent_on_a_failed_query" },
    ])),
    ("okx", ConfirmForm::Shared(&[
        Proof { file: OKX, test: "confirm_found_closed_is_the_accept_then_its_terminal" },
        Proof { file: OKX, test: "confirm_absent_is_the_venue_confirms_absent_reject" },
        Proof { file: OKX, test: "confirm_failed_query_emits_nothing" },
        Proof { file: OKX, test: "the_confirm_closure_rejects_absent_only_after_dispatch_and_stays_silent_on_a_failed_query" },
    ])),
    ("hyperliquid", ConfirmForm::Shared(&[
        Proof { file: HYPERLIQUID, test: "a_closed_order_is_the_accept_and_its_terminal" },
        Proof { file: HYPERLIQUID, test: "absent_rejects_only_a_dispatched_order" },
        Proof { file: HYPERLIQUID, test: "a_failed_read_and_a_wrong_shape_say_nothing" },
    ])),
    ("oanda", ConfirmForm::Own(&[
        Proof { file: "crates/bridges/oanda/src/execution/execution_tests.rs", test: "confirm_never_rules_an_order_absent" },
    ])),
    ("ig", ConfirmForm::Own(&[
        Proof { file: "crates/bridges/ig/src/execution/execution_tests.rs", test: "the_confirm_rejects_no_unproven_order_and_accepts_nothing_on_a_failed_read" },
    ])),
    ("fxcm", ConfirmForm::Own(&[
        Proof { file: "crates/bridges/fxcm/src/execution/execution_tests.rs", test: "a_confirm_that_cannot_find_the_order_says_nothing_and_keeps_the_row" },
    ])),
    ("polymarket", ConfirmForm::Own(&[
        Proof { file: "crates/bridges/polymarket/src/execution/execution_tests.rs", test: "confirm_resolves_an_unknown_submit_only_when_the_venue_has_the_order" },
        Proof { file: "crates/bridges/polymarket/src/execution/execution_tests.rs", test: "confirm_says_nothing_on_a_non_answer" },
    ])),
    ("deribit", ConfirmForm::None(
        "0171 item 5 is not built: its wrapper delegates to an actor built without `with_confirm`, and the \
         submit-timeout read (`resolve_ambiguous_submit`) is no confirm; the ladder's last resort decides",
    )),
    ("alpaca", ConfirmForm::None(NO_LOOKUP)),
    ("ibkr", ConfirmForm::None(NO_LOOKUP)),
    ("ctrader", ConfirmForm::None(NO_LOOKUP)),
    ("dukascopy", ConfirmForm::None(NO_LOOKUP)),
    // vike:new-venue:row ("{venue}", ConfirmForm::None("TODO(new-venue: {venue}): state its active confirm: None with 0171's reason, or the shared form or its own with the tests that prove it")),
];

/// Every bridge directory under `crates/bridges` whose production sources (no `_tests.rs` file, no comment line)
/// name `needle`.
fn bridges_whose_sources_name(needle: &str) -> BTreeSet<String> {
    fn names(dir: &Path, needle: &str) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else { return false };
        entries.flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                return names(&path, needle);
            }
            let is_production = path.extension().is_some_and(|e| e == "rs")
                && !path.to_string_lossy().ends_with("_tests.rs");
            is_production
                && std::fs::read_to_string(&path).is_ok_and(|text| {
                    text.lines().any(|l| !l.trim_start().starts_with("//") && l.contains(needle))
                })
        })
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bridges");
    std::fs::read_dir(&root)
        .expect("crates/bridges")
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| dir.is_dir() && names(&dir.join("src"), needle))
        .filter_map(|dir| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// **Row 11's roster.** Every venue of `vike_model::VENUES` has exactly one confirm row, a reason or its proofs.
#[test]
fn every_roster_venue_states_its_active_confirm_once() {
    let rows: Vec<&str> = CONFIRM_ROWS.iter().map(|(venue, _)| *venue).collect();
    let unique: BTreeSet<&str> = rows.iter().copied().collect();
    assert_eq!(unique.len(), rows.len(), "a venue has two confirm rows: {rows:?}");
    let roster: BTreeSet<&str> = vike_model::VENUES.iter().copied().collect();
    assert_eq!(unique, roster, "the confirm rows must name exactly the roster");
    for (venue, form) in CONFIRM_ROWS {
        match form {
            ConfirmForm::None(why) => assert!(!why.trim().is_empty(), "{venue}: no reason"),
            ConfirmForm::Shared(by) | ConfirmForm::Own(by) => {
                assert!(!by.is_empty(), "{venue}: a confirm proven by no test")
            }
        }
    }
}

/// **Row 11 against the venue table and the sources.** A venue confirms (shared or own) exactly when its
/// `order_client_id_lookup_for` row says it looks the order up; the venues whose bridge implements
/// `ConfirmByOrderClientId` are exactly the shared ones (a bridge's directory under `crates/bridges` is its venue id
/// for every one of them).
#[test]
fn every_confirm_row_agrees_with_the_venue_table_and_the_bridge_sources() {
    for (venue, form) in CONFIRM_ROWS {
        let confirms = !matches!(form, ConfirmForm::None(_));
        assert_eq!(
            confirms,
            order_client_id_lookup_for(venue).looks_up,
            "{venue}: the confirm row and `order_client_id_lookup_for` disagree"
        );
    }
    let shared: BTreeSet<String> = CONFIRM_ROWS
        .iter()
        .filter(|(_, form)| matches!(form, ConfirmForm::Shared(_)))
        .map(|(venue, _)| venue.to_string())
        .collect();
    assert_eq!(
        bridges_whose_sources_name("ConfirmByOrderClientId"),
        shared,
        "the bridges that implement `ConfirmByOrderClientId` must be exactly the `Shared` rows"
    );
}

/// **Row 11's proofs exist.** Each test a row names is a `fn` of the file it names, so a renamed or deleted proof
/// reddens here instead of leaving the row claiming it.
#[test]
fn every_confirm_proof_names_a_test_that_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut missing = Vec::new();
    for (venue, form) in CONFIRM_ROWS {
        for proof in form.proofs() {
            let holds = std::fs::read_to_string(root.join(proof.file))
                .is_ok_and(|source| source.contains(&format!("fn {}(", proof.test)));
            if !holds {
                missing.push(format!("{venue}: `{}` holds no `fn {}`", proof.file, proof.test));
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}
