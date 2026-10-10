//! egui render of the Connections tool's **Credentials** tab — a venue RAIL, a per-venue DETAIL
//! pane, and in-app masked key editing.
//!
//! Connect/disconnect controls are NOT here: the live backend connection is process-level state
//! rendered by the tool's own ambient strip (`vike_app_core::ui::tool_views::connections`), identical
//! on both tabs, so that one fact has one rendering.
//!
//! # ⚠⚠ THE TWO AXES ARE GOVERNED BY OPPOSITE RULES, AND BOTH WERE GOT WRONG ONCE
//!
//! A tool window AUTO-SIZES TO ITS CONTENT on BOTH axes — `egui-0.36.1`'s `Window::show_dyn`
//! builds its `Resize` with `.with_stroke(false)` and then `resizable(false)`, and `Resize::end`
//! then reports `size[d] = last_content_size[d]`, which `Resize::begin` feeds back as
//! `desired_size = desired_size.max(last_content_size)`. So the widest and the tallest thing this
//! module (this file and its children under `view/`) draws is not merely a layout choice: it is an
//! INSTRUCTION about how big the Connections
//! window should be. The owner read the result off a live capture as *"i see ui is disbalanced"*
//! TWICE, and the two reports were opposite defects:
//!
//! * **Round one — content demanding a window it did not need.** A ~250-character legend SENTENCE
//!   as a wrapping `Label` (a `Label` wraps at `ui.available_width()`, so in a maximized window it
//!   laid out on ONE LINE, edge to edge, above a panel whose real content was ~700pt), and a rail
//!   allocated `egui::vec2(RAIL_W, ui.available_height())` with a vertical `ui.separator()` beside
//!   it taking the same height again. A ~1250pt window whose rail ended at y≈600 and whose detail
//!   pane ended at y≈400, with `connections_body`'s foot strip correctly pinned to a floor 600pt
//!   below the last thing it described. The strip was never the defect.
//! * **Round two — content REFUSING the window it was given.** The cure for round one was a
//!   `PANEL_MAX_W` measure over the whole panel, and that capped the column the approved design
//!   leaves uncapped: the grid is `minmax(190px, 232px) minmax(0, 1fr)`, so the RAIL is bounded
//!   and the DETAIL pane is `1fr`. Measured on a maximized 2560x1600 live capture: an ~866pt
//!   column hugging the top-left with every rule stopping a third of the way across. The
//!   tombstone above [`connections::NOTE_W`] carries it.
//!
//! **The rule that falls out, and it is the one to carry into any future edit here:**
//!
//! * **WIDTH — FILL IT.** `ui.available_width()` is read outright by the panel container and by
//!   the detail column. That is a FIXED POINT of the sizer's feedback line rather than a runaway
//!   (`x.max(x) == x`), and it is verified by
//!   `crates/vike-connections/tests/panel/layout.rs`'s
//!   `the_windows_sizing_loop_converges_with_a_filling_detail_pane`, which drives the real
//!   `egui::Resize`. The things that may NOT fill it are PROSE ([`connections::NOTE_W`]) and the RAIL
//!   ([`connections::RAIL_MIN_W`]/[`connections::RAIL_MAX_W`]) — a paragraph and a name column are content with a natural
//!   width, and a `Label` wrapping at the arena is the one shape that genuinely grows without
//!   bound.
//! * **HEIGHT — NEVER READ IT.** `ui.available_height()` appears nowhere in this module (this file or any child under `view/`) and may not.
//!   There is no vertical analogue of "fill": the panel is a FORM, its natural height is the sum
//!   of its rows, and a window taller than that is slack the foot strip's pinning already accounts
//!   for. A greedy height read is round one again.
//!
//! # ⚠ The grid is GONE, and what replaced each of its columns
//!
//! This panel used to be one 5-column `egui::Grid` — `Venue | Status | Sim | Demo | Live` — with a
//! ●/○ dot per credential cell and a hover naming the key. Three things were wrong with it and
//! each is now a distinct piece of this module:
//!
//! * **The `Status` column and the credential dots sat in one row of one table**, which is exactly
//!   the two-facts-one-glyph confusion [`crate::summary`]'s module doc argues against. Status is
//!   now a LABELLED cell in the detail pane, reading [`crate::summary::FeedFact`] — which, unlike
//!   the old `live.get(venue).copied().unwrap_or_default()`, does not render "no producer in this
//!   build" as the `Unknown` a silent producer gives.
//! * **A tier a venue does not HAVE rendered as the same hollow ring as an unset one.** The only
//!   difference was a missing ✏ and a tooltip naming a placeholder key. `tier_row` says
//!   `not configurable` in words and names the venue.
//! * **A hover tooltip could name ONE key**, and most venues' forms write two, three or four of
//!   them. The detail pane spells every key out, `(optional)` included, without opening the form.
//!
//! **STATUS SOURCES**: the detail pane's `Status` row reads whatever live sources the binary
//! threads in via the `live` map — today the venues with a live feed producer, each parsed via
//! [`vike_model::feed_status::parse_feed_status`]. A venue with no producer is absent from the map and is reported
//! as having none. Extending coverage is purely a matter of the binary adding that venue's status
//! handle to the map it passes here. Key editing IS in scope: each configurable tier row gets a
//! small edit affordance that opens a masked (password) form and writes through
//! [`vike_secrets::save_credentials_to_store_journalled`]. That call, and the account-row acts'
//! `edit_account_in_journalled`, are the ONLY store writes this panel performs and both live in
//! `view/editor.rs` on purpose: the workspace's credential-writer gate pins that one file as one row.
//!
//! SECURITY (non-negotiable, see the crate's callers/tests too):
//! - An existing secret's plaintext is NEVER read back into the UI — edit fields always start
//!   empty; the status dot is the only feedback on "is something configured", never the value.
//! - Edit fields are `egui::TextEdit::password(true)` (masked).
//! - Only non-empty fields are written; an empty field means "leave unchanged".
//! - No secret value is ever passed to `tracing`/`println!`/`eprintln!`/`dbg!` — only the
//!   venue/env tier ("saved credentials for binance/live").
//!
//! # The ACCOUNT dimension
//!
//! A venue may hold more than one account (`vike_model::accounts::account_keys`), and this grid is the
//! credential EDITOR — so until it could reach a labelled account, an operator could SEE one on the
//! Data Manager's Venues tab and create or edit one nowhere at all.
//!
//! It arrives as a SELECTOR above the grid rather than as a third axis inside it; `account_strip`
//! (in `view/strip.rs`) carries the whole argument — why not more rows, why not more columns, how an
//! account is created, and what removal is (a ROW act, behind a typed confirm, refused while that
//! row still owns credentials — `account_rows_block`, in `view/editor.rs`). The two properties to
//! carry away from here:
//!
//! * **Every key name is composed through `vike_model::accounts::account_keys::account_key`**
//!   (`keys::account_fields` / `keys::account_expected_key_name`), which returns the DEFAULT account's names
//!   unchanged. A box with no labelled account therefore reads, renders and writes exactly what it
//!   did — `crates/vike-connections/tests/panel/account_editor.rs`'s
//!   `a_store_with_no_labelled_account_renders_the_panel_that_shipped` pins the panel and
//!   `crates/vike-connections/tests/panel/write_journal.rs` still gates the write end to end,
//!   unchanged.
//! * **No per-venue table grew a column.** The grammar appends the label after the WHOLE of today's
//!   key, so every bespoke suffix in the [`crate::keys`] tables is carried along without being
//!   enumerated.

