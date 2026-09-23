//! §7.5's DECLARATION gate: the wire's lane admission is the declared capability matrix, over the
//! whole `vike_model::VENUES` roster.
//!
//! Feature-free and roster-exhaustive, so a NEW venue reddens it — the workspace's standing roster
//! rule, and the same shape every per-venue capability table already uses. It is placed in the LIGHT
//! crate deliberately: it needs no server, no feature and no network, so it fires on every PR
//! regardless of how the `live-feeds` suite arm is spelled.
//!
//! This gate covers the ADMISSION decision. The LABEL — that a Depth subscription's frames never
//! decode as `MdFrame::Book` — is a different claim on a different path, and
//! `crates/vike-datahub/tests/md_hub.rs`'s
//! `a_conflating_lane_never_wears_the_lossless_lanes_name` is where it lives.

use vike_datahub_client::market::MdLane;

const LANES: [MdLane; 3] = [MdLane::Depth, MdLane::Book, MdLane::Trades];

/// The wire's admission decision for `(venue, lane)` **equals** the declared matrix's, for every
/// roster venue and every lane.
///
/// It is not a tautology today, and the next test proves that rather than asserting it.
#[test]
fn the_wires_lane_admission_is_the_declared_matrix() {
    for venue in vike_model::VENUES {
        for lane in LANES {
            let declared = vike_data::require_live_verb(venue, lane.live_verb()).is_ok();
            let declared_again = vike_model::caps_for(venue).live_data.supports(lane.live_verb());
            assert_eq!(
                declared, declared_again,
                "`{venue}` / {lane:?}: the wire's authority (`require_live_verb`) and the caps row \
                 it reads must be one answer"
            );
        }
    }
}

/// ⚠ **THE NON-VACUITY FLOOR, WITH TEETH.** All four cells of the matrix must be POPULATED by the
/// roster: a lane some venue serves and some venue refuses, in both directions.
///
/// A gate that always-admits and a gate that always-refuses both pass a per-row equality check. If
/// the caps table ever changes so that every servable venue serves both book lanes, this gate
/// becomes a tautology — and it should SAY SO OUT LOUD rather than keep passing. This is
/// `crates/vike-ops/tests/graceful_stop_pin.rs`'s `the_pin_has_a_non_empty_input` shape, applied to
/// a matrix.
#[test]
fn the_roster_populates_every_cell_of_the_lane_matrix() {
    let mut book_yes = 0;
    let mut book_no = 0;
    let mut depth_yes = 0;
    let mut depth_no = 0;
    for venue in vike_model::VENUES {
        if vike_data::require_live_verb(venue, MdLane::Book.live_verb()).is_ok() {
            book_yes += 1;
        } else {
            book_no += 1;
        }
        if vike_data::require_live_verb(venue, MdLane::Depth.live_verb()).is_ok() {
            depth_yes += 1;
        } else {
            depth_no += 1;
        }
    }
    assert!(
        book_yes > 0 && book_no > 0,
        "book: {book_yes} yes / {book_no} no — the gate is vacuous"
    );
    assert!(
        depth_yes > 0 && depth_no > 0,
        "depth: {depth_yes} yes / {depth_no} no — the gate is vacuous"
    );
}

/// The PARTITION the initial venue set lands on, pinned as a fact rather than described in prose:
/// on the six venues the market-data plane starts with, **no venue serves both book lanes**.
///
/// This is what gives §7.5's gate teeth from the first commit, and it is the thing a reader is most
/// likely to get wrong — `Depth` and `Book` share one payload struct, so they LOOK interchangeable.
#[test]
fn the_start_narrow_set_partitions_the_two_book_lanes() {
    for venue in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
        assert!(
            vike_data::require_live_verb(venue, MdLane::Depth.live_verb()).is_ok(),
            "`{venue}` serves the CONFLATING depth lane"
        );
        assert!(
            vike_data::require_live_verb(venue, MdLane::Book.live_verb()).is_err(),
            "`{venue}` declares NO lossless book lane — a lossless binance L2 book is unobtainable \
             in this workspace by any route, and letting the conflating lane wear the book's name \
             is what lets a maker-fill backtest report fills it could never have got"
        );
    }
    assert!(vike_data::require_live_verb("polymarket", MdLane::Book.live_verb()).is_ok());
    assert!(vike_data::require_live_verb("polymarket", MdLane::Depth.live_verb()).is_err());
}

/// Every refusal the wire can forward is the matrix's OWN `&'static str`, so the two cannot drift.
/// The message must also name the lane it refused, or an operator reading a per-venue status string
/// learns only that something was unsupported.
#[test]
fn a_refusal_carries_the_matrixs_own_words() {
    for venue in vike_model::VENUES {
        for lane in LANES {
            if let Err(e) = vike_data::require_live_verb(venue, lane.live_verb()) {
                let msg = e.to_string();
                assert!(
                    msg.contains("VenueCaps.live_data"),
                    "`{venue}` / {lane:?}: the refusal must cite the declared matrix: {msg}"
                );
            }
        }
    }
}
