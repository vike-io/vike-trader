//! Process-exclusive rows: the claim, the declaration lookups, a shared resource, rows that yield.
use super::*;

// Planted on `fxcm`, not `dukascopy`, under a resource no other test claims: the claim set is
// process-global and keyed on the resource, so a parallel test cannot hold it.
static EXCLUSIVE: Planted = Planted {
    exclusive: true,
    resource: Some("the claim-release test's resource"),
    ..Planted::new("fxcm", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, false)
};
static EXCLUSIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&EXCLUSIVE)];

/// Review Focus 3: an armed mount that ends PAPER releases its claim, so the next mount reaches
/// the bridge again.
#[test]
fn the_fold_keeps_the_claim_only_for_a_live_outcome() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("fxcm", VenueMode::Demo);
    for _ in 0..2 {
        let _ = mount_one(
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

// The claim-KEEPING half: a kept claim lasts the test process, so its resource is shared with no
// other row.
static EXCLUSIVE_LIVE: Planted = Planted {
    exclusive: true,
    resource: Some("the claim-keeping test's resource"),
    ..Planted::new("ig", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true)
};
static EXCLUSIVE_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&EXCLUSIVE_LIVE)];

/// …and the other half: a LIVE outcome KEEPS the claim, so a second mount in the same process is
/// refused before its bridge is asked and lands on paper.
#[test]
fn a_live_exclusive_outcome_keeps_the_claim_and_refuses_the_next_mount() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let armed = policy("ig", VenueMode::Demo);
    let mut first = HashSet::new();
    mount_one(&EXCLUSIVE_LIVE_REG, "ig", "EURUSD", &tx, &mut first, Some(&budget()), Some(&armed))
        .expect("the first mount starts");
    assert!(first.contains("ig"), "the first mount went live and kept the claim");
    let mut second = HashSet::new();
    mount_one(&EXCLUSIVE_LIVE_REG, "ig", "EURUSD", &tx, &mut second, Some(&budget()), Some(&armed))
        .expect("a refused claim is paper, not an error");
    assert!(second.is_empty(), "the second mount is paper");
    assert_eq!(EXCLUSIVE_LIVE.mounts.load(Ordering::SeqCst), 1, "the kept claim refused it first");
}

// ---- the lookups that answer a contract row from its declaration --------------------------------

/// A book identity no legacy table carries for this id.
const PLANTED_BOOK: BookIdentity = BookIdentity::Named {
    prefix: "PLANTED",
    demo_tiers: &["DEMO"],
    live_tiers: &["LIVE"],
    name_suffixes: &["ACCOUNT_ID"],
    evm_key_suffixes: &[],
};

/// A contract row that is nothing but its declaration.
static DECLARES: PlantedMount = PlantedMount {
    declaration: VenueDeclaration {
        grid_source: DeclaredGridSource::PerSymbolFetch,
        book_identity: PLANTED_BOOK,
        ..PLANTED_DECLARATION
    },
    ..PlantedMount::new("planted", Resolution::Paper(PaperCause::NoLiveArm))
};
static DECLARES_REG: [VenueRow; 1] = [VenueRow::Mount(&DECLARES)];

/// `book_identity_for` and `declared_grid_source` answer from the declaration: no legacy table
/// carries `planted`, so a fall-through would answer `None` and `NoGrid`.
#[test]
fn a_contract_rows_book_identity_and_grid_source_come_from_its_declaration() {
    assert_eq!(
        crate::book_identity::book_identity_for(&DECLARES_REG, "planted"),
        Some(PLANTED_BOOK)
    );
    assert_eq!(
        crate::symbol_grid::declared_grid_source(&DECLARES_REG, "planted"),
        DeclaredGridSource::PerSymbolFetch
    );
}

// ---- the claim is keyed on the RESOURCE, not the venue ------------------------------------------

const SHARED: &str = "a resource two planted venues share";

static SHARING_FIRST: Planted = Planted {
    exclusive: true,
    resource: Some(SHARED),
    ..Planted::new("alpaca", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true)
};
static SHARING_SECOND: Planted = Planted {
    exclusive: true,
    resource: Some(SHARED),
    ..Planted::new("oanda", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true)
};
static SHARING_REG: [VenueRow; 2] =
    [VenueRow::Mount(&SHARING_FIRST), VenueRow::Mount(&SHARING_SECOND)];

/// Two venues that declare ONE resource exclude each other: the first to go live keeps the claim,
/// and the second is refused before its bridge is asked.
#[test]
fn two_venues_that_declare_one_resource_exclude_each_other() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let both = crate::MountPolicy {
        venues: VenuePolicy::default()
            .declare("alpaca", VenueMode::Demo)
            .declare("oanda", VenueMode::Demo),
        ..Default::default()
    };
    let mut live = HashSet::new();
    for (venue, symbol) in [("alpaca", "AAPL"), ("oanda", "EURUSD")] {
        mount_one(&SHARING_REG, venue, symbol, &tx, &mut live, Some(&budget()), Some(&both))
            .expect("a refused claim is paper, not an error");
    }
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
        Resolution::Armed { tier: Tier::Demo, held_below_live: None },
        true,
    )
};
static YIELDS_REG: [VenueRow; 1] = [VenueRow::Mount(&YIELDS)];

/// `SidecarHeldElsewhere`: with a labelled account named, the DEFAULT account yields; the fold
/// renders the bridge's `held_by_another` with the holder and never asks it to mount.
#[test]
fn an_account_that_yields_the_resource_is_refused_with_its_holder_and_never_mounted() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let named = crate::MountPolicy {
        venues: VenuePolicy::default().declare("hyperliquid", VenueMode::Demo).declare_account(
            "hyperliquid",
            &alt,
            VenueMode::Demo,
        ),
        ..Default::default()
    };
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    mount_one(&YIELDS_REG, "hyperliquid", "BTC", &tx, &mut live, Some(&budget()), Some(&named))
        .expect("a yielding account is paper, not an error");
    assert!(live.is_empty(), "the default account yields");
    assert_eq!(
        YIELDS.mounts.load(Ordering::SeqCst),
        0,
        "the yielding account never reaches the bridge"
    );
    let expected = format!("{} yields to ALT", AccountLabel::Default);
    assert!(
        HELD_BY_ANOTHER.lock().expect("the planted log").contains(&expected),
        "the refusal renders the holder the policy named: `{expected}`"
    );
}

const PAPER_RESOURCE: &str = "the paper-answer test's resource";

static PAPER_EXCLUSIVE: Planted = Planted {
    exclusive: true,
    resource: Some(PAPER_RESOURCE),
    ..Planted::new("deribit", Resolution::Paper(PaperCause::NoCredentials), false)
};
static PAPER_EXCLUSIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&PAPER_EXCLUSIVE)];

/// A PAPER answer on an exclusive row mounts without touching the claim: the bridge is asked
/// while the resource is held elsewhere; a fold that claimed first would never ask it.
#[test]
fn a_paper_answer_on_an_exclusive_row_mounts_without_touching_the_claim() {
    let held = crate::exclusive::claim(PAPER_RESOURCE).expect("the test holds the resource");
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    mount_one(
        &PAPER_EXCLUSIVE_REG,
        "deribit",
        "BTC-PERPETUAL",
        &tx,
        &mut live,
        None,
        Some(&policy("deribit", VenueMode::Demo)),
    )
    .expect("paper never refuses");
    assert_eq!(PAPER_EXCLUSIVE.mounts.load(Ordering::SeqCst), 1, "the bridge is asked");
    assert!(live.is_empty());
    drop(held);
}