use std::collections::HashMap;
use vike_ui_theme::value::connections;

use vike_model::accounts::account_keys::AccountLabel;
use vike_model::feed_status::ConnectionState;

use crate::env_write::CredentialWrite;
use crate::status::AccountGrids;
use crate::summary::StoreHealth;

mod detail;
mod editor;
mod marks;
mod panel;
mod rail;
mod state;
mod strip;

pub use detail::key_family;
use panel::panel_body;
use state::EditState;

// ⚠ TRANSITIONAL, and STILL HERE — these four are local names for `vike_ui_theme::status`'s colours
// (`OK`, `ERROR`, `MUTED`, `WARNING`), and a local name adds nothing a reader needs: the shared names
// became colour ROLES in design-system step 7. The migration plan
// (docs/superpowers/plans/2026-09-29-gui-design-system-pr7-connections.md) said each would be deleted
// by the task that moved its last use and that none would survive that PR; none was, and the
// children of this module still read all four. Deleting them is a behaviour-neutral change of its
// own (spell each use with the role name), and it was deliberately NOT folded into the layout split.
const CONFIGURED_COLOR: egui::Color32 = vike_ui_theme::status::OK;
const ERROR_COLOR: egui::Color32 = vike_ui_theme::status::ERROR;
const ABSENT_COLOR: egui::Color32 = vike_ui_theme::status::MUTED;
const CONNECTING_COLOR: egui::Color32 = vike_ui_theme::status::WARNING;

