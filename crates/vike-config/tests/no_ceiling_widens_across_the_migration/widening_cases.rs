//! The gate, and the kill proofs that show it refuses each widening a reshape can hide.

use vike_secrets::Account;

use super::fixtures::{
    A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE, A_LABELLED_ACCOUNT_WITH_NO_LINE,
    A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE, Fixture, PROD2_SHAPE,
};
use super::judge::{
    Move, RULED, STRUCK, SUBJECT, UNCAPPED, moves, new_effective, objections, old_effective,
};

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

/// **THE GATE.** For every account on a store built from the fixtures: the new effective behaviour
/// is never ABOVE the old. Equality is the expectation; the assertion is the inequality because a
/// NARROWING is safe and a widening is not — and the row set is asserted separately, because a
/// fold that DROPS rows narrows them all and would otherwise pass.
///
/// ⚠ **[`PROD2_SHAPE`] alone cannot EXPRESS a widening, and running only it would be an assertion
/// that cannot fail for its stated reason.** A widening needs `tier > min(ceiling, tier)`, i.e.
/// `tier > ceiling` — and on that fixture every account's tier is at or below its venue's line, so
/// the property holds there for ANY assignment of `armed` whatsoever, including a deliberately
/// wrong one. The widening half is carried entirely by fixtures where a venue line sits BELOW an
/// account it covers, which is why [`A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE`] runs here too and why
/// [`the_struck_rule_from_the_signed_spec_is_caught_as_a_widening`] exists.
///
/// ⚠ **The subject is [`SUBJECT`] and NOT [`RULED`], and that is a finding rather than a
/// preference.** §5.2 step 5's venue-row-only wording widens a labelled account on
/// [`A_LABELLED_ACCOUNT_WITH_NO_LINE`], which is a legal store an operator produces by filing one
/// `__LABEL` credential key. Running the headline gate on [`RULED`] and omitting that fixture
/// would let stage 3 ship the venue-row-only fold with this gate green — the exact defect this
/// task found. So EVERY fixture in this file runs here, and the two rejected spellings are kill
/// proofs below.
#[test]
fn no_account_comes_out_of_the_migration_armed_higher_than_it_went_in() {
    for shape in [PROD2_SHAPE, A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE, A_LABELLED_ACCOUNT_WITH_NO_LINE] {
        let fx = Fixture::build(shape);
        let found = objections(&fx, &SUBJECT(&fx));
        assert!(
            found.is_empty(),
            "the 2->3 fold widens an account's ceiling:\n{}",
            found.join("\n")
        );
    }

    let (venues, accounts) = A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE;
    let fx = Fixture::build_with(venues, accounts);
    let found = objections(&fx, &SUBJECT(&fx));
    assert!(
        found.is_empty(),
        "the 2->3 fold widens an account whose own `[accounts]` line sits above its venue's:\n{}",
        found.join("\n")
    );
}

/// **A kill proof for the CRITICAL defect this gate shipped with**, and the reason [`went_in`]
/// exists.
///
/// The reshape hands back the rows it wrote, so it can rewrite them — and §5.2 step 5 DOES rewrite
/// `tier` on every row it copies (`sim` -> `paper`). A gate that recomputed the old ceiling from
/// the returned row would read a rewritten `tier = live` as *this row always could reach live* and
/// see no change at all. Here the subject rewrites `hyperliquid/demo` upward and arms it: it went
/// in at `live.cap(demo)` = `demo` and comes out `live`, and the gate must say so.
#[test]
fn a_reshape_that_rewrites_a_rows_tier_upward_cannot_hide_the_widening() {
    let fx = Fixture::build(PROD2_SHAPE);
    let target = fx.id_of("hyperliquid", "demo");
    let mut after = SUBJECT(&fx);
    for (account, armed) in &mut after {
        if account.id == target {
            account.tier = "live".to_string();
            *armed = true;
        }
    }

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains(&format!("account {target} ")) && f.contains("WIDENS")),
        "the old ceiling must be resolved from the row that WENT IN, or a reshape hides a \
         widening simply by rewriting the row it is judged on: {found:?}"
    );
    assert!(
        moves(&fx, &after).contains(&(target, Move::Widened)),
        "…and the equality pin must read it as WIDENED rather than as a narrowing"
    );

    // …and the defect is DEMONSTRATED rather than described: judged on the row handed back — the
    // spelling this file shipped with — the very same reshape raises nothing at all. Without this
    // line the assertion above would pass under both spellings and prove nothing about either.
    assert_eq!(
        judged_on_the_returned_row(&fx, &after),
        0,
        "recomputing the old ceiling from the AFTER row must be the thing that goes blind here; \
         if it now objects too, this kill proof has stopped exercising the fix"
    );
}

/// The comparison this file shipped with on `8b3fe904b`: old recomputed from the row the reshape
/// HANDED BACK. Kept as a measuring instrument so the kill proofs above can show the two spellings
/// disagree, rather than merely asserting that today's one is right.
fn judged_on_the_returned_row(fx: &Fixture, after: &[(Account, bool)]) -> usize {
    after
        .iter()
        .filter(|(a, armed)| new_effective(a, *armed) > old_effective(&fx.policy, a))
        .count()
}

