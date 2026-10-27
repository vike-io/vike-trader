//! Process-exclusive rows: the claim, the declaration lookups, a shared resource, rows that yield.
use super::*;

// Planted on `fxcm`, not `dukascopy`, under a resource no other test claims: the claim set is
// process-global and keyed on the resource, so a parallel test cannot hold it.
static EXCLUSIVE: Planted = Planted {
    exclusive: true,
    resource: Some("the claim-release test's resource"),
    ..Planted::new(
        "fxcm",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        false,
    )
};
static EXCLUSIVE_REG: [VenueRow; 1] = [VenueRow::Adapter(&EXCLUSIVE)];

/// Review Focus 3: an armed start that builds no client (and so does not run) releases its claim,
/// so the next start reaches the bridge again.
#[test]
fn the_fold_keeps_the_claim_only_for_a_live_outcome() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("fxcm", AccountTier::Demo);
    for _ in 0..2 {
        let _ = start_one(
            &EXCLUSIVE_REG,
            "fxcm",
            "EURUSD",
            &tx,
            &mut live,
            Some(&budget()),
            Some(&armed),
        );
    }
    assert_eq!(
        EXCLUSIVE.mounts.load(Ordering::SeqCst),
        2,
        "a released claim lets the next mount in"
    );
}

// The claim-KEEPING half: a kept claim lasts as long as the started engine, so its resource is shared with no
// other row.
static EXCLUSIVE_LIVE: Planted = Planted {
    exclusive: true,
    resource: Some("the claim-keeping test's resource"),
    ..Planted::new("ig", Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }, true)
};
static EXCLUSIVE_LIVE_REG: [VenueRow; 1] = [VenueRow::Adapter(&EXCLUSIVE_LIVE)];

/// …and the other half: a LIVE outcome KEEPS the claim while its engine lives, so a second start is
/// refused before its bridge is asked and does NOT RUN (`AdapterFailed`) — never paper in its place.
#[test]
fn a_live_exclusive_outcome_keeps_the_claim_and_refuses_the_next_start() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let armed = policy("ig", AccountTier::Demo);
    let mut first = HashSet::new();
    let running = start_one(
        &EXCLUSIVE_LIVE_REG,
        "ig",
        "EURUSD",
        &tx,
        &mut first,
        Some(&budget()),
        Some(&armed),
    )
    .unwrap_or_else(|e| panic!("the first mount starts: {e}"));
    assert!(first.contains("ig"), "the first mount went live and kept the claim");
    let mut second = HashSet::new();
    let stopped = not_running(start_one(
        &EXCLUSIVE_LIVE_REG,
        "ig",
        "EURUSD",
        &tx,
        &mut second,
        Some(&budget()),
        Some(&armed),
    ));
    assert_eq!(stopped.block, ArmingBlock::AdapterFailed, "the second mount does not run");
    assert!(second.is_empty(), "the second mount is not live");
    assert_eq!(EXCLUSIVE_LIVE.mounts.load(Ordering::SeqCst), 1, "the kept claim refused it first");
    // The claim ends with the adapter: the engine goes, and the next start runs.
    drop(running);
    let mut third = HashSet::new();
    let _again = start_one(
        &EXCLUSIVE_LIVE_REG,
        "ig",
        "EURUSD",
        &tx,
        &mut third,
        Some(&budget()),
        Some(&armed),
    )
    .unwrap_or_else(|e| panic!("the start after the adapter ended runs: {e}"));
    assert_eq!(EXCLUSIVE_LIVE.mounts.load(Ordering::SeqCst), 2, "…and reached the bridge");
}

// ---- the lookups that answer a contract row from its declaration --------------------------------

/// A book identity no legacy table carries for this id.
const PLANTED_BOOK: BookIdentity =
    BookIdentity::Named { name_fields: &["ACCOUNT_ID"], evm_key_fields: &[] };

/// A contract row that is nothing but its declaration.
static DECLARES: PlantedAdapter = PlantedAdapter {
    declaration: VenueDeclaration {
        grid_source: DeclaredGridSource::PerSymbolFetch,
        book_identity: PLANTED_BOOK,
        ..PLANTED_DECLARATION
    },
    ..PlantedAdapter::new("planted", Resolution::Paper(PaperCause::NoLiveArm))
};
static DECLARES_REG: [VenueRow; 1] = [VenueRow::Adapter(&DECLARES)];

/// `book_identity_for` and `declared_grid_source` answer from the declaration: no legacy table
/// carries `planted`, so a fall-through would answer `None` and `NoGrid`.
#[test]
fn a_contract_rows_book_identity_and_grid_source_come_from_its_declaration() {
    assert_eq!(
        crate::trade::adapters::book_identity::book_identity_for(&DECLARES_REG, "planted"),
        Some(PLANTED_BOOK)
    );
    assert_eq!(
        crate::trade::adapters::symbol_grid::declared_grid_source(&DECLARES_REG, "planted"),
        DeclaredGridSource::PerSymbolFetch
    );
}

// ---- the claim is keyed on the RESOURCE, not the venue ------------------------------------------

const SHARED: &str = "a resource two planted venues share";