// ⚠ TOMBSTONE — `SELECTED_COLOR` lived here, a `const` over the Graphite accent, and painted the
// selected venue's name and the selected account's chip IN the accent. What you are LOOKING AT —
// the venue whose tiers the detail pane shows, the account being viewed — is a selection, and a
// selection is marked in the ACCENT (design system spec §2) as a SHAPE: the accent is never the
// colour of a word (spec §4.3, `ui-theme.toml`'s `accent-is-a-shape`), so the word keeps the text
// ink and `accent_underline` draws the accent beneath it. A `const` cannot read the theme the
// trader picked, so `accent_underline` reads `Tokens::of(ui.ctx()).theme.accent`. Until
// 2026-09-28 the status green was the accent by alias, so these sites could spell it
// `CONFIGURED_COLOR` and look right; `tests/panel/selection_colour.rs` keeps them apart.

// ⚠ TOMBSTONE — `connection_state_label_color` lived here. It painted the old grid's Status
// COLUMN, one cell per venue row, from `live.get(venue).copied().unwrap_or_default()`. Both halves
// of that went with the column: the colour choice is now `venue_detail`'s read of its `FeedFact`'s
// row of the `connection` map (one cell per SELECTED venue, not one per row), and the
// `unwrap_or_default()` — which rendered "nothing in this build produces a status for this venue"
// as the same `Unknown` a silent producer gives — is replaced by `crate::summary::FeedFact`, which
// keeps the two answers apart.
// `vike_ui_theme::status`'s doc names `venue_detail` as the Connections tool's consumer.

/// A PROSE paragraph, set in [`connections::NOTE_W`] rather than across whatever width the window happens to
/// offer. See that constant for why a sentence may not be allowed to decide the window's width.
/// A warning note arrives as `icons::WARNING.before(…)`, which is why this takes any widget text.
fn note(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    let w = ui.available_width().min(connections::NOTE_W);
    ui.allocate_ui_with_layout(
        egui::vec2(w, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.add(egui::Label::new(text));
        },
    );
}

/// **The sentence the form leads with, and the one thing it has to make unmistakable.**
///
/// ⚠ It is `pub` so a headless test can assert the RENDERED text against the same bytes rather
/// than a paraphrase of them — `crates/vike-connections/tests/panel/a11y_form.rs`.
///
/// The old wording ("Fields are masked and start empty. Leave a field blank to keep its current
/// value unchanged; the existing secret is never shown here.") was true and was still read as a
/// bug: the owner's report of this panel was *"editing Dukascopy credentials doesn't show them"*,
/// which is half a missing-field complaint and half this. A form that discards what you cannot
/// see is indistinguishable from a broken one unless it says, at the point of use, that the
/// blankness is the DESIGN and what blank then does.
///
/// So this leads with the refusal and names the consequence in the operator's own terms. It does
/// not weaken the masking, and it must not: rule 1 of this module's security contract is that a
/// stored plaintext is never read back into the UI, gated end to end by
/// `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor`.
pub const MASKED_FIELD_HINT: &str = "Nothing is shown here on purpose: a value \
     already in the store is never read back into the app, so every field is masked and starts \
     EMPTY even for a key that is already set. Type a new value to REPLACE that key; leave a \
     field blank to KEEP whatever is there. Each field's grey text is the exact key it writes.";

