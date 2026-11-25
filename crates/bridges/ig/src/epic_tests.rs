//! The user name <-> epic pair over the operator's kept rows (`super::kept_list_for_tests`), and its refusals.

use vike_model::{Contract, ContractFx, Instrument};

use super::*;

/// `EURUSD` is the epic its kept row states (the Mini), typed in any case, and that epic is `EURUSD` again; the
/// standard lot is `EURUSD.CFD` both ways (decision 0290 item 6, 2026-11-17).
#[test]
fn a_user_name_is_the_epic_of_its_kept_row_and_back() {
    let rows = kept_list_for_tests();
    for typed in ["EURUSD", "eurusd", " EURUSD "] {
        let names = names_in(&rows, typed).expect("a kept row");
        assert_eq!((names.user.as_str(), names.wire.as_str()), ("EURUSD", "CS.D.EURUSD.MINI.IP"));
    }
    for typed in ["EURUSD.CFD", "eurusd.cfd"] {
        let names = names_in(&rows, typed).expect("the standard lot's row");
        assert_eq!(
            (names.user.as_str(), names.wire.as_str()),
            ("EURUSD.CFD", "CS.D.EURUSD.CFD.IP")
        );
    }
    assert_eq!(user_name_in(&rows, "CS.D.USDJPY.MINI.IP").as_deref(), Some("USDJPY"));
    assert_eq!(user_name_in(&rows, "CS.D.EURUSD.CFD.IP").as_deref(), Some("EURUSD.CFD"));
    assert_eq!(
        user_name_in(&rows, "CS.D.USDJPY.CFD.IP"),
        None,
        "another epic of the pair is no row"
    );
}

/// No list, no row, the old epic spelling: each refused in words, the first naming the operator's fix (IG's list is
/// read over a running account's session since IG-LIST, so the fix is the refresh).
#[test]
fn a_name_with_no_row_or_no_list_is_refused_in_words() {
    let why = names_in(&[], "EURUSD").expect_err("no list");
    assert!(why.contains("vike-cli data catalog refresh --venue ig"), "{why}");
    let rows = kept_list_for_tests();
    let why = names_in(&rows, "AUDNZD").expect_err("no row");
    assert!(why.contains("AUDNZD") && why.contains("ig"), "{why}");
    let why = names_in(&rows, "CS.D.EURUSD.MINI.IP").expect_err("an epic is no user name");
    assert!(why.contains("CS.D.EURUSD.MINI.IP"), "{why}");
}

/// A kept row whose own name is not an IG epic (three segments) is refused by the one shape rule before anything
/// could be sent with it.
#[test]
fn a_row_whose_own_name_is_no_epic_is_refused() {
    let fx = Contract::Fx(ContractFx::default());
    let rows = vike_catalog::spelled(vec![Instrument::new("ig", "CS.D.EURUSD", "EUR", "USD", fx)]);
    let why = names_in(&rows, "EURUSD").expect_err("not an epic");
    assert!(why.contains("CS.D.EURUSD") && why.contains("is not an epic"), "{why}");
}

/// **A kept row's epic is held to the ONE epic shape** (IG-2, `vike_catalog::is_ig_epic`, which the history lane and
/// the order read too): five dotted segments pass, three, four or six are refused before anything is sent.
#[test]
fn a_kept_epic_is_held_to_the_one_epic_shape() {
    for (epic, admitted) in [
        ("CS.D.EURUSD.MINI.IP", true),
        ("CS.D.EURUSD", false),
        ("CS.D.EURUSD.MINI", false),
        ("CS.D.EURUSD.MINI.IP.X", false),
    ] {
        let rows = vike_catalog::spelled(vec![Instrument::new(
            "ig",
            epic,
            "EUR",
            "USD",
            Contract::Fx(ContractFx::default()),
        )]);
        assert_eq!(vike_catalog::is_ig_epic(epic), admitted, "{epic}");
        match names_in(&rows, "EURUSD") {
            Ok(names) => assert!(admitted && names.wire == epic, "{epic} was admitted"),
            Err(why) => assert!(!admitted && why.contains("is not an epic"), "{epic}: {why}"),
        }
    }
}

/// Over the process's held list: the epic is answered and remembered, and an epic no row names is booked under itself.
#[test]
fn the_held_list_answers_both_ways() {
    hold_kept_list_for_tests();
    assert_eq!(epic_of("GBPUSD").as_deref(), Ok("CS.D.GBPUSD.MINI.IP"));
    assert_eq!(user_name_or_epic("CS.D.GBPUSD.MINI.IP"), "GBPUSD");
    assert_eq!(epic_of("EURUSD.CFD").as_deref(), Ok("CS.D.EURUSD.CFD.IP"));
    assert_eq!(user_name_or_epic("CS.D.EURUSD.CFD.IP"), "EURUSD.CFD");
    assert_eq!(
        user_name_or_epic("IX.D.FTSE.CFD.IP"),
        "IX.D.FTSE.CFD.IP",
        "no row: the epic itself"
    );
    assert_eq!(user_name_or_epic(""), "");
}

/// **`EURUSD` on IG is the Mini and `EURUSD.CFD` its standard lot** (the owner's ruling of 2026-11-17, replacing that of
/// 2026-11-16) on a list holding both and a knock-out: the writer keeps the naming IG's rows state
/// (`vike_catalog::spelled`), so the standard lot, whose row states the pair too, is no second `EURUSD`. A lookup of the
/// knock-out's epic is refused in words, and the way back names nothing for it.
///
/// MUTATION PROOF: let the writer spell over a stated naming and `EURUSD` names two rows.
#[test]
fn eurusd_is_the_mini_and_eurusd_cfd_its_standard_lot() {
    let row = |epic: &str, market: &str| {
        let pick = crate::operator_row::Pick {
            epic: epic.into(),
            name: String::new(),
            passed_over: Vec::new(),
        };
        let market = serde_json::from_str(market).expect("the capture parses");
        crate::operator_row::describe(&pick, &market).expect("the operator's row")
    };
    let mini = row(
        "CS.D.EURUSD.MINI.IP",
        include_str!("../tests/fixtures/markets/CS.D.EURUSD.MINI.IP.json"),
    );
    let standard = row(
        "CS.D.EURUSD.CFD.IP",
        include_str!("../tests/fixtures/markets/CS.D.EURUSD.CFD.IP.json"),
    );
    let knock_out =
        crate::catalog::row_of("CS.D.EURUSD.CFDBULL.IP", "KNOCKOUTS", Some("-"), "EUR/USD Bull KO");
    let rows = vike_catalog::spelled(vec![mini, standard, knock_out]);
    let names = names_in(&rows, "EURUSD").expect("the Mini");
    assert_eq!((names.user.as_str(), names.wire.as_str()), ("EURUSD", "CS.D.EURUSD.MINI.IP"));
    let names = names_in(&rows, "EURUSD.CFD").expect("the standard lot");
    assert_eq!((names.user.as_str(), names.wire.as_str()), ("EURUSD.CFD", "CS.D.EURUSD.CFD.IP"));
    let why = names_in(&rows, "cs.d.eurusd.cfdbull.ip").expect_err("a knock-out has no name");
    assert!(why.contains("knock-out") && why.contains("CS.D.EURUSD.CFDBULL.IP"), "{why}");
    assert_eq!(user_name_in(&rows, "CS.D.EURUSD.CFDBULL.IP"), None);
}
