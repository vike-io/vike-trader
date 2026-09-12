//! **The Data Manager's Venues (per-venue arming) sub-tab, gated end to end.**
//!
//! `vike-app` is not in the derived CI roster and
//! `crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs` exists to keep it that way, so every
//! decision this screen makes lives in `vike-app-core` and is gated here. Four properties, each
//! with the defect it exists for:
//!
//! 1. **The Effective column REACHES the screen.** The whole point of the tab is answering *"I set
//!    live and it is still paper"*, and the answer is a cell of text. A pure test of
//!    `effective_cell` stays green with the renderer wired to nothing, which is why the render
//!    half is driven through the REAL `venues_tab_content` and read back off the accessibility
//!    tree.
//! 2. **A LOOSENING click writes nothing.** It opens the typed-confirm prompt instead. Driven as a
//!    real click on the real button, because "the renderer routed the click to `apply_arming`
//!    instead of `begin_confirm`" is precisely the bug a state-machine-only test cannot see.
//! 3. **A TIGHTENING click writes immediately**, with no ceremony.
//! 4. **A write touches exactly one key**, and every other byte of `policy.toml` — comments,
//!    blank lines, ordering, sibling keys — survives.
//!
//! ⚠ Every write in this file lands in a `tempfile::tempdir()`. `vike_config::set_setting` takes
//! the directory as a PARAMETER and this tab threads it from the binary's boot walk, which is what
//! makes the file possible at all: a tab that resolved its own settings directory could not be
//! tested without editing the developer's real `policy.toml`.

use std::collections::HashMap;
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_app_core::split_plane::AppMode;
use vike_app_core::tool_views::{
    ArmDirection, ArmEdit, VenueArmingInputs, apply_arming, arm_direction, venues_tab_content,
};
use vike_config::{ArmingBlock, VenueArming, VenueMode};
use vike_model::account_keys::AccountLabel;

/// 2026-08-24T00:00:00Z.
const T: i64 = 1_787_616_000_000;

const POLICY: &str = "policy.toml";

// ------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------

fn row(
    venue: &'static str,
    ceiling: VenueMode,
    effective: VenueMode,
    block: ArmingBlock,
) -> VenueArming {
    // The DEFAULT account — the only one a box with no `[accounts]` table has, and the row every
    // assertion in this file was written against.
    VenueArming { venue, label: AccountLabel::Default, ceiling, effective, block }
}

/// A three-row fixture spanning the three interesting shapes: a clear row, a row capped BELOW its
/// ceiling with a named cause, and a disarmed one.
fn rows() -> Vec<VenueArming> {
    vec![
        row("binance", VenueMode::Live, VenueMode::Demo, ArmingBlock::MainnetSwitchUnset),
        row("bybit", VenueMode::Paper, VenueMode::Paper, ArmingBlock::Disarmed),
        row("okx", VenueMode::Demo, VenueMode::Demo, ArmingBlock::None),
    ]
}

/// Credentials for the grid's third column, through the same producer the Connections grid uses —
/// so this fixture is a credential MAP, never a hand-built status list.
fn creds_vars() -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("BINANCE_DEMO_API_KEY".to_string(), "k".to_string());
    vars.insert("BINANCE_DEMO_API_SECRET".to_string(), "s".to_string());
    vars
}

fn inputs(dir: Option<&Path>, mode: AppMode, rows: Vec<VenueArming>) -> VenueArmingInputs {
    VenueArmingInputs::new(rows, &creds_vars(), dir.map(Path::to_path_buf), mode)
}

/// A `policy.toml` with comments, a sibling ceiling, an unrelated flat key and a trailing comment —
/// every shape the byte-preservation gate cares about.
const BEFORE: &str = "\
# risk ceilings — reviewed 2026-08-24
max_leverage = 2.0  # perp desk only

[venues]
# binance is the only account with a live budget
binance = \"demo\"  # raise after the recon soak
okx = \"demo\"
";

