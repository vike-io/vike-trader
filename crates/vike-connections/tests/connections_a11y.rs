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
        vec![
            "BINANCE_LIVE_API_KEY",
            "BINANCE_LIVE_API_SECRET",
            "BINANCE_LIVE_API_PASSPHRASE",
            // The venue-wide attribution tail — see the role assertion below, which is where this
            // test stops being "every field is masked" and becomes "every SECRET field is".
            "BINANCE_BROKER_CODE",
        ],
        "the LIVE cell's ✎ must open the LIVE form, in `edit_fields` order"
    );

    for (f, key) in fields.iter().zip(&placeholders) {
        // ⚠ The rule is `key_sensitivity`'s, not this test's. Three of these four authenticate and
        // must be `PasswordInput` — the state assistive tech and any tree-reading automation are
        // entitled to speak aloud. The fourth is an affiliate tag stamped on outgoing orders in the
        // clear: masking it would buy nothing and cost the operator the ability to read it back,
        // which is the failure the owner reported about this panel.
        let want = match vike_connections::view::key_sensitivity(key) {
            vike_connections::view::Sensitivity::Secret => Role::PasswordInput,
            vike_connections::view::Sensitivity::Public => Role::TextInput,
        };
        assert_eq!(f.accesskit_node().role(), want, "{key}");
    }
    // ...and the masking really is doing work here: most of this form IS secret.
    assert_eq!(
        placeholders
            .iter()
            .filter(|k| vike_connections::view::key_sensitivity(k)
                == vike_connections::view::Sensitivity::Secret)
            .count(),
        3,
        "a form where nothing is classified secret would pass the loop above vacuously"
    );
}

/// **THE OTHER HALF OF THE OWNER'S REPORT.** *"Editing Dukascopy credentials doesn't show them"*
/// is two complaints in one sentence: fields that were missing (closed elsewhere) and fields that
/// are DELIBERATELY empty. The second is the design and must stay — a stored plaintext is never
/// read back — but a form that silently discards what you cannot see is indistinguishable from a
/// broken one, so the panel has to SAY it where the operator is looking.
///
/// Asserted against `view::MASKED_FIELD_HINT`'s own bytes rather than a paraphrase, plus the three
/// facts a reader has to come away with, so a rewrite that drops one of them reddens rather than
/// merely reading differently. ⚠ It asserts the text REACHED THE TREE — the hint used to be a
/// `ui.label` nobody had proven renders, and this file's whole reason for existing is that the pure
/// tests cannot see a widget that was never drawn.
#[test]
fn an_opened_form_says_why_it_is_empty_before_a_field_is_typed() {
    let mut h = harness("binance");
    h.run();
    let before = tree_text(&h);
    assert!(
        !before.contains(vike_connections::view::MASKED_FIELD_HINT),
        "the hint belongs to an OPEN form, not to the closed panel: {before}"
    );

    let buttons = edit_buttons(&h);
    buttons.last().expect("binance offers an edit button").click();
    drop(buttons);
    h.run();

    let text = tree_text(&h);
    assert!(
        text.contains(vike_connections::view::MASKED_FIELD_HINT),
        "the masked-field hint must reach the screen with the form: {text}"
    );
    // The three facts, each stated in the operator's own terms.
    for phrase in ["never read back", "starts EMPTY", "REPLACE", "KEEP"] {
        assert!(text.contains(phrase), "the hint must still say `{phrase}`: {text}");
    }
    // ...and every SECRET field is still masked. The wording change must never be paid for with
    // the masking, which is what "just show me the value" would actually cost.
    let secrets = text_fields(&h)
        .iter()
        .filter(|f| {
            let key = f.accesskit_node().placeholder().unwrap_or_default().to_string();
            vike_connections::view::key_sensitivity(&key)
                == vike_connections::view::Sensitivity::Secret
        })
        .map(|f| f.accesskit_node().role())
        .collect::<Vec<_>>();
    assert_eq!(secrets, vec![Role::PasswordInput; 3], "binance's three API fields stay masked");
}