/// The egui temp slot this panel's whole selection + form state lives in, keyed off the `Ui` it is
/// rendered on. ONE derivation, so [`shown_account`] and [`connections_ui`] cannot key differently.
fn state_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("vike_connections_edit_state")
}

/// **Which account [`connections_ui`] will render, asked BEFORE it renders.**
///
/// The tool's segmented control and its status line carry a COUNT of this panel's dots, and they
/// are drawn above the panel — so the count has to be derivable from outside without waiting a
/// frame for the panel to report it. A stale-by-one-frame count is exactly the "plausible number"
/// this tool is not allowed to render.
///
/// ⚠ **Call it with the SAME `Ui` you then hand to [`connections_ui`].** Both read [`state_id`],
/// which is derived from `ui.id()`; adding widgets between the two calls does not change that id,
/// but putting the panel inside a container would, and the two would then answer about different
/// slots. This function READS the slot and never writes it, and it applies `preselect` the way the
/// panel will (through the same equality check `EditState::select_account` makes), so the answer
/// is the account the very next call renders.
#[must_use]
pub fn shown_account(ui: &egui::Ui, preselect: Option<&AccountLabel>) -> AccountLabel {
    if let Some(p) = preselect {
        return p.clone();
    }
    let state: EditState = ui.data(|d| d.get_temp(state_id(ui))).unwrap_or_default();
    state.account
}