fn seeded_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(POLICY), BEFORE).expect("seed");
    dir
}

fn policy_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(POLICY)).expect("policy.toml")
}

// ------------------------------------------------------------------------------------------
// The harness
// ------------------------------------------------------------------------------------------

/// One persistent harness over the REAL `venues_tab_content`, owning the edit state across frames
/// the way a tool window does.
struct Screen {
    harness: Harness<'static, ()>,
}

impl Screen {
    fn new(inputs: VenueArmingInputs) -> Self {
        let state = std::cell::RefCell::new(ArmEdit::Idle);
        let harness = Harness::builder().with_size(egui::vec2(1100.0, 700.0)).build_ui(move |ui| {
            venues_tab_content(ui, Some(&inputs), &mut state.borrow_mut(), None, T);
        });
        Self { harness }
    }

    fn run(&mut self) {
        self.harness.run();
    }

    /// Every accessibility node's rendered text, in tree order — labels file their text under
    /// `value`, buttons and selectables under `label`.
    fn texts(&self) -> Vec<String> {
        self.harness
            .root()
            .children_recursive()
            .filter_map(|n| {
                let a = n.accesskit_node();
                a.label().map(|s| s.to_string()).or_else(|| a.value().map(|s| s.to_string()))
            })
            .collect()
    }

    fn contains(&self, needle: &str) -> bool {
        self.texts().iter().any(|t| t.contains(needle))
    }

    /// The clickable switch cells whose label is `mode`, in tree (row) order.
    fn switches<'t>(&'t self, mode: &str) -> Vec<Node<'t>> {
        self.harness
            .root()
            .children_recursive()
            .filter(|n| {
                let a = n.accesskit_node();
                matches!(a.role(), Role::Button | Role::CheckBox | Role::RadioButton)
                    && a.label().as_deref() == Some(mode)
            })
            .collect()
    }

    fn text_fields<'t>(&'t self) -> Vec<Node<'t>> {
        self.harness
            .root()
            .children_recursive()
            .filter(|n| {
                matches!(
                    n.accesskit_node().role(),
                    Role::TextInput | Role::MultilineTextInput | Role::PasswordInput
                )
            })
            .collect()
    }
}

// ------------------------------------------------------------------------------------------
// 1. The columns reach the screen
// ------------------------------------------------------------------------------------------

/// **Every column of every row renders**, and the Effective cell carries BOTH the tier and the
/// named cause when something is capping it.
///
/// Reddens on the renderer dropping a column, on `effective_cell` being replaced by the ceiling
/// (the "I set live and it is still paper" defect, rendered), and on the block badge being
/// swallowed — none of which a test of the pure cell functions alone can see.
#[test]
fn every_column_of_every_row_reaches_the_screen() {
    let dir = seeded_dir();
    let mut screen = Screen::new(inputs(Some(dir.path()), AppMode::LocalCore, rows()));
    screen.run();

    for venue in ["binance", "bybit", "okx"] {
        assert!(screen.contains(venue), "the Venue column is missing {venue}");
    }
    // The headers themselves, so a silently-empty grid cannot pass.
    for header in ["Venue", "Mode", "Credentials", "Effective", "Switch"] {
        assert!(screen.contains(header), "the {header} header is missing");
    }
    // binance: ceiling `live`, effective `demo`, and the cause NAMED.
    assert!(
        screen.contains(ArmingBlock::MainnetSwitchUnset.as_str()),
        "a capped row must name its cause on screen: {:?}",
        screen.texts()
    );
    // Credentials: tier NAMES with presence, from the real `credential_status` over `creds_vars` —
    // binance has DEMO keys and nothing else, so the cell must show exactly that split.
    assert!(
        screen.contains("\u{25CF} demo"),
        "the credential column must mark binance's demo tier PRESENT: {:?}",
        screen.texts()
    );
    assert!(screen.contains("\u{25CB} live"), "…and its live tier ABSENT: {:?}", screen.texts());
    // …and NOTHING that looks like a credential key or value reaches the tree.
    for text in screen.texts() {
        assert!(!text.contains("API_KEY"), "a credential key name reached the screen: {text}");
        assert!(!text.contains("API_SECRET"), "a credential key name reached the screen: {text}");
    }
}