/// **The same defect wearing the other column, which needs no tier rewrite at all.** §5.2 step 5
/// copies `label` too. Drop a labelled row's `label` and, judged on the after row, the account
/// resolves as its venue's DEFAULT — which inherits the venue line — so `paper` -> `live` reads as
/// no change.
#[test]
fn a_reshape_that_drops_a_rows_label_cannot_hide_the_widening() {
    let fx = Fixture::build(A_LABELLED_ACCOUNT_WITH_NO_LINE);
    let target = fx.id_of_label("binance", "ALT");

    let mut after = SUBJECT(&fx);
    for (account, armed) in &mut after {
        if account.id == target {
            account.label = None;
            *armed = true;
        }
    }

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains(&format!("account {target} ")) && f.contains("WIDENS")),
        "a labelled account went in at `paper` (it has no `[accounts]` line); a reshape that \
         drops the label and arms it comes out `live`, and the gate must say so: {found:?}"
    );

    // …and the same demonstration: on the returned row the account reads as its venue's DEFAULT,
    // which INHERITS the venue line, so the old spelling sees `live` -> `live` and says nothing.
    assert_eq!(
        judged_on_the_returned_row(&fx, &after),
        0,
        "the label column must be the thing that goes blind here; if it now objects too, this \
         kill proof has stopped exercising the fix"
    );
}

/// **A kill proof for the spec PROSE's own omission**, and the test that makes [`SUBJECT`]'s
/// answer on a stated labelled line demonstrably the rule rather than luck.
///
/// §5.2's amended wording says *read the LABELLED arming row* and does not mention the venue cap
/// `VenuePolicy::account` applies. On [`A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE`] —
/// `binance = "demo"` with `[accounts.binance] ALT = "live"` — that omission arms `ALT` to `live`
/// where the old model capped it to `demo`.
#[test]
fn the_uncapped_labelled_line_is_caught_as_a_widening() {
    let (venues, accounts) = A_LABELLED_ACCOUNT_OVER_ITS_VENUE_LINE;
    let fx = Fixture::build_with(venues, accounts);
    assert!(
        fx.arming.iter().any(|r| r.label.as_deref() == Some("ALT") && r.mode == "live"),
        "the fixture must put a STATED labelled arming row in the store — the one arm of \
         `VenuePolicy::account` no other fixture here reaches: {:?}",
        fx.arming
    );
    let alt = fx.id_of_label("binance", "ALT");

    let found = objections(&fx, &UNCAPPED(&fx));
    assert!(
        found.iter().any(|f| f.contains(&format!("account {alt} ")) && f.contains("WIDENS")),
        "reading the labelled arming row WITHOUT the venue cap arms an account the old model \
         capped on read, and the gate must say so: {found:?}"
    );

    // …and the SHIPPED fold, which applies the cap, does not.
    //
    // ⚠ The `armed` BIT is asserted directly rather than inferred from the absence of an
    // objection. The two are equivalent HERE — arming this row would widen, so an empty objection
    // list implies a `false` bit — but the message above says *it disarms this account*, and a
    // test whose message names a stronger fact than its assertion is the shape that goes quietly
    // wrong when a fixture moves. Carried from Task 4's deferred Minor M-b, which was deferred
    // precisely because `SUBJECT` was a model and this is now the column.
    let armed_alt = SUBJECT(&fx)
        .into_iter()
        .find(|(a, _)| a.id == alt)
        .map(|(_, armed)| armed)
        .expect("the ALT row is in the fold's output");
    assert!(
        !armed_alt,
        "the shipped fold caps the stated `ALT = \"live\"` line by its venue's `demo` line, so \
         this account must come out DISARMED — `armed = true` here is the widening `UNCAPPED` \
         above was just refused for"
    );
    assert!(
        objections(&fx, &SUBJECT(&fx)).is_empty(),
        "…and nothing else on this store widens either"
    );
}

/// **…and that claim is MEASURED rather than asserted.** Arm EVERY row of the the CI box fixture — the
/// most aggressive assignment there is, and one no rule would produce — and the widening half
/// still raises nothing, because every account's tier already sits at or below its venue's line.
///
/// This is here so a reader does not mistake the the CI box fixture for the thing that guards against
/// widening. It guards the row set and the EQUALITY; the adversarial fixtures guard the
/// inequality.
#[test]
fn arming_every_row_of_the_prod2_fixture_still_widens_nothing() {
    let fx = Fixture::build(PROD2_SHAPE);
    let all_armed: Vec<(Account, bool)> = fx.accounts.iter().map(|a| (a.clone(), true)).collect();
    let found = objections(&fx, &all_armed);
    assert!(
        found.is_empty(),
        "the the CI box shape was believed unable to express a widening and it just did — re-read the \
         fixture before trusting anything else in this file: {found:?}"
    );
}