static SHARING_FIRST: Planted = Planted {
    exclusive: true,
    resource: Some(SHARED),
    ..Planted::new(
        "alpaca",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        true,
    )
};
static SHARING_SECOND: Planted = Planted {
    exclusive: true,
    resource: Some(SHARED),
    ..Planted::new(
        "oanda",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        true,
    )
};
static SHARING_REG: [VenueRow; 2] =
    [VenueRow::Adapter(&SHARING_FIRST), VenueRow::Adapter(&SHARING_SECOND)];

/// Two venues that declare ONE resource exclude each other: the first to go live keeps the claim,
/// and the second is refused before its bridge is asked — it does NOT RUN (`AdapterFailed`).
#[test]
fn two_venues_that_declare_one_resource_exclude_each_other() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let both = vike_tradehub_core::AdapterPolicy::default()
        .with_account("alpaca", &AccountLabel::Default, AccountTier::Demo)
        .with_account("oanda", &AccountLabel::Default, AccountTier::Demo);
    let mut live = HashSet::new();
    let _first =
        start_one(&SHARING_REG, "alpaca", "AAPL", &tx, &mut live, Some(&budget()), Some(&both))
            .unwrap_or_else(|e| panic!("the first venue mounts: {e}"));
    let stopped = not_running(start_one(
        &SHARING_REG,
        "oanda",
        "EURUSD",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&both),
    ));
    assert_eq!(stopped.block, ArmingBlock::AdapterFailed, "the second venue does not run");
    assert!(live.contains("alpaca"), "the first venue went live and kept the shared resource");
    assert!(!live.contains("oanda"), "the second venue is refused: its resource is taken");
    assert_eq!(SHARING_SECOND.mounts.load(Ordering::SeqCst), 0, "…before its bridge is asked");
}

// ---- the exclusive branches that start nothing --------------------------------------------------

static YIELDS: Planted = Planted {
    exclusive: true,
    resource: Some("the yielding test's resource"),
    ..Planted::new(
        "hyperliquid",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        true,
    )
};
static YIELDS_REG: [VenueRow; 1] = [VenueRow::Adapter(&YIELDS)];

/// `SidecarHeldElsewhere`: with an ACTIVE labelled account row, the DEFAULT account yields and does
/// NOT RUN with that reason; the fold renders the bridge's `held_by_another` with the holder and
/// never asks it to start.
#[test]
fn an_account_that_yields_the_resource_is_refused_with_its_holder_and_never_started() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let named = vike_tradehub_core::AdapterPolicy::default()
        .with_account("hyperliquid", &AccountLabel::Default, AccountTier::Demo)
        .with_account("hyperliquid", &alt, AccountTier::Demo);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let stopped = not_running(start_one(
        &YIELDS_REG,
        "hyperliquid",
        "BTC",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&named),
    ));
    assert_eq!(
        (stopped.tier, stopped.block),
        (Some(AccountTier::Demo), ArmingBlock::SidecarHeldElsewhere)
    );
    assert!(live.is_empty(), "the default account yields");
    assert_eq!(
        YIELDS.mounts.load(Ordering::SeqCst),
        0,
        "the yielding account never reaches the bridge"
    );
    let expected = format!("{} yields to ALT", AccountLabel::Default);
    assert!(
        HELD_BY_ANOTHER.lock().expect("the planted log").contains(&expected),
        "the refusal renders the holder the account table named: `{expected}`"
    );
}

const PAPER_RESOURCE: &str = "the paper-answer test's resource";

static PAPER_EXCLUSIVE: Planted = Planted {
    exclusive: true,
    resource: Some(PAPER_RESOURCE),
    ..Planted::new("deribit", Resolution::Paper(PaperCause::NoCredentials), false)
};
static PAPER_EXCLUSIVE_REG: [VenueRow; 1] = [VenueRow::Adapter(&PAPER_EXCLUSIVE)];

/// A PAPER answer on an exclusive row does NOT RUN, for the bridge's own reason, and never touches
/// the claim: the resource is held elsewhere throughout, yet the account reads `NoCredentials`
/// (not a claim refusal), and the bridge is never asked to start — before decision 0156 it was,
/// to build a paper engine in the account's place.
#[test]
fn a_paper_answer_on_an_exclusive_row_does_not_run_without_touching_the_claim() {
    let held = crate::trade::adapters::exclusive::claim(PAPER_RESOURCE)
        .expect("the test holds the resource");
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let stopped = not_running(start_one(
        &PAPER_EXCLUSIVE_REG,
        "deribit",
        "BTC-PERPETUAL",
        &tx,
        &mut live,
        None,
        Some(&policy("deribit", AccountTier::Demo)),
    ));
    assert_eq!(stopped.block, ArmingBlock::NoCredentials, "the bridge's reason, not the claim's");
    assert_eq!(PAPER_EXCLUSIVE.mounts.load(Ordering::SeqCst), 0, "the bridge is never asked");
    assert!(live.is_empty());
    assert!(
        crate::trade::adapters::exclusive::claim(PAPER_RESOURCE).is_none(),
        "the test still holds the claim"
    );
    drop(held);
}