/// A THIN (`--observe`-only) build renders the ceilings it can read and says plainly that it
/// cannot answer the Effective column — never a tier it does not know, and never a blank cell that
/// would read as "paper".
#[test]
fn a_thin_build_renders_the_ceilings_and_refuses_to_guess_an_effective_tier() {
    let dir = seeded_dir();
    let thin_rows: Vec<VenueArming> = rows()
        .into_iter()
        .map(|r| VenueArming { effective: r.ceiling, block: ArmingBlock::NoMountInThisBuild, ..r })
        .collect();
    let mut screen = Screen::new(inputs(Some(dir.path()), AppMode::ObserveOnly, thin_rows));
    screen.run();

    assert!(
        screen.contains("links no venue mount"),
        "the thin build must SAY it cannot answer: {:?}",
        screen.texts()
    );
    assert!(screen.contains("binance"), "the ceilings are still rendered");
    assert!(screen.contains("—"), "the Effective cell is a dash, not a guessed tier");
}

/// A fat build that is OBSERVING points at the surface that edits the REMOTE node's policy, rather
/// than letting the operator believe this file governs the backend they are watching.
#[test]
fn an_observing_build_points_at_the_backend_settings_surface() {
    let dir = seeded_dir();
    let mut screen = Screen::new(inputs(Some(dir.path()), AppMode::ObserveWithFeeds, rows()));
    screen.run();
    assert!(screen.contains("Backend settings"), "{:?}", screen.texts());
}

/// A boot that resolved NO project disables the switch and says why — it never guesses a path.
#[test]
fn a_project_less_boot_disables_the_switch_and_names_the_reason() {
    let mut screen = Screen::new(inputs(None, AppMode::LocalCore, rows()));
    screen.run();
    assert!(screen.contains("no settings directory"), "{:?}", screen.texts());
    for mode in ["paper", "demo", "live"] {
        for node in screen.switches(mode) {
            assert!(
                node.accesskit_node().is_disabled(),
                "the {mode} switch must be inert with nowhere to write"
            );
        }
    }
}

// ------------------------------------------------------------------------------------------
// 2 & 3. The switch, clicked for real
// ------------------------------------------------------------------------------------------

/// **A LOOSENING click writes NOTHING.** It opens the typed-confirm prompt, with an EMPTY buffer.
///
/// This is the gate that a state-machine test cannot stand in for: the defect it catches is the
/// renderer routing a raise straight into `apply_arming`, which every pure test of `can_apply`
/// would sail past.
#[test]
fn clicking_a_higher_mode_opens_the_confirm_and_writes_nothing() {
    let dir = seeded_dir();
    let before = policy_text(dir.path());
    let mut screen = Screen::new(inputs(Some(dir.path()), AppMode::LocalCore, rows()));
    screen.run();
    assert!(screen.text_fields().is_empty(), "no confirm box before the click");

    // Row 2 is bybit (ceiling `paper`); raising it to `live` is the most dangerous move on the
    // screen, so it is the one driven here.
    let live_switches = screen.switches("live");
    assert_eq!(live_switches.len(), 3, "one live switch per row");
    live_switches[1].click();
    drop(live_switches);
    screen.run();

    assert_eq!(policy_text(dir.path()), before, "a loosening CLICK must not write a byte");
    let fields = screen.text_fields();
    assert_eq!(fields.len(), 1, "exactly one confirm box opened");
    assert!(
        screen.contains("policy.venues.bybit"),
        "the prompt must name the exact key the operator has to type: {:?}",
        screen.texts()
    );
    assert!(
        fields[0].accesskit_node().value().unwrap_or_default().is_empty(),
        "the confirm buffer is NEVER pre-filled"
    );
}

