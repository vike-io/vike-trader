//! Accessibility-tree tests for the credential RAIL + DETAIL pane + masked edit form
//! ([`vike_connections::connections_ui`]).
//!
//! ⚠ The first three properties below were written against the 5-column GRID this pane replaced,
//! and they survive that replacement UNCHANGED — which is the point of keeping them: the ✎ still
//! appears on exactly the tiers a venue can store keys for, in Sim/Demo/Live order, and the form
//! it opens is still masked and still belongs to the cell that was clicked. The rewrite moved the
//! layout; it may not have moved any of that. The properties AFTER them are the ones the detail
//! pane adds.
//!
//! `view.rs`'s own tests all call the pure `edit_fields` helper directly; the RENDER was untested,
//! which left three of the module's stated rules asserted nowhere:
//!
//! 1. **"Edit fields are `egui::TextEdit::password(true)` (masked)"** — security rule 3 of the
//!    module doc, and the test only an accessibility tree can make. `.password(true)` is what
//!    makes egui file the widget as `Role::PasswordInput` instead of `Role::TextInput`, and the
//!    same call is what masks the value handed to assistive tech. Dropping it compiles, renders a
//!    normal-looking field, keeps every pure test green — and reads a live API secret aloud.
//! 2. **The ✎ opens the tier row it sits in.** `tier_row` is called three times per venue with only
//!    the tier string differing; an off-by-one would open the SIM form from the LIVE cell and save
//!    a live key into a sim var. The placeholders assert which form actually opened.
//! 3. **A tier with no credentials to write offers no edit affordance.** `tier_row` renders the
//!    ✎ only when `edit_fields(venue, tier)` is non-empty. The pure tests pin what `edit_fields`
//!    RETURNS; nothing pinned that the view still consults it, so a `tier_row` that stopped
//!    asking would open a form writing `DUKASCOPY_SIM_*` vars no bridge reads.
//!
//! ⚠ These tests never click **Save** — [`NEVER_WRITTEN_STORE`] is the belt that says so. The
//! braces are that `connections_ui` no longer resolves the store for itself: the path is now a
//! `vike_connections::CredentialWrite` parameter, so a harness can point it anywhere and the real
//! `workspace_dotenv_path()` is unreachable from here. Nothing in this workspace may rewrite the
//! user's only copy of live venue keys (CLAUDE.md, "Credentials & the live gate"). Opening the
//! form and reading the tree is the whole interaction — no secret is typed, so none can leak.
//! (The Save arm's own end-to-end gate lives in `tests/credential_write_journal.rs`, which drives
//! it against a throwaway store and a throwaway ledger.)
//!
//! ⚠ **One test seeds a REAL store, and it is the one that matters most.** Every harness above
//! builds an all-`false` grid, so until
//! [`no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor`] existed
//! there was no VALUE anywhere on the box for a regression to render — a pane that started
//! pre-filling the edit buffers from the store, the exact thing security rule 1 forbids, passed
//! every assertion in this file. That test plants a sentinel value in a scratch `secrets.env`,
//! derives the grid from the same map the binary would, and walks the rail, a detail pane, an
//! opened editor and a labelled account's grid asserting the sentinel reaches none of them. It
//! still never clicks Save.

use std::collections::HashMap;
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::{
    AccountGrids, ConnectionState, CredentialWrite, StoreHealth, VenueCredStatus, connections_ui,
};

/// The glyph `tier_row` labels its edit button with.
const PENCIL: &str = "\u{270E}";

/// The rail's four marks — the same four `view.rs` renders, spelled here so an assertion can name
/// the one it means. ⚠ Only the first two are MEASUREMENTS, which is why only they are dots.
const GLYPH_CONFIGURED: &str = "\u{25CF}";
const GLYPH_NOT_SET: &str = "\u{25CB}";
const GLYPH_NOT_CONFIGURABLE: &str = "\u{00B7}";
const GLYPH_UNKNOWN: &str = "?";