/// **A FIELD THE MOUNT MAY NOT HONOUR SAYS SO, BESIDE ITSELF.** Dukascopy's `_SERVER` is the one
/// name this panel writes whose effect is conditional: `vike_dukascopy::exec`'s
/// `DukascopyExec::spawn_with_program` keeps a stored value only when it ends in `.jnlp` and
/// otherwise falls back to its own `DEFAULT_DEMO_JNLP` — and the web-platform login url the
/// variable most often holds is not one. Offering the field without the condition would be a
/// control that silently does nothing; not offering it at all is the defect that was reported.
///
/// ⚠ The residual is asserted too. Nothing exercises a stored server end to end —
/// `crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s `live_config` force-blanks
/// `cfg.server` before both demo logins — so the note has to say that rather than let a rendered
/// field imply a measured effect.
#[test]
fn the_conditional_jnlp_field_carries_its_condition_and_its_residual() {
    let mut duka = harness("dukascopy");
    duka.run();
    let buttons = edit_buttons(&duka);
    assert_eq!(buttons.len(), 1, "dukascopy offers exactly one editable tier (DEMO)");
    buttons[0].click();
    drop(buttons);
    duka.run();

    let fields = text_fields(&duka);
    let placeholders: Vec<String> = fields
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        placeholders,
        vec![
            "DUKASCOPY_DEMO1_LOGIN",
            "DUKASCOPY_DEMO1_PASSWORD",
            "DUKASCOPY_DEMO1_SERVER",
            "DUKASCOPY_DEMO2_LOGIN",
            "DUKASCOPY_DEMO2_PASSWORD",
            "DUKASCOPY_DEMO2_SERVER",
        ],
        "both demo accounts, WHOLE, in the loader's own per-account order"
    );
    // ⚠ NOT "masked like every other one" — that is what this panel used to do and is exactly the
    // half of the owner's report that was a design flaw rather than a missing field. A login and a
    // JNLP url NAME things; masking them is how a value an operator set six months ago becomes
    // unknowable from inside the app that wrote it. The password beside them is still masked.
    let roles: Vec<(String, Role)> = fields
        .iter()
        .zip(&placeholders)
        .map(|(f, k)| (k.clone(), f.accesskit_node().role()))
        .collect();
    for (key, role) in &roles {
        let want = match vike_connections::view::key_sensitivity(key) {
            vike_connections::view::Sensitivity::Secret => Role::PasswordInput,
            vike_connections::view::Sensitivity::Public => Role::TextInput,
        };
        assert_eq!(*role, want, "{key}");
    }
    assert_eq!(
        roles.iter().filter(|(_, r)| *r == Role::PasswordInput).count(),
        2,
        "both demo accounts' PASSWORDS are still masked: {roles:?}"
    );

    let text = tree_text(&duka);
    let note = vike_connections::view::form_note("dukascopy", "DEMO").expect("this cell has one");
    assert!(note.contains(".jnlp") && note.contains("No live smoke"), "the note itself: {note}");
    assert!(text.contains(note), "the condition must reach the screen with the form: {text}");

    // ...and no other venue's form claims a condition it does not have.
    let mut bin = harness("binance");
    bin.run();
    let buttons = edit_buttons(&bin);
    buttons.last().expect("binance offers an edit button").click();
    drop(buttons);
    bin.run();
    assert!(
        !tree_text(&bin).contains("No live smoke"),
        "the note is per-cell — binance's form must carry none"
    );
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
    // ⚠ The CELL, not the tree text. `view.rs`'s mark legend now names `not set` in every state,
    // so a `text.contains("not set")` here would pass with `tier_row` rendering nothing at all —
    // an assertion that cannot fail for its stated reason.
    assert!(
        status_cells(&duka).iter().any(|c| c == "\u{25CB} not set"),
        "the real DEMO tier still reads as unset, in its own cell: {text}"
    );
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

/// **Every DETAIL-PANE STATUS CELL on screen, verbatim** — `tier_row`'s `<glyph> <TierState
/// label>`, which is the pane's one CLAIM about what was measured for a tier.
///
/// ⚠ It exists because a whole-tree `contains("not set")` stopped being able to answer the
/// question the unreadable-store gate asks. `view.rs`'s mark legend moved to the TOP of the panel
/// (it used to be two paragraphs at the foot, laid out below the window floor and reaching no
/// pixel), so the words `not set` are now on screen in every state as a LEGEND ENTRY. The legend
/// spells its entries `<glyph> = <word>` precisely so the two remain distinguishable; this helper
/// is the reading that uses the distinction, and it is strictly sharper than the substring check
/// it replaced — a substring could never have told a cell from a footnote in the first place.
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
    // ⚠ Re-keyed from `!text.contains("not set")` when the mark legend moved to the TOP of the
    // panel. The legend names `not set` in every state (it is a legend), so the tree text can no
    // longer answer this; [`status_cells`] reads the CLAIMS instead, which is what the sentence
    // below always meant. Strictly sharper: the old form could not have told a cell from a
    // footnote, and it is the pane's cells that are forbidden from claiming a measurement.
    assert_eq!(
        status_cells(&h),
        vec![
            "? unknown — store unreadable".to_string(),
            "? unknown — store unreadable".to_string(),
            "? unknown — store unreadable".to_string()
        ],
        "NOT ONE cell may claim to have been measured: {text}"
    );
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