/// **A TIGHTENING click writes immediately** — no confirm box, no ceremony. An action that can only
/// REDUCE authority must not be ceremonious.
#[test]
fn clicking_a_lower_mode_writes_at_once_with_no_ceremony() {
    let dir = seeded_dir();
    let mut screen = Screen::new(inputs(Some(dir.path()), AppMode::LocalCore, rows()));
    screen.run();

    // Row 1 is binance, ceiling `live` → click `paper`.
    let paper = screen.switches("paper");
    assert_eq!(paper.len(), 3);
    paper[0].click();
    drop(paper);
    screen.run();

    assert!(screen.text_fields().is_empty(), "a tightening asks for no confirm");
    let after = policy_text(dir.path());
    assert!(after.contains("binance = \"paper\""), "the write landed: {after}");
    assert!(
        screen.contains("restart"),
        "the row must name the restart requirement: {:?}",
        screen.texts()
    );
}

// ------------------------------------------------------------------------------------------
// 4. The write itself
// ------------------------------------------------------------------------------------------

/// **The confirm gate lives in the WRITE, not only in the button.** A loosening with no confirm,
/// or with a near-miss, changes no byte — the same rule the daemon applies server-side, so a second
/// caller cannot route around the ceremony.
#[test]
fn a_loosening_without_the_exact_confirm_writes_no_byte() {
    let dir = seeded_dir();
    let before = policy_text(dir.path());

    let refusals = [
        (None, "no confirm at all"),
        (Some(""), "an empty confirm"),
        (Some("policy.venues"), "a truncated key"),
        (Some("binance"), "the venue alone"),
        (Some("policy.venues.binance "), "a trailing space"),
        (Some("policy.venues.okx"), "another row's key"),
    ];
    for (confirm, what) in refusals {
        let err = apply_arming(
            Some(dir.path()),
            "policy.venues.binance",
            "binance",
            VenueMode::Demo,
            VenueMode::Live,
            confirm,
            None,
            T,
        )
        .expect_err("a loosening without the exact key must be refused");
        assert!(err.contains("policy.venues.binance"), "{what}: the refusal names the key: {err}");
        assert_eq!(policy_text(dir.path()), before, "{what}: a refused write changed a byte");
    }

    // …and the CONTROL, which is what makes the refusals above the CONFIRM's doing rather than a
    // broken fixture: the exact key writes.
    apply_arming(
        Some(dir.path()),
        "policy.venues.binance",
        "binance",
        VenueMode::Demo,
        VenueMode::Live,
        Some("policy.venues.binance"),
        None,
        T,
    )
    .expect("the exact key unlocks the write");
    assert!(policy_text(dir.path()).contains("binance = \"live\""));
}

/// A TIGHTENING needs no confirm at the write either — and it is the only direction that does not.
#[test]
fn a_tightening_writes_with_no_confirm() {
    let dir = seeded_dir();
    apply_arming(
        Some(dir.path()),
        "policy.venues.binance",
        "binance",
        VenueMode::Demo,
        VenueMode::Paper,
        None,
        None,
        T,
    )
    .expect("a tightening is a plain action");
    assert!(policy_text(dir.path()).contains("binance = \"paper\""));
    assert_eq!(arm_direction(VenueMode::Demo, VenueMode::Paper), ArmDirection::Tighten);
}