/// Render the Credentials tab: the account strip, the venue RAIL, the selected venue's DETAIL
/// pane (badges, meta row, one row per tier spelling its `.env` key names out) and the masked
/// inline edit form that writes them.
///
/// ⚠ **Two shapes, one accessibility tree.** Above [`connections::RAIL_DETAIL_MIN_W`] the rail is a fixed
/// column beside the detail pane; below it the rail becomes a wrapped chip row above the detail
/// pane. Both render the same dots with the same hovers and the same
/// selected-venue-is-a-`Label` rule, so nothing a test reads off the tree depends on the width.
///
/// `live` maps a venue name (matching [`vike_model::VENUES`]'s spelling, e.g. `"binance"`) to
/// its current [`ConnectionState`]; a venue absent from the map is reported as having NO PRODUCER
/// rather than as `Unknown` ([`crate::summary::FeedFact`]). The binary
/// supplies a real state for every venue with a live feed producer today
/// (binance/bybit/okx/aster/hyperliquid/polymarket); venues without one (deribit, the FX/broker
/// venues) stay absent, and the detail pane says there is no producer rather than inventing one.
///
/// `creds` is where a **Save** click lands and where it is RECORDED — the credential store path and
/// the change journal, both resolved by the binary's one boot walk. See [`CredentialWrite`]; the
/// store path used to be found here by a `$VIKE_SETTINGS_DIR`-blind walk, which is also why this
/// widget's Save arm was untestable (it wrote the developer's real credential store).
///
/// `grids` carries ONE credential grid per account the store holds — [`AccountGrids`], whose
/// `labelled` half is EMPTY on a single-account box, in which case this renders the default
/// account's `Vec` and nothing else changes. The account being shown is picked in the strip at the
/// top (`account_strip`, which carries the whole design argument for a selector rather than more
/// rows or more columns) and lives in the widget's own temp memory, like the edit form's buffers.
///
/// `preselect` is a ONE-SHOT override of that selection, and it exists for exactly one caller: the
/// `connections-account` QA capture arm (`vike_app_core::ui::startup`), which needs this panel to open
/// on a LABELLED account so that the chip strip and the removal-instruction line are on a contact
/// sheet at all. `None` — every other frame, and every frame of every other launch — leaves the
/// selection entirely to the temp state, so the panel behaves exactly as it did before the
/// parameter existed.
///
/// ⚠ **It is applied through [`EditState::select_account`], not by assignment**, so a preselect
/// arriving while a credential form is open closes that form: the buffers were typed against the
/// account that was selected when it opened, and composing them into another account's key names
/// is the one way this panel could write a live key somewhere nobody is looking. Being one-shot is
/// the CALLER's job (`ToolView::connections_account` is `take`n) — this function honours whatever
/// it is handed, every frame it is handed one.
pub fn connections_ui(
    ui: &mut egui::Ui,
    grids: &AccountGrids,
    live: &HashMap<String, ConnectionState>,
    health: &StoreHealth,
    creds: CredentialWrite<'_>,
    preselect: Option<AccountLabel>,
) {
    let state_id = state_id(ui);
    let mut state: EditState = ui.data_mut(|d| d.get_temp(state_id)).unwrap_or_default();

    if let Some(account) = preselect {
        state.select_account(account);
    }

    // ⚠⚠ **THE PANEL TAKES THE WHOLE WIDTH IT IS GIVEN, AND IS ALLOCATED AT ZERO HEIGHT.** Those
    // are two different claims and the container exists for both.
    //
    // WIDTH — `ui.available_width()` outright, with NO ceiling. This read `.min(PANEL_MAX_W)` and
    // that was the wrong half of the design: the rail is the bounded column, the detail pane is
    // `1fr`. Capping here capped BOTH, which is what put an ~866pt column in the top-left corner
    // of a maximized 2560pt window. The tombstone above [`connections::NOTE_W`] carries the measurement and the
    // fixed-point argument for why filling cannot run the window away.
    //
    // HEIGHT — `0.0`, which is a DESIRED size and not a bound: egui clips nothing by `max_rect`,
    // and `scope_dyn` advances the parent by the child's own `min_rect`. So the panel is laid out
    // at the height of what it draws, and — belt and braces — every `available_height()` inside it
    // reads ~0 rather than the window's, so a future greedy read cannot silently re-open the
    // vertical feedback loop this panel has already been bitten by once.
    //
    // …and it is a CONTAINER rather than a `set_max_*` on the caller's own `Ui`, for two reasons
    // that outlive the width change:
    //
    // * [`state_id`] is read from the CALLER's `ui`, above — so `shown_account`, which the binary
    //   asks on that same `Ui` one step earlier, still keys the same slot. The container moves the
    //   ids of the WIDGETS inside it and nothing else.
    // * The now-deleted `connections_body` used to draw the foot strip on the caller's `Ui` AFTER
    //   this call returned, and mutating that `Ui`'s max rect would have followed the strip with
    //   it — the strip spanned the WINDOW, process-level state and not part of this panel. The
    //   standalone window is gone and `strip_row` is drawn BEFORE this call now
    //   (`crates/vike-app-core/src/ui/tool_views/data.rs`'s `data_body`, on `DataDest::Credentials`),
    //   so nothing draws on the caller's `Ui` after this call returns today — but a container
    //   stays the right shape regardless: it is still this panel's OWN sizing decision rather than
    //   a mutation of state a caller owns, which is the part of this argument that never depended
    //   on who else used that `Ui`.
    let measure = ui.available_width();
    let pick = ui
        .allocate_ui_with_layout(
            egui::vec2(measure, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| panel_body(ui, grids, live, health, creds, &mut state),
        )
        .inner;

    if let Some(venue) = pick {
        state.select_venue(&venue);
    }

    ui.data_mut(|d| d.insert_temp(state_id, state));
}