/// A store path these tests never write, and which no walk produced.
///
/// Relative and deliberately absurd: if a future edit made this file's harness reach the Save arm,
/// the failure is a stray file in the crate directory that review would notice, not a mutation of
/// whoever's `secrets.env` the working directory happened to sit above.
const NEVER_WRITTEN_STORE: &str = "a11y-tests-never-save-to-this.env";

/// One venue's row only, so a count of ✎ buttons is a count of THAT venue's editable tiers — and
/// so the rail's single row is the SELECTED one, which the detail pane is therefore showing.
fn harness(venue: &'static str) -> Harness<'static, ()> {
    harness_with(venue, HashMap::new())
}

/// The same, with a live-feed status map — the fact the DETAIL pane's `Status` row renders and
/// the rail's dots deliberately do not.
fn harness_with(
    venue: &'static str,
    live: HashMap<String, ConnectionState>,
) -> Harness<'static, ()> {
    let grids = AccountGrids::single(vec![VenueCredStatus {
        venue: venue.to_string(),
        sim: false,
        demo: false,
        live: false,
    }]);
    harness_grids(grids, live, StoreHealth::Readable)
}

/// The same, over a grid and a store health the caller built — the door the sentinel-value suite
/// and the unreadable-store suite come through.
fn harness_grids(
    grids: AccountGrids,
    live: HashMap<String, ConnectionState>,
    health: StoreHealth,
) -> Harness<'static, ()> {
    // `journal: None` — these tests assert about the TREE, and a ledger they never write to is one
    // fewer thing that could make them pass for the wrong reason.
    let creds = CredentialWrite { store: Path::new(NEVER_WRITTEN_STORE), journal: None, now_ms: 0 };
    Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, &health, creds, None);
    })
}

/// Every accessibility node matching `pred`, in tree order.
///
/// Walked explicitly rather than through `get_by_*` because a plain label could render the SAME ✎
/// glyph as a plain label, and this test must count BUTTONS. (egui files a `Role::Label`'s text
/// under the node's `value`, not its `label`, so the two are already distinguishable — asserting
/// the role as well means the count cannot start matching the legend if that ever changes.)
fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

fn edit_buttons<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(PENCIL)
    })
}

fn text_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| {
        matches!(
            n.accesskit_node().role(),
            Role::TextInput | Role::MultilineTextInput | Role::PasswordInput
        )
    })
}

/// The edit affordance appears exactly on the tiers a venue can actually store keys for.
///
/// binance has all three (generic `_API_KEY`/`_API_SECRET`/`_API_PASSPHRASE` at Sim/Demo/Live);
/// dukascopy has ONLY Demo — no Sim or Live tier exists for it, so those two cells must stay
/// dot-only. Reddens on `tier_row` dropping its `TierState::NotConfigurable` guard, which the
/// pure `dukascopy_sim_and_live_have_no_edit_fields` test cannot see.
#[test]
fn only_tiers_with_credentials_to_write_offer_an_edit_button() {
    let mut generic = harness("binance");
    generic.run();
    assert_eq!(edit_buttons(&generic).len(), 3, "binance stores keys at all three tiers");

    let mut duka = harness("dukascopy");
    duka.run();
    assert_eq!(
        edit_buttons(&duka).len(),
        1,
        "dukascopy has only a DEMO tier — its Sim and Live cells must offer no form"
    );
}

/// SECURITY: every field of an opened credential form is a PASSWORD input in the accessibility
/// tree — not merely drawn with dots — and the form that opened belongs to the cell that was
/// clicked.
///
/// Reddens on deleting `.password(true)` from `render_edit_form`'s `TextEdit` (the role falls back
/// to `Role::TextInput`, the state in which assistive tech and any tree-reading automation are
/// entitled to speak a typed API secret out loud), and on `tier_row` passing the wrong tier.
#[test]
fn an_opened_credential_form_is_masked_and_belongs_to_the_cell_that_was_clicked() {
    let mut h = harness("binance");
    h.run();
    assert!(text_fields(&h).is_empty(), "no form is open before the ✎ is clicked");

    // Row order is Sim, Demo, Live — click the LAST cell's ✎ and the LIVE form must open.
    let buttons = edit_buttons(&h);
    buttons.last().expect("binance offers an edit button").click();
    drop(buttons);
    h.run();

    let fields = text_fields(&h);
    let placeholders: Vec<String> = fields
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        placeholders,
        vec!["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_SECRET", "BINANCE_LIVE_API_PASSPHRASE"],
        "the LIVE cell's ✎ must open the LIVE form, in `edit_fields` order"
    );

    for (f, key) in fields.iter().zip(&placeholders) {
        assert_eq!(
            f.accesskit_node().role(),
            Role::PasswordInput,
            "{key} must be masked — TextEdit::password(true)"
        );
    }
}