/// **THE byte-preservation gate: exactly the dotted key moves, and every other line of
/// `policy.toml` survives verbatim** — comments, the trailing comment on the edited line, blank
/// lines, ordering and the sibling ceiling.
///
/// `policy.toml` is hand-edited and its comments carry the REASON a ceiling is where it is; a
/// switch that re-serialized the parsed model would delete every one of them.
#[test]
fn a_write_moves_one_key_and_leaves_every_other_byte_alone() {
    let dir = seeded_dir();
    apply_arming(
        Some(dir.path()),
        "policy.venues.binance",
        "binance",
        VenueMode::Demo,
        VenueMode::Live,
        Some("policy.venues.binance"),
        None,
        T,
    )
    .expect("the write");

    let after = policy_text(dir.path());
    let (b, a): (Vec<&str>, Vec<&str>) = (BEFORE.lines().collect(), after.lines().collect());
    assert_eq!(b.len(), a.len(), "no line added or removed:\n{after}");
    for (before_line, after_line) in b.iter().zip(&a) {
        if before_line.starts_with("binance") {
            assert_eq!(
                *after_line, "binance = \"live\"  # raise after the recon soak",
                "the value moved and its trailing comment survived"
            );
        } else {
            assert_eq!(after_line, before_line, "an untouched line must be untouched BYTES");
        }
    }

    // …and the result is a file the next boot accepts, with the ceiling actually moved.
    let settings = vike_config::load(Some(dir.path()), &HashMap::new()).expect("it still loads");
    assert_eq!(settings.policy.venues.get("binance"), VenueMode::Live);
    assert_eq!(settings.policy.venues.get("okx"), VenueMode::Demo, "the sibling is untouched");
    // ⚠ The unrelated FLAT ceiling in the fixture is asserted by the byte comparison above and
    // deliberately NOT re-read off `settings` here: `crates/vike-config/tests/policy_is_consumed.rs`
    // scans the tree for reads of a field it has classified as unconsumed and fails the PR that
    // adds one — a test fixture touching it would read as production consumption. The byte loop is
    // the stronger check anyway (it pins the trailing comment too).
}

/// **The Mode column follows the FILE, not this process's boot-time load** — so a write the
/// operator just made is visible on the next frame instead of looking like it did nothing.
///
/// The write is restart-to-apply for the MOUNT, and that is what the note beside the row says; a
/// Mode column that also lagged would be indistinguishable from a broken switch.
#[test]
fn the_ceilings_are_re_read_from_disk_so_a_write_is_visible_at_once() {
    use vike_app_core::tool_views::reload_venue_ceilings;

    let dir = seeded_dir();
    let boot = reload_venue_ceilings(Some(dir.path())).expect("the seeded file loads");
    assert_eq!(boot.get("binance"), VenueMode::Demo);

    apply_arming(
        Some(dir.path()),
        "policy.venues.binance",
        "binance",
        VenueMode::Demo,
        VenueMode::Live,
        Some("policy.venues.binance"),
        None,
        T,
    )
    .expect("the write");

    let after = reload_venue_ceilings(Some(dir.path())).expect("still loads");
    assert_eq!(after.get("binance"), VenueMode::Live, "the reload sees the write");

    // No directory, and a file the loader refuses, both answer `None` — the caller keeps its
    // boot-time ceilings rather than being handed an empty map that would read as all-paper.
    assert!(reload_venue_ceilings(None).is_none());
    std::fs::write(dir.path().join(POLICY), "this is not toml = = =").expect("break it");
    assert!(
        reload_venue_ceilings(Some(dir.path())).is_none(),
        "a broken file must not degrade to an all-paper map — that would render every venue \
         disarmed and blame the operator's policy for a parse error"
    );
}

/// A key the ROSTER does not carry is refused by the loader BEFORE a byte lands — the writer
/// validates the would-be file with the same `apply` a restart would run, so a typo cannot arm
/// something that does not exist and cannot corrupt the file trying.
#[test]
fn a_write_naming_no_venue_is_refused_before_a_byte_lands() {
    let dir = seeded_dir();
    let before = policy_text(dir.path());
    let err = apply_arming(
        Some(dir.path()),
        "policy.venues.binanc",
        "binanc",
        VenueMode::Demo,
        VenueMode::Live,
        Some("policy.venues.binanc"),
        None,
        T,
    )
    .expect_err("a ceiling on a venue that does not exist must not be written");
    assert!(err.contains("binanc"), "{err}");
    assert_eq!(policy_text(dir.path()), before, "a refused write changes NO byte");
}

