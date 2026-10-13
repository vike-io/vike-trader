//! A11y tests of the rail marks and the DETAIL pane: tier states, fields and their presence, the
//! feed badge.

use std::collections::{BTreeMap, HashMap};

use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::AccountGrids;
use vike_model::AccountTier;
use vike_model::feed_status::ConnectionState;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{AccountKeys, Accounts};

use crate::support::{grids_of, harness, harness_grids, harness_with, nodes, row, tree_text};

/// The rail's four marks — the same four `view.rs` renders, spelled here so an assertion can name
/// the one it means. ⚠ Only the first two are MEASUREMENTS, which is why only they are dots.
const GLYPH_CONFIGURED: &str = "\u{25CF}";
const GLYPH_NOT_SET: &str = "\u{25CB}";
const GLYPH_NOT_CONFIGURABLE: &str = "\u{00B7}";
const GLYPH_UNKNOWN: &str = "?";

/// dukascopy with ONE default-account row, at DEMO (id 1), holding `book`'s fields.
fn dukascopy_demo(book: CredentialBook) -> Harness<'static, ()> {
    let grids = grids_of(&["dukascopy"], vec![row(1, "dukascopy", AccountTier::Demo, None)], book);
    harness_grids(grids, HashMap::new())
}

/// ⚠ **A cell that takes no credential says WHY, in words.** The grid this pane replaces rendered a
/// cell nothing can be configured in as the same hollow ring an unset one gets, distinguishable only
/// by a missing ✏ — which pointed an operator at a form that does not exist.
///
/// Two reasons, two sentences: the PAPER tier (the simulator reads no credential) and a tier with
/// NO ACCOUNT ROW — which names the command that creates the row, because a credential Save never
/// creates one.
///
/// Reddens on `tier_row` collapsing `TierState::NotConfigurable` back into `NotSet`.
#[test]
fn a_cell_that_takes_no_credential_says_why_in_words() {
    let mut duka = dukascopy_demo(CredentialBook::new());
    duka.run();
    let text = tree_text(&duka);
    assert!(text.contains("not configurable"), "the state is spelled out: {text}");
    assert!(
        text.contains("paper: the simulator reads no credential"),
        "the paper tier says why: {text}"
    );
    assert!(
        text.contains("no live account for dukascopy")
            && text.contains("vike-cli secrets account add --venue dukascopy --tier live"),
        "…and a tier with no row names the row and the command that creates it: {text}"
    );
    // ⚠ The CELL, not the tree text. `view.rs`'s mark legend names `not set` in every state,
    // so a `text.contains("not set")` here would pass with `tier_row` rendering nothing at all —
    // an assertion that cannot fail for its stated reason.
    assert!(
        status_cells(&duka).iter().any(|c| c == "\u{25CB} not set"),
        "the DEMO row, holding nothing, still reads as unset, in its own cell: {text}"
    );
}