/// Everything the tree says, one node per line — label and value both, because egui files a
/// `Role::Label`'s text under `value` rather than `label`.
fn tree_text(h: &Harness<'_, ()>) -> String {
    h.root()
        .children_recursive()
        .map(|n| {
            let a = n.accesskit_node();
            let mut s = String::new();
            if let Some(l) = a.label() {
                s.push_str(&l);
                s.push(' ');
            }
            if let Some(v) = a.value() {
                s.push_str(&v);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// ⚠ **A tier a venue does not HAVE says so IN WORDS.** The grid this pane replaces rendered it as
/// the same hollow ring an unset-but-real tier gets, distinguishable only by a missing ✎ and a
/// tooltip naming a key nothing writes — which pointed an operator at a form that does not exist.
///
/// Reddens on `tier_row` collapsing `TierState::NotConfigurable` back into `NotSet`.
#[test]
fn a_tier_a_venue_does_not_have_says_so_in_words() {
    let mut duka = harness("dukascopy");
    duka.run();
    let text = tree_text(&duka);
    assert!(text.contains("not configurable"), "the state is spelled out: {text}");
    assert!(
        text.contains("dukascopy has no sim tier"),
        "…and the sentence NAMES the venue and the tier: {text}"
    );
    assert!(text.contains("dukascopy has no live tier"), "{text}");
    assert!(text.contains("not set"), "the real DEMO tier still reads as unset: {text}");
}

/// ⚠ **The DETAIL pane spells every key name out, without opening a form.** A hover tooltip could
/// name ONE key and most venues' forms write two, three or four — dukascopy's DEMO cell alone
/// writes four, and before this pane there was no way to see the other three without clicking.
///
/// ⚠ And it renders NAMES only: every string here is composed by `account_fields` out of the
/// venue, the tier and the account label, which has no access to the store at all.
#[test]
fn the_detail_pane_spells_out_every_key_name_the_form_would_write() {
    let mut duka = harness("dukascopy");
    duka.run();
    let text = tree_text(&duka);
    for key in [
        "DUKASCOPY_DEMO1_LOGIN",
        "DUKASCOPY_DEMO1_PASSWORD",
        "DUKASCOPY_DEMO2_LOGIN",
        "DUKASCOPY_DEMO2_PASSWORD",
    ] {
        assert!(text.contains(key), "{key} must be on screen with no form open: {text}");
    }

    // An OPTIONAL field is marked as such — `edit_fields`' own UI label carries it, and dropping
    // the mark would make a two-of-three form look like a three-of-three one.
    let mut hl = harness("hyperliquid");
    hl.run();
    let text = tree_text(&hl);
    assert!(text.contains("HYPERLIQUID_LIVE_ACCOUNT_ADDRESS"), "{text}");
    assert!(text.contains("(optional)"), "the optional field is marked: {text}");
}

/// ⚠ **THE TWO FACTS STAY APART.** The rail's dots are credential PRESENCE; the detail pane's
/// `Status` is the LIVE FEED's, labelled as such, and a venue with no feed producer says there is
/// none rather than reading `Unknown`.
///
/// Reddens on a pane that folds an absent producer into `ConnectionState::default()`, which is
/// what the grid this replaces did — and on the footer note claiming the DOTS are the feed's,
/// which is what that grid's footer said and what would now be attached to the wrong glyph.
#[test]
fn the_feed_status_is_labelled_and_an_absent_producer_is_not_unknown() {
    let mut none = harness("deribit");
    none.run();
    let text = tree_text(&none);
    assert!(text.contains("Status (live feed)"), "the cell says WHICH fact it is: {text}");
    assert!(
        text.contains("no feed producer in this build"),
        "an absent producer is not `Unknown`: {text}"
    );
    // ⚠ EXACT-node, not `contains`. The absent-producer SENTENCE is "no feed producer in this
    // build", which contains the BADGE's whole text — so a substring check here can never fail and
    // would be the assertion-that-cannot-fail-for-its-stated-reason. The badge is its own label
    // node; that is the thing to look for.
    assert!(
        !has_badge(&none, FEED_PRODUCER_BADGE),
        "…and the badge is a statement about this build, absent when there is no producer: {text}"
    );
    assert!(
        text.contains("CREDENTIAL PRESENCE"),
        "the footer must say what the dots are, not what the old Status column was: {text}"
    );

    let mut live = HashMap::new();
    live.insert("binance".to_string(), ConnectionState::Connected);
    let mut wired = harness_with("binance", live);
    wired.run();
    let text = tree_text(&wired);
    assert!(has_badge(&wired, FEED_PRODUCER_BADGE), "a wired producer earns it: {text}");
    assert!(text.contains("Connected"), "{text}");
}

/// The detail pane's feed chip, verbatim — `crates/vike-connections/src/view.rs`'s
/// `FEED_PRODUCER_BADGE`.
const FEED_PRODUCER_BADGE: &str = "feed producer in this build";

/// Whether the badge is on screen as its OWN node, rather than as a substring of a sentence that
/// happens to contain it — which "no feed producer in this build" and the rail's footer both do.
fn has_badge(h: &Harness<'_, ()>, text: &str) -> bool {
    let want = text.to_string();
    !nodes(h, move |n| {
        let a = n.accesskit_node();
        a.value().as_deref() == Some(want.as_str()) || a.label().as_deref() == Some(want.as_str())
    })
    .is_empty()
}

/// ⚠ **THE BADGE MAY NOT CONTRADICT THE ROW UNDER IT.** It is a BUILD fact — this binary has a
/// feed producer for this venue — and `FeedFact::has_producer` is true for every reported state,
/// `Disconnected` included. It therefore renders a few pixels above `Status (live feed)
/// Disconnected`, and while it read `Streaming feed` that pairing was a present-tense claim of
/// activity sitting on top of the measurement denying it.
///
/// Two halves, and the first is the one that already held: the badge is CONDITIONAL (a venue with
/// no producer does not get it — the test above). The second is this one: whatever it says must
/// still be true when the producer is down.
///
/// Reddens on the badge going back to a word that asserts liveness, and on it being drawn
/// unconditionally.
#[test]
fn the_feed_badge_states_a_build_fact_and_survives_a_disconnected_producer() {
    let mut live = HashMap::new();
    live.insert("binance".to_string(), ConnectionState::Disconnected);
    let mut h = harness_with("binance", live);
    h.run();
    let text = tree_text(&h);

    assert!(text.contains("Disconnected"), "the producer reports down: {text}");
    assert!(
        has_badge(&h, FEED_PRODUCER_BADGE),
        "…and the badge still states the BUILD fact, which is still true: {text}"
    );
    assert!(
        !text.contains("Streaming"),
        "…but nothing on this pane may claim the feed is streaming while it is not: {text}"
    );
}

/// The detail pane's KEY FAMILY cell names the store's own prefix — `POLY_`, not the venue slug's
/// `POLYMARKET_`, which is a prefix nothing in this tree writes or reads.
#[test]
fn the_key_family_cell_names_the_stores_prefix_not_the_venue_slug() {
    let mut poly = harness("polymarket");
    poly.run();
    let text = tree_text(&poly);
    assert!(text.contains("Key family"), "{text}");
    assert!(text.contains("POLY_"), "{text}");
    assert!(text.contains("bespoke"), "{text}");
}

/// Every accessibility node whose whole text is exactly `glyph` — i.e. a RAIL mark, as opposed to
/// the detail pane's `<glyph> <words>` cell or the account strip's `● default` chip.
fn rail_marks<'t, 'h>(h: &'t Harness<'h, ()>, glyph: &str) -> Vec<Node<'t>> {
    let want = glyph.to_string();
    nodes(h, move |n| {
        let a = n.accesskit_node();
        a.value().as_deref() == Some(want.as_str())
            || a.label().as_deref() == Some(want.as_str()) && a.value().is_none()
    })
}

/// ⚠ **A TIER THAT DOES NOT EXIST IS NOT A FILLED DOT.** The rail used to render
/// `TierState::NotConfigurable` as `●` at 45% opacity — a filled dot at the glyph level however
/// dim, and therefore indistinguishable from `Configured` both to a tree-reading test and to the
/// one human check this tree has.
///
/// That check is `scripts/qa_shots.sh`'s `05-connections` pose, which tells a judge that on the
/// credential-free QA root *a single FILLED dot in those three columns means the isolation broke —
/// report it immediately*. dukascopy SIM/LIVE, aster SIM, hyperliquid SIM, alpaca SIM and
/// polymarket SIM/DEMO are all `NotConfigurable`, so every clean capture false-positived that
/// instruction; `docs/gui-contact-sheet.md` restated the same claim.
///
/// Reddens on the mark going back to `●` in any opacity.
#[test]
fn a_tier_a_venue_does_not_have_is_not_rendered_as_a_filled_dot() {
    // dukascopy: SIM and LIVE do not exist, DEMO exists and is unset. Nothing is configured, so a
    // filled dot anywhere in this rail is a dot that measured nothing.
    let mut duka = harness("dukascopy");
    duka.run();
    assert!(
        rail_marks(&duka, GLYPH_CONFIGURED).is_empty(),
        "nothing is configured, so no rail mark may be the FILLED dot:\n{}",
        tree_text(&duka)
    );
    assert_eq!(rail_marks(&duka, GLYPH_NOT_CONFIGURABLE).len(), 2, "SIM and LIVE have no tier");
    assert_eq!(rail_marks(&duka, GLYPH_NOT_SET).len(), 1, "DEMO exists and is unset");

    // …and the filled dot still means what it says when something IS stored.
    let grids = AccountGrids::single(vec![VenueCredStatus {
        venue: "dukascopy".to_string(),
        sim: false,
        demo: true,
        live: false,
    }]);
    let mut set = harness_grids(grids, HashMap::new(), StoreHealth::Readable);
    set.run();
    assert_eq!(
        rail_marks(&set, GLYPH_CONFIGURED).len(),
        1,
        "the ONE stored tier is the ONE filled dot:\n{}",
        tree_text(&set)
    );
    assert_eq!(rail_marks(&set, GLYPH_NOT_CONFIGURABLE).len(), 2);
}

/// ⚠ **A STORE THAT COULD NOT BE OPENED IS NOT A STORE WITH NOTHING IN IT.**
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env` is documented INFALLIBLE: an
/// unopenable store logs `tracing::error!` and returns an EMPTY map, byte-identical to an absent
/// one. Folded blind, this panel renders `not set` for every cell and `0 set` for the count —
/// measured statements about a file nothing read, which the root `CLAUDE.md` names as the exact
/// failure to avoid ("a permissions bug wearing the 'not configured' answer looks exactly like a
/// correct fresh install").
///
/// Reddens on `connections_ui` ignoring `StoreHealth`, which is the state this pane was in.
#[test]
fn an_unreadable_store_says_so_and_renders_no_measurement() {
    let grids = AccountGrids::single(vec![VenueCredStatus {
        venue: "binance".to_string(),
        sim: false,
        demo: false,
        live: false,
    }]);
    let health = StoreHealth::Unreadable(
        "credential store /srv/vike/settings/secrets.env could not be read: permission denied"
            .to_string(),
    );
    let mut h = harness_grids(grids, HashMap::new(), health);
    h.run();
    let text = tree_text(&h);

    assert!(
        text.contains("could not be opened"),
        "the banner states the fault before anything else: {text}"
    );
    assert!(text.contains("permission denied"), "…verbatim, so it is actionable: {text}");
    assert!(!text.contains("not set"), "NOT ONE cell may claim to have been measured: {text}");
    assert!(text.contains("unknown"), "…they say unknown instead: {text}");
    assert_eq!(
        rail_marks(&h, GLYPH_UNKNOWN).len(),
        3,
        "all three of binance's tiers are unmeasured:\n{text}"
    );
    assert!(rail_marks(&h, GLYPH_NOT_SET).is_empty(), "…and none is the hollow ring: {text}");
    assert!(rail_marks(&h, GLYPH_CONFIGURED).is_empty(), "…nor the filled dot: {text}");
}

// ------------------------------------------------------------------------------------------
// ⚠ THE PROPERTY THAT MATTERS MOST — and until now the one this suite could not have caught
// ------------------------------------------------------------------------------------------

/// A value no venue key, no tier token, no account label, no venue name and no scratch path
/// contains — so a hit is a LEAK and never a coincidence. Same discipline (and the same reason) as
/// `tests/credential_write_journal.rs`'s typed constants.
const SENTINEL: &str = "zqxjvw7413mfbphgnd8256wu";

/// The shortest run of [`SENTINEL`] this test refuses to find on screen. A pane that rendered every
/// character but the last would pass a bare `contains` check, and a truncated API secret is still
/// an API secret.
const MIN_LEAK_WINDOW: usize = 8;

/// The store this suite's OTHER tests never write is a relative dead path; this one needs a REAL
/// file, because the property is "a value on disk does not reach the screen" and a store that does
/// not exist cannot prove it.
fn seeded_store() -> (vike_model::scratch::ScratchDir, HashMap<String, String>) {
    // The system temp directory is legitimate in a test — `crates/vike-ops/tests/system_temp_gate.rs`
    // scopes itself to production code. `ScratchDir` is unique per process and self-deleting.
    let dir = vike_model::scratch::ScratchDir::create_in(
        &std::env::temp_dir(),
        "vike-connections-no-secret",
    )
    .expect("scratch root");

    // Every SHAPE this panel can render a key name for: the generic `{VENUE}_{TIER}_API_*` trio,
    // a bespoke FX pair, an EIP-712 private key, and one LABELLED account (whose label the panel
    // DOES render — which is exactly why the label and the value are different strings here).
    let keys = [
        "BINANCE_LIVE_API_KEY",
        "BINANCE_LIVE_API_SECRET",
        "BINANCE_LIVE_API_PASSPHRASE",
        "BINANCE_DEMO_API_KEY",
        "BINANCE_DEMO_API_SECRET",
        "DUKASCOPY_DEMO1_LOGIN",
        "DUKASCOPY_DEMO1_PASSWORD",
        "HYPERLIQUID_LIVE_PRIVATE_KEY",
        "BINANCE_LIVE_API_KEY__HEDGE",
        "BINANCE_LIVE_API_SECRET__HEDGE",
    ];
    let vars: HashMap<String, String> =
        keys.iter().map(|k| ((*k).to_string(), SENTINEL.to_string())).collect();

    // …and the same bytes on disk, at the path the panel is handed. Nothing in `connections_ui`
    // reads this file today; writing it is what makes the test able to FAIL if something ever
    // starts to — a read-back into the edit buffers being the regression the module's own security
    // rule 1 forbids by name.
    let mut text = String::new();
    for k in keys {
        text.push_str(&format!("{k}={SENTINEL}\n"));
    }
    std::fs::write(dir.path().join("secrets.env"), text).expect("seed the store");
    (dir, vars)
}

fn assert_no_sentinel(h: &Harness<'_, ()>, stage: &str) {
    let text = tree_text(h);
    for start in 0..=SENTINEL.len() - MIN_LEAK_WINDOW {
        let window = &SENTINEL[start..start + MIN_LEAK_WINDOW];
        assert!(
            !text.contains(window),
            "{stage}: a {MIN_LEAK_WINDOW}-character run of a stored credential VALUE ({window}) \
             reached the accessibility tree:\n{text}"
        );
    }
}

/// ⚠ **NO STORED VALUE REACHES THE RAIL, THE DETAIL PANE OR AN OPENED EDITOR.**
///
/// This is the pane's first security rule — *"An existing secret's plaintext is NEVER read back
/// into the UI — edit fields always start empty"* — and until now nothing asserted it end to end:
/// every harness in this file seeded an all-`false` grid, so there was no value on the box for a
/// regression to render. A pane that pre-filled the edit buffers from the store, or put a value in
/// a heading, a chip or a cell, passed every test here.
///
/// Driven the way the binary drives it: a real `secrets.env` on disk, the grid derived from the
/// same map by `AccountGrids::from_vars`, and the panel walked through the four states that could
/// each leak differently — the rail, a selected venue's detail pane, an OPENED form, and a
/// LABELLED account's grid.
///
/// ⚠ **Save is never clicked** — the suite's rule, and here the store is a real file, so it matters
/// more than usual. The scratch directory is self-deleting either way.
#[test]
fn no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor() {
    let (dir, vars) = seeded_store();
    let store = dir.path().join("secrets.env");

    // The guard against a test that cannot fail for its stated reason: the value really is on disk
    // and really is what the grid was derived from.
    let on_disk = std::fs::read_to_string(&store).expect("read the seeded store");
    assert!(on_disk.contains(SENTINEL), "the store must actually hold the sentinel");
    let grids = AccountGrids::from_vars(&vars);
    assert!(
        grids.labels().any(|l| l.text() == Some("HEDGE")),
        "the labelled account must be enumerated, or half this test is inert"
    );
    assert!(
        grids.default_grid().iter().any(|r| r.venue == "binance" && r.live && r.demo),
        "binance's LIVE and DEMO must read as configured, or the grid saw nothing"
    );

    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { store: &store, journal: None, now_ms: 0 };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
    });
    h.run();
    assert_no_sentinel(&h, "the rail on first paint");

    // 1 — SELECT the venue whose keys are seeded, so the detail pane renders them. ⚠ The selected
    // row is a LABEL, not a button (the pane's `aria-current` idiom), so there is NO button to
    // click when binance is already the roster's first row — which it is today. Both shapes end on
    // binance, and the assertion below is what proves which pane is actually on screen rather than
    // the click count.
    {
        let picks = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("binance")
        });
        assert!(picks.len() <= 1, "a venue appears in the rail once: {}", picks.len());
        if let Some(p) = picks.first() {
            p.click();
        }
    }
    h.run();
    let text = tree_text(&h);
    assert!(
        text.contains("BINANCE_LIVE_API_KEY"),
        "the detail pane must be binance's, and it renders the key NAME: {text}"
    );
    assert_no_sentinel(&h, "binance's detail pane");

    // 2 — OPEN the LIVE form. The buffers must start EMPTY; a read-back is the regression.
    {
        let pencils = edit_buttons(&h);
        assert_eq!(pencils.len(), 3, "binance offers three editable tiers");
        pencils.last().expect("the LIVE row's edit button").click();
    }
    h.run();
    let fields = text_fields(&h);
    assert_eq!(fields.len(), 3, "the LIVE form is open");
    for f in &fields {
        let a = f.accesskit_node();
        assert_eq!(a.role(), Role::PasswordInput, "still masked");
        assert_eq!(
            a.value().unwrap_or_default(),
            "",
            "an edit field starts EMPTY — blank means keep, and a pre-filled one is the store \
             read back into the UI"
        );
    }
    drop(fields);
    assert_no_sentinel(&h, "binance's opened LIVE editor");

    // 3 — the LABELLED account's grid, reached through its chip.
    {
        let picks = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("HEDGE")
        });
        assert_eq!(picks.len(), 1, "the HEDGE chip is a button until it is selected");
        picks[0].click();
    }
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("HEDGE"), "the account LABEL is rendered — names are not values: {text}");
    assert_no_sentinel(&h, "the HEDGE account's grid");

    // …and Save was never reached: the seeded bytes are untouched.
    assert_eq!(
        std::fs::read_to_string(&store).expect("re-read the store"),
        on_disk,
        "this suite never writes the store"
    );
}