/// With no settings directory there is nowhere to write, and the refusal SAYS so instead of
/// guessing a path — the `$VIKE_SETTINGS_DIR`-blind-resolver defect, refused rather than repeated.
#[test]
fn no_settings_directory_refuses_by_name() {
    let err = apply_arming(
        None,
        "policy.venues.binance",
        "binance",
        VenueMode::Demo,
        VenueMode::Paper,
        None,
        None,
        T,
    )
    .expect_err("nowhere to write");
    assert!(err.contains("no settings directory"), "{err}");
}

/// The accepted write is RECORDED — the change journal's first production `set_setting` record —
/// carrying the file, the key, the old value and the new one, under the GUI actor.
#[test]
fn an_accepted_write_lands_in_the_change_journal() {
    use vike_model::change_journal::{
        CHANGES_SUBDIR, ChangeJournal, KIND_SET_SETTING, Proc, month_file_name,
    };

    let dir = seeded_dir();
    let state = dir.path().join("state");
    let journal = ChangeJournal::in_state_dir(&state, Proc::new("vike-test", 4711, "0.1.0"));

    apply_arming(
        Some(dir.path()),
        "policy.venues.binance",
        "binance",
        VenueMode::Demo,
        VenueMode::Live,
        Some("policy.venues.binance"),
        Some(&journal),
        T,
    )
    .expect("the write");

    let ledger = std::fs::read_to_string(state.join(CHANGES_SUBDIR).join(month_file_name(T)))
        .expect("the ledger file");
    assert_eq!(ledger.lines().count(), 1, "one action, one record:\n{ledger}");
    let record: serde_json::Value = serde_json::from_str(ledger.trim()).expect("one JSON line");
    assert_eq!(record["kind"], KIND_SET_SETTING);
    assert_eq!(record["target"]["file"], "policy.toml");
    assert_eq!(record["target"]["key"], "policy.venues.binance");
    assert_eq!(record["target"]["old"], "\"demo\"");
    assert_eq!(record["target"]["new"], "\"live\"");
    assert_eq!(record["actor"]["origin"], "gui");
    // The change is on disk and the RUNNING process keeps its boot value — the outcome that says
    // exactly that, rather than a bare `applied` which would claim it is in force.
    assert_eq!(record["outcome"], "applied_pending_restart");
}

/// A REFUSED write is recorded too. "Somebody tried to raise a ceiling and was refused" is as much
/// an audit fact as a success, and a ledger that records only successes cannot answer whether
/// anyone tried.
#[test]
fn a_refused_write_is_recorded_as_refused() {
    use vike_model::change_journal::{CHANGES_SUBDIR, ChangeJournal, Proc, month_file_name};

    let dir = seeded_dir();
    let state = dir.path().join("state");
    let journal = ChangeJournal::in_state_dir(&state, Proc::new("vike-test", 4711, "0.1.0"));

    apply_arming(
        Some(dir.path()),
        "policy.venues.binanc",
        "binanc",
        VenueMode::Demo,
        VenueMode::Live,
        Some("policy.venues.binanc"),
        Some(&journal),
        T,
    )
    .expect_err("the loader refuses a venue that does not exist");

    let ledger = std::fs::read_to_string(state.join(CHANGES_SUBDIR).join(month_file_name(T)))
        .expect("the ledger file");
    let record: serde_json::Value = serde_json::from_str(ledger.trim()).expect("one JSON line");
    assert_eq!(record["outcome"], "refused");
    assert_eq!(record["target"]["key"], "policy.venues.binanc");
}

