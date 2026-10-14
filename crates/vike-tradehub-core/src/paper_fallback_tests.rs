//! The paper book's HALT arming and its slippage, asserted on the CONSTRUCTED book.

use crate::AdapterPolicy;
use crate::policy::DEFAULT_PAPER_SLIPPAGE;

/// Every paper book `make_engine` builds — a `paper`-tier account's simulator, the one place a
/// paper engine is still built (decision 0156) — arms the operator HALT sentinel.
///
/// A paper book is a MOUNT, so `touch <project>/settings/state/HALT` must reach it. Before
/// `paper_client`, eleven paper arms armed nothing: an all-paper daemon silently observed no HALT
/// file.
///
/// ⚠ Asserts on the CONSTRUCTED book, not the source text: a text gate cannot tell an armed
/// construction from an unarmed one. Drop `.with_halt_path(..)` from `paper_client` -> red.
#[test]
fn the_paper_fallback_every_venue_arm_uses_is_halt_armed() {
    let client = super::paper_client(
        "binance",
        "BTCUSDT",
        DEFAULT_PAPER_SLIPPAGE,
        vike_model::FeeSchedule::Free,
    );
    assert!(
        client.halt_path().is_some(),
        "a paper-tier account's book must arm the HALT sentinel — an operator rehearses the \
         kill switch on exactly this mount"
    );
}

/// **A box without the `config.paper_slippage` row builds the book it always built**: no policy at
/// all, and a policy that carries no row, both give the compiled default — `0.0002`, the literal the
/// mount hard-coded — and the book is still HALT-armed.
#[test]
fn a_paper_book_without_the_row_keeps_the_compiled_slippage() {
    assert_eq!(DEFAULT_PAPER_SLIPPAGE, 0.0002, "the figure the mount shipped with");
    let no_policy =
        super::paper_book_for("binance", "BTCUSDT", None, vike_model::FeeSchedule::Free);
    assert_eq!(no_policy.slippage, DEFAULT_PAPER_SLIPPAGE);

    let no_row = AdapterPolicy::default();
    assert_eq!(no_row.paper_slippage, None, "the default policy carries no row");
    let book =
        super::paper_book_for("binance", "BTCUSDT", Some(&no_row), vike_model::FeeSchedule::Free);
    assert_eq!(book.slippage, DEFAULT_PAPER_SLIPPAGE);
    assert!(book.halt_path().is_some(), "the default book is HALT-armed as before");
}

/// **The row reaches the book**: whatever the policy carries is the slippage the paper client
/// fills with, `0` (no slippage at all) and a large figure included, and the book is still
/// HALT-armed. Fees are another parameter: the schedule handed in is the schedule the book keeps.
#[test]
fn the_policy_row_is_the_slippage_the_paper_book_fills_with() {
    for row in [0.0, 0.0001, 0.001, 0.05] {
        let policy = AdapterPolicy { paper_slippage: Some(row), ..AdapterPolicy::default() };
        let book = super::paper_book_for(
            "binance",
            "BTCUSDT",
            Some(&policy),
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(book.slippage, row, "the row {row} is the book's slippage");
        assert!(book.halt_path().is_some(), "{row}: the HALT arming is not the row's to drop");
    }
}