/// **The SECOND sentinel, and the reason this suite needed one.**
///
/// ⚠ The panel used to mask every field and read none of them back, so "no stored value is on
/// screen" was the whole rule and [`SENTINEL`] alone could state it. It is no longer the whole
/// rule: `vike_connections::view::key_sensitivity` splits the store into what AUTHENTICATES (a
/// key, a secret, a token — still never read back, still masked) and what NAMES something (a
/// server, an account number, an address, an attribution tag), and the second kind is prefilled
/// into an opened form on purpose, because a masked field that starts empty is how a server name
/// an operator set six months ago becomes unknowable from inside the app that wrote it.
///
/// So the security property is now a CLASSIFICATION and not an absence, and a test that planted
/// one sentinel could only ever assert the absence — it would pass just as well against a panel
/// that had stopped reading anything back at all, which is the failure the split was made to fix.
/// Two sentinels state both halves: this one must APPEAR (in an opened form, and nowhere else),
/// and [`SENTINEL`] must appear NOWHERE.
const PUBLIC_SENTINEL: &str = "kfbrmt5091zwycpldx4738qv";

/// The keys [`seeded_store`] plants [`PUBLIC_SENTINEL`] in — the readable half. Every other seeded
/// key gets [`SENTINEL`].
///
/// ⚠ Chosen from the store's own shapes rather than invented: a JForex account number, its server,
/// and an EIP-712 account address. `key_sensitivity` is what actually decides, and
/// `crates/vike-app-core/tests/credential_editor_completeness_gate.rs` pins its verdict per key —
/// this list is asserted AGAINST that function below, so the two cannot drift.
const PUBLIC_KEYS: [&str; 3] =
    ["DUKASCOPY_DEMO1_LOGIN", "DUKASCOPY_DEMO1_SERVER", "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS"];

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
        "DUKASCOPY_DEMO1_SERVER",
        "HYPERLIQUID_LIVE_PRIVATE_KEY",
        "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS",
        "BINANCE_LIVE_API_KEY__HEDGE",
        "BINANCE_LIVE_API_SECRET__HEDGE",
    ];
    // ⚠ WHICH sentinel a key gets is decided by `key_sensitivity` itself, not by [`PUBLIC_KEYS`]:
    // that list is then asserted equal to the classifier's own verdict, so a test claiming
    // "`_SERVER` is readable" cannot be true only because this file said so.
    let value_for = |k: &str| {
        if vike_connections::view::key_sensitivity(k) == vike_connections::view::Sensitivity::Public
        {
            PUBLIC_SENTINEL
        } else {
            SENTINEL
        }
    };
    assert_eq!(
        keys.iter().copied().filter(|k| value_for(k) == PUBLIC_SENTINEL).collect::<Vec<_>>(),
        PUBLIC_KEYS.to_vec(),
        "the readable half of the seeded store must be exactly what `key_sensitivity` says it is"
    );
    let vars: HashMap<String, String> =
        keys.iter().map(|k| ((*k).to_string(), value_for(k).to_string())).collect();

    // …and the same bytes on disk, at the path the panel is handed. Nothing in `connections_ui`
    // reads this file today; writing it is what makes the test able to FAIL if something ever
    // starts to — a SECRET read back into the edit buffers being the regression the module's own
    // security rule 1 forbids by name.
    let mut text = String::new();
    for k in keys {
        text.push_str(&format!("{k}={}\n", value_for(k)));
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

    // 2 — OPEN the LIVE form. Every SECRET buffer must start EMPTY; a read-back is the regression.
    // ⚠ binance's LIVE form is FOUR fields, not three: the generic trio plus the venue-wide
    // `BINANCE_BROKER_CODE`, which is public and readable. The store seeds no broker code, so it
    // starts empty here for a different reason — that it is not masked is the assertion.
    {
        let pencils = edit_buttons(&h);
        assert_eq!(pencils.len(), 3, "binance offers three editable tiers");
        pencils.last().expect("the LIVE row's edit button").click();
    }
    h.run();
    let fields = text_fields(&h);
    assert_eq!(fields.len(), 4, "the LIVE form is open: the API trio plus the broker code");
    for f in &fields {
        let a = f.accesskit_node();
        let key = a.placeholder().unwrap_or_default().to_string();
        let secret = vike_connections::view::key_sensitivity(&key)
            == vike_connections::view::Sensitivity::Secret;
        assert_eq!(
            a.role(),
            if secret { Role::PasswordInput } else { Role::TextInput },
            "{key}: a secret is masked and a name is not"
        );
        assert_eq!(
            a.value().unwrap_or_default(),
            "",
            "{key}: this store seeds no value for it, so the buffer must be empty — and for a \
             SECRET that holds whatever the store says, because blank means keep"
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

/// **THE OTHER HALF OF THE SAME RULE: a NAME is read back, and only inside an opened form.**
///
/// ⚠ [`no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor`] asserts an
/// ABSENCE, and an absence is satisfied by a panel that reads nothing back at all — which is the
/// state the owner reported as *"editing Dukascopy credentials doesn't show them"*. This test is
/// the claim that makes that one a classification rather than a blanket: the dukascopy DEMO form
/// PREFILLS the account number and the JNLP url it already holds, renders them UNMASKED so they can
/// actually be read, and the password beside them is still empty and still a `Role::PasswordInput`.
///
/// ⚠ **The scope is the FORM.** The rail and the detail pane render key NAMES and never values, so
/// the readable sentinel must be absent until the ✎ is clicked — asserted in that order, because
/// "it appears somewhere" would be satisfied by a panel painting the store across the rail.
#[test]
fn a_readable_field_is_prefilled_unmasked_inside_the_form_and_nowhere_else() {
    let (dir, vars) = seeded_store();
    let store = dir.path().join("secrets.env");
    let grids = AccountGrids::from_vars(&vars);
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { store: &store, journal: None, now_ms: 0 };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 900.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
    });
    h.run();
    assert!(
        !tree_text(&h).contains(PUBLIC_SENTINEL),
        "a readable VALUE is not painted on the rail — the rail renders key names"
    );

    // Select dukascopy, then open its one editable tier.
    {
        let picks = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("dukascopy")
        });
        assert_eq!(picks.len(), 1, "dukascopy is in the rail exactly once");
        picks[0].click();
    }
    h.run();
    assert!(
        !tree_text(&h).contains(PUBLIC_SENTINEL),
        "…nor in the detail pane, which also renders only names"
    );
    assert_no_sentinel(&h, "dukascopy's detail pane");

    {
        let pencils = edit_buttons(&h);
        assert_eq!(pencils.len(), 1, "dukascopy offers one editable tier");
        pencils[0].click();
    }
    h.run();

    let fields = text_fields(&h);
    assert_eq!(fields.len(), 6, "both demo accounts, three fields each");
    // ⚠ The key each field writes is read from `edit_fields`, NOT from the widget's placeholder:
    // egui files `hint_text` as the accessibility `placeholder` and a PREFILLED field has a value
    // instead, so reading the key off the tree would answer `""` for exactly the fields this test
    // exists to check — and `key_sensitivity` fails closed, so every one of them would then be
    // demanded to be masked. Zipping against the table is what keeps the check pointed at the
    // field it means.
    let keys: Vec<String> = vike_connections::view::edit_fields("dukascopy", "DEMO")
        .into_iter()
        .map(|(_, k)| k)
        .collect();
    assert_eq!(keys.len(), fields.len(), "the form renders one widget per table row");
    let mut prefilled = 0usize;
    for (f, key) in fields.iter().zip(&keys) {
        let a = f.accesskit_node();
        let value = a.value().unwrap_or_default().to_string();
        match vike_connections::view::key_sensitivity(key) {
            vike_connections::view::Sensitivity::Secret => {
                assert_eq!(a.role(), Role::PasswordInput, "{key} must stay masked");
                assert_eq!(value, "", "{key} is a SECRET and must never be read back");
            }
            vike_connections::view::Sensitivity::Public => {
                assert_eq!(
                    a.role(),
                    Role::TextInput,
                    "{key} names something — masking it is what made it unreadable"
                );
                if PUBLIC_KEYS.contains(&key.as_str()) {
                    assert_eq!(value, PUBLIC_SENTINEL, "{key} must show what the store holds");
                    prefilled += 1;
                }
            }
        }
    }
    drop(fields);
    assert_eq!(prefilled, 2, "DEMO1's login and server are the two seeded readable fields");

    // The SECRET sentinel is still nowhere, with the form open over the same store.
    assert_no_sentinel(&h, "dukascopy's opened DEMO editor");

    // ...and nothing was written.
    assert!(
        std::fs::read_to_string(&store).expect("re-read the store").contains(PUBLIC_SENTINEL),
        "the store is untouched"
    );
}