/// ⚠ **The confirm-gated refusal is NOT journalled, and that is deliberate** — stated here so the
/// absence is a decision rather than an oversight. It never reaches the writer: nothing was
/// attempted against the file, the UI never let the click through, and a ledger line for every
/// keystroke that is not yet the key would drown the records that matter.
#[test]
fn a_confirm_refusal_never_reaches_the_ledger() {
    use vike_model::change_journal::{CHANGES_SUBDIR, ChangeJournal, Proc, month_file_name};

    let dir = seeded_dir();
    let state = dir.path().join("state");
    let journal = ChangeJournal::in_state_dir(&state, Proc::new("vike-test", 4711, "0.1.0"));

    apply_arming(
        Some(dir.path()),
        "policy.venues.binance",
        "binance",
        VenueMode::Demo,
        VenueMode::Live,
        Some("nope"),
        Some(&journal),
        T,
    )
    .expect_err("the confirm gate");

    assert!(
        !state.join(CHANGES_SUBDIR).join(month_file_name(T)).exists(),
        "the confirm gate writes no ledger line"
    );
}

// ------------------------------------------------------------------------------------------
// 5. The Credentials column is about the ROW'S ACCOUNT
// ------------------------------------------------------------------------------------------

/// A legal label, built through the ONE validator so this fixture cannot pin a spelling
/// `vike_model::account_keys` would refuse.
fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The DISTINCT Credentials cells on screen, sorted — every rendered text carrying all three tier
/// names. Deduplicated because egui's accessibility tree exposes a label twice (the widget node and
/// its inner text node), which is a property of the harness rather than of this column.
fn credential_cells(screen: &Screen) -> Vec<String> {
    let mut cells: Vec<String> = screen
        .texts()
        .into_iter()
        .filter(|t| t.contains("sim") && t.contains("demo") && t.contains("live"))
        .collect();
    cells.sort();
    cells.dedup();
    cells
}

/// A row for a LABELLED account of `venue`.
fn labelled_row(venue: &'static str, label: AccountLabel) -> VenueArming {
    VenueArming {
        venue,
        label,
        ceiling: VenueMode::Demo,
        effective: VenueMode::Demo,
        block: ArmingBlock::None,
    }
}

/// **THE DEFECT, on screen.** The store holds the DEFAULT binance account's demo keys and nothing
/// for the labelled one. Both rows render; the labelled row must NOT show the default account's
/// dots.
///
/// The cell used to come from `credential_status`, keyed per VENUE, so both rows rendered the same
/// three dots and the labelled row claimed credentials belonging to another account.
///
/// Read off the REAL accessibility tree, through the REAL `venues_tab_content`, because the pure
/// `credentials_cell` stays green with the renderer wired to the wrong grid.
#[test]
fn a_labelled_account_row_shows_its_own_credentials_not_the_default_accounts() {
    let dir = seeded_dir();
    let rows = vec![
        row("binance", VenueMode::Demo, VenueMode::Demo, ArmingBlock::None),
        labelled_row("binance", alt()),
    ];
    // `creds_vars` configures BINANCE_DEMO_* for the default account only.
    let inputs = VenueArmingInputs::new(
        rows,
        &creds_vars(),
        Some(dir.path().to_path_buf()),
        AppMode::LocalCore,
    );

    // The cell resolution itself, per row — the two rows must disagree.
    assert!(
        inputs.creds_for("binance", &AccountLabel::Default).expect("the default row").demo,
        "the default account IS configured"
    );
    assert!(
        inputs.creds_for("binance", &alt()).is_none_or(|c| !c.demo),
        "the ALT account has no keys of its own and must not borrow the default account's"
    );

    // …and it reaches the screen: the two rows render DIFFERENT credential cells, one claiming
    // demo and one denying it.
    //
    // ⚠ The DISTINCT cell texts, not a count of nodes: egui's accessibility tree carries each
    // label twice (the widget and its inner text), so a raw occurrence count answers 2 for a
    // correct single row and would have to be written to expect that — a number with nothing
    // behind it. What the column must guarantee is that the two rows DISAGREE.
    let mut screen = Screen::new(inputs);
    screen.run();
    let cells = credential_cells(&screen);
    assert_eq!(
        cells.len(),
        2,
        "the two rows must render two DIFFERENT credential cells: {:?}",
        screen.texts()
    );
    assert!(
        cells.iter().any(|c| c.contains("\u{25CF} demo")),
        "the default row claims demo: {cells:?}"
    );
    assert!(
        cells.iter().any(|c| c.contains("\u{25CB} demo")),
        "…and the labelled row denies it: {cells:?}"
    );
    // The row is still NAMED as an account, so the two cells are attributable.
    assert!(
        screen.texts().iter().any(|t| t.contains("account `ALT`")),
        "the labelled row must name its account: {:?}",
        screen.texts()
    );
}