/// ⚠ **The DETAIL pane spells every FIELD out, with its presence, without opening a form.** A hover
/// could name ONE thing and most venues' forms write two, three or four fields.
///
/// ⚠ And it renders FIELD WORDS and presence only: no value ever reaches it.
#[test]
fn the_detail_pane_spells_out_every_field_and_its_presence() {
    let mut empty = dukascopy_demo(CredentialBook::new());
    empty.run();
    let text = tree_text(&empty);
    for line in ["LOGIN  missing", "PASSWORD  missing"] {
        assert!(text.contains(line), "{line} must be on screen with no form open: {text}");
    }

    let mut book = CredentialBook::new();
    book.insert_account(1, "LOGIN", "login-1");
    let mut half = dukascopy_demo(book);
    half.run();
    let text = tree_text(&half);
    assert!(text.contains("LOGIN  stored"), "a held field says so: {text}");
    assert!(text.contains("PASSWORD  missing"), "…and the one still owed: {text}");
    assert!(text.contains("partial: 1 of 2 required fields stored"), "a PARTIAL set: {text}");
    assert!(!text.contains("login-1"), "a field's PRESENCE, never its value: {text}");

    // An OPTIONAL field is marked as such — dropping the mark would make a one-of-two form look
    // like a two-of-two one.
    let mut hl = harness("hyperliquid");
    hl.run();
    let text = tree_text(&hl);
    assert!(text.contains("ACCOUNT_ADDRESS  (optional)"), "the optional field is marked: {text}");
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

/// The detail pane's feed chip, verbatim — `crates/vike-connections/src/view/detail.rs`'s
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

/// The detail pane's FIELDS cell names the venue's schema-row fields — polymarket's L1 key first —
/// and says the set is bespoke rather than the generic API trio.
#[test]
fn the_fields_cell_names_the_schema_rows_fields() {
    let mut poly = harness("polymarket");
    poly.run();
    let text = tree_text(&poly);
    assert!(text.contains("Fields"), "{text}");
    assert!(text.contains("PRIVATE_KEY · "), "{text}");
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

/// **Every DETAIL-PANE STATUS CELL on screen, verbatim** — `tier_row`'s `<glyph> <TierState
/// label>`, which is the pane's one CLAIM about what was measured for a tier.
///
/// ⚠ It exists because a whole-tree `contains("not set")` stopped being able to answer the
/// question the unreadable-store gate asks. `view.rs`'s mark legend sits at the TOP of the panel,
/// so the words `not set` are on screen in every state as a LEGEND ENTRY. The legend spells its
/// entries `<glyph> = <word>` precisely so the two remain distinguishable; this helper is the
/// reading that uses the distinction.
fn status_cells(h: &Harness<'_, ()>) -> Vec<String> {
    // The four cell spellings `tier_row` can render, composed exactly as it composes them:
    // `<glyph> <TierState::label()>`. ⚠ EXACT equality, not a glyph prefix — the account strip's
    // `● default` chip and the legend's `● = configured` entry both START with a rail glyph, and a
    // prefix test would sweep them in and make this helper answer a different question.
    let want: Vec<String> = [
        (GLYPH_CONFIGURED, "configured"),
        (GLYPH_NOT_SET, "not set"),
        ("\u{2014}", "not configurable"),
        (GLYPH_UNKNOWN, "unknown — store unreadable"),
    ]
    .iter()
    .map(|(g, w)| format!("{g} {w}"))
    .collect();
    // ⚠ The same one-node-per-widget predicate [`rail_marks`] uses: egui can file a label's text on
    // BOTH the widget's node and an enclosing one, so matching `value().or(label())` would double
    // every cell and make a count mean nothing.
    nodes(h, |_| true)
        .iter()
        .filter_map(|n| {
            let a = n.accesskit_node();
            let t = match (a.value(), a.label()) {
                (Some(v), _) => v.to_string(),
                (None, Some(l)) => l.to_string(),
                (None, None) => return None,
            };
            want.contains(&t).then_some(t)
        })
        .collect()
}

/// ⚠ **A CELL THAT TAKES NO CREDENTIAL IS NOT A FILLED DOT.** The rail used to render
/// `TierState::NotConfigurable` as `●` at 45% opacity — a filled dot at the glyph level however
/// dim, and therefore indistinguishable from `Configured` both to a tree-reading test and to the
/// one human check this tree has.
///
/// That check is `scripts/qa_shots.sh`'s `05-connections` pose, which tells a judge that on the
/// credential-free QA root *a single FILLED dot in those three columns means the isolation broke —
/// report it immediately*. Every paper cell, and every cell with no account row, is
/// `NotConfigurable`, so every clean capture would false-positive that instruction;
/// `docs/gui-contact-sheet.md` restates the same claim.
///
/// Reddens on the mark going back to `●` in any opacity.
#[test]
fn a_tier_a_venue_does_not_have_is_not_rendered_as_a_filled_dot() {
    // dukascopy: PAPER reads no credential, LIVE has no account row, DEMO's row holds nothing.
    // Nothing is configured, so a filled dot anywhere in this rail is a dot that measured nothing.
    let mut duka = dukascopy_demo(CredentialBook::new());
    duka.run();
    assert!(
        rail_marks(&duka, GLYPH_CONFIGURED).is_empty(),
        "nothing is configured, so no rail mark may be the FILLED dot:\n{}",
        tree_text(&duka)
    );
    assert_eq!(rail_marks(&duka, GLYPH_NOT_CONFIGURABLE).len(), 2, "PAPER, and LIVE with no row");
    assert_eq!(rail_marks(&duka, GLYPH_NOT_SET).len(), 1, "DEMO's row holds nothing");

    // …and the filled dot still means what it says when the row holds its required fields.
    let mut book = CredentialBook::new();
    book.insert_account(1, "LOGIN", "l");
    book.insert_account(1, "PASSWORD", "p");
    let mut set = dukascopy_demo(book);
    set.run();
    assert_eq!(
        rail_marks(&set, GLYPH_CONFIGURED).len(),
        1,
        "the ONE complete row is the ONE filled dot:\n{}",
        tree_text(&set)
    );
    assert_eq!(rail_marks(&set, GLYPH_NOT_CONFIGURABLE).len(), 2);
}

/// ⚠ **A STORE THAT COULD NOT BE OPENED IS NOT A STORE WITH NOTHING IN IT.**
/// A store that exists and will not open answers no rows and no fields — byte-identical to an
/// empty one. Folded blind, this panel renders "no account" for every cell and `0 set` for the
/// count — measured statements about a file nothing read, which the root `CLAUDE.md` names as the
/// exact failure to avoid ("a permissions bug wearing the 'not configured' answer looks exactly
/// like a correct fresh install").
///
/// Reddens on `connections_ui` ignoring the grid's health, which is the state this pane was in.
#[test]
fn an_unreadable_store_says_so_and_renders_no_measurement() {
    let why = "credential store /srv/vike/settings/db/vike.db could not be read: permission denied";
    let dir = AccountDirectory::read(
        Err::<Accounts, _>(why),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>(why),
        Err::<CredentialBook, _>(why),
    );
    let grids = AccountGrids::for_venues(&["binance"], &dir, &HashMap::new());
    let mut h = harness_grids(grids, HashMap::new());
    h.run();
    let text = tree_text(&h);

    assert!(
        text.contains("could not be opened"),
        "the banner states the fault before anything else: {text}"
    );
    assert!(text.contains("permission denied"), "…verbatim, so it is actionable: {text}");
    // ⚠ The CLAIMS, read by [`status_cells`] — the legend names `not set` in every state, so the
    // tree text cannot answer this. Paper still says it takes no credential: that is a fact about
    // the tier, true whether or not a store was read, and it claims nothing about the store.
    assert_eq!(
        status_cells(&h),
        vec![
            "\u{2014} not configurable".to_string(),
            "? unknown — store unreadable".to_string(),
            "? unknown — store unreadable".to_string()
        ],
        "NOT ONE demo or live cell may claim to have been measured: {text}"
    );
    assert_eq!(
        rail_marks(&h, GLYPH_UNKNOWN).len(),
        2,
        "binance's demo and live cells are unmeasured:\n{text}"
    );
    assert!(rail_marks(&h, GLYPH_NOT_SET).is_empty(), "…and none is the hollow ring: {text}");
    assert!(rail_marks(&h, GLYPH_CONFIGURED).is_empty(), "…nor the filled dot: {text}");
}