/// **The the CI box measurement, as a named expectation** — §5.2 step 5, MEASURED 2026-09-23. A
/// different answer on this shape is a defect, not a surprise.
///
/// ⚠ The spec's *"no row changes behaviour"* is a claim about the DEPLOYED behaviour and is true
/// there: hyperliquid's demo account is not trading today either, because one process mounts a
/// venue at one tier (`crates/vike-mount/src/arming.rs`'s `account_route_key` carries that sentence)
/// and this box mounts hyperliquid at live. It is NOT true of the per-row ceiling arithmetic this
/// gate can compute, where that row goes `demo` -> `paper`. So the pin below is 15 rows UNCHANGED
/// and exactly ONE narrowing, NAMED — which is strictly stronger than pinning 16 unchanged would
/// have been, because it refuses any OTHER row moving at all.
#[test]
fn the_prod2_shape_comes_out_with_fifteen_armed_and_exactly_one_named_narrowing() {
    let fx = Fixture::build(PROD2_SHAPE);
    assert_eq!(fx.accounts.len(), 16, "16 account rows: {:?}", fx.accounts);
    assert_eq!(
        fx.arming.iter().filter(|r| r.label.is_none()).count(),
        14,
        "14 venue lines — a declared `[venues]` table mirrors ROSTER-COMPLETE: {:?}",
        fx.arming
    );

    let after = SUBJECT(&fx);
    assert_eq!(after.len(), 16, "…and 16 come out");
    assert_eq!(after.iter().filter(|(_, armed)| *armed).count(), 15, "15 rows arm");

    let hyperliquid_demo = fx.id_of("hyperliquid", "demo");
    let disarmed: Vec<i64> = after.iter().filter(|(_, armed)| !*armed).map(|(a, _)| a.id).collect();
    assert_eq!(
        disarmed,
        vec![hyperliquid_demo],
        "the one row that does NOT arm is hyperliquid's demo account — the venue is mounted at \
         live, so its demo credential set is what the operator's own mode did not name"
    );

    let how = moves(&fx, &after);
    let narrowed: Vec<i64> =
        how.iter().filter(|(_, m)| *m == Move::Narrowed).map(|(id, _)| *id).collect();
    assert_eq!(
        narrowed,
        vec![hyperliquid_demo],
        "exactly one row's effective behaviour moves, and it is the same row: {how:?}"
    );
    assert_eq!(
        how.iter().filter(|(_, m)| *m == Move::Unchanged).count(),
        15,
        "…and the other 15 come out IDENTICAL, which is the equality §5.2 step 5 expects: {how:?}"
    );

    assert!(objections(&fx, &after).is_empty(), "…and nothing widens");
}

/// **A kill proof for the row-set half.** A reshape that forgets an account row narrows it to
/// `paper` — safe by the inequality, and a silent loss of the row the operator armed. The gate
/// must refuse it on the row set alone.
#[test]
fn a_reshape_that_drops_an_account_row_is_refused() {
    let fx = Fixture::build(PROD2_SHAPE);
    let dropped = fx.id_of("binance", "live");
    let after: Vec<(Account, bool)> =
        SUBJECT(&fx).into_iter().filter(|(a, _)| a.id != dropped).collect();

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains("did not preserve the account rows")),
        "a fold that loses a row must be refused on the ROW SET, since the inequality reads it as \
         a narrowing: {found:?}"
    );
}

/// **…and the count alone is not enough.** A reshape that drops one row and duplicates another
/// has the right count and the wrong rows.
#[test]
fn a_reshape_that_duplicates_a_row_in_place_of_another_is_refused() {
    let fx = Fixture::build(PROD2_SHAPE);
    let dropped = fx.id_of("binance", "live");
    let mut after: Vec<(Account, bool)> =
        SUBJECT(&fx).into_iter().filter(|(a, _)| a.id != dropped).collect();
    let twin = after[0].clone();
    after.push(twin);
    assert_eq!(after.len(), fx.accounts.len(), "the COUNT is right, which is the point");

    let found = objections(&fx, &after);
    assert!(
        found.iter().any(|f| f.contains("did not preserve the account rows")),
        "the row set is compared as a multiset of ids, not as a count: {found:?}"
    );
}

/// **A kill proof for the inequality half, and the reason §5.2 step 5 was rewritten.**
///
/// The wording this spec was SIGNED with — *`armed = 1` where the old effective ceiling was above
/// `paper`* — composed with §3.2's `armed ? tier : paper` arms a `tier = live` account whose venue
/// line says `demo`: old effective `demo`, new effective `live`. A widening, in the one step
/// annotated as the one that must not widen. The gate catches it.
#[test]
fn the_struck_rule_from_the_signed_spec_is_caught_as_a_widening() {
    let fx = Fixture::build(A_LIVE_ACCOUNT_UNDER_A_DEMO_LINE);
    let live_row = fx.id_of("bybit", "live");

    let found = objections(&fx, &STRUCK(&fx));
    assert!(
        found.iter().any(|f| f.contains(&format!("account {live_row} ")) && f.contains("WIDENS")),
        "the struck rule arms bybit's live account under a demo line and the gate must say so: \
         {found:?}"
    );

    // …and the rule that replaced it does not.
    assert!(
        objections(&fx, &RULED(&fx)).is_empty(),
        "the RULED rule arms only where the operator's own mode named the account's tier"
    );
}