/// The mirror image: `__ALT` keys light the LABELLED row and leave the default one dark. This is
/// the direction that proves the column reads the label rather than merely failing to find keys.
#[test]
fn a_labelled_accounts_own_keys_light_its_row_and_only_its_row() {
    let dir = seeded_dir();
    let mut vars = HashMap::new();
    vars.insert("BINANCE_DEMO_API_KEY__ALT".to_string(), "k".to_string());
    vars.insert("BINANCE_DEMO_API_SECRET__ALT".to_string(), "s".to_string());
    let rows = vec![
        row("binance", VenueMode::Demo, VenueMode::Demo, ArmingBlock::None),
        labelled_row("binance", alt()),
    ];
    let inputs =
        VenueArmingInputs::new(rows, &vars, Some(dir.path().to_path_buf()), AppMode::LocalCore);
    assert!(
        inputs.creds_for("binance", &alt()).expect("the ALT grid").demo,
        "the ALT account's own keys must light its row"
    );
    assert!(
        !inputs.creds_for("binance", &AccountLabel::Default).expect("the default grid").demo,
        "…and the DEFAULT row must stay dark"
    );

    let mut screen = Screen::new(inputs);
    screen.run();
    let cells = credential_cells(&screen);
    assert_eq!(cells.len(), 2, "the two rows must disagree: {:?}", screen.texts());
    assert!(
        cells.iter().any(|c| c.contains("\u{25CF} demo")),
        "the ALT row claims demo: {cells:?}"
    );
    assert!(
        cells.iter().any(|c| c.contains("\u{25CB} demo")),
        "…and the default row denies it: {cells:?}"
    );
    // …and NOTHING that looks like a credential key or value reaches the tree — including the
    // LABEL half of a key name, which is the new string this feature could have leaked.
    for text in screen.texts() {
        assert!(!text.contains("API_KEY"), "a credential key name reached the screen: {text}");
        assert!(!text.contains("API_SECRET"), "a credential key name reached the screen: {text}");
        assert!(!text.contains("__ALT"), "a labelled key name reached the screen: {text}");
    }
}

/// **A single-account box is unchanged.** Every row is a default-account row, so no labelled grid
/// is built at all and every cell resolves through the same `Vec` it always did — asserted against
/// `credential_status`, the producer the column used before this existed.
#[test]
fn a_single_account_box_renders_the_same_credentials_column_it_always_did() {
    let dir = seeded_dir();
    let inputs = inputs(Some(dir.path()), AppMode::LocalCore, rows());
    assert!(inputs.labelled_creds.is_empty(), "a box with no labelled row builds no second grid");
    let before = vike_connections::credential_status(&creds_vars());
    assert_eq!(inputs.creds, before, "the default grid is the venue-wide grid, row for row");
    for r in &inputs.rows {
        assert_eq!(
            inputs.creds_for(r.venue, &r.label),
            before.iter().find(|c| c.venue == r.venue),
            "{}'s cell must resolve exactly as the venue-keyed lookup did",
            r.venue
        );
    }
}
