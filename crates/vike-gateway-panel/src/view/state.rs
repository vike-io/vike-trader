//! The panel's per-widget edit state (`EditState`), the form it has open (`FormTarget`) and a
//! Save's verdict (`Verdict`).

use vike_model::accounts::account_keys::AccountLabel;

use crate::keys::{FormPlace, form_fields};

/// **The form that is open**: the venue, which form ([`FormPlace`]), and — for a tier's form — the
/// account row it writes, fixed at the moment the form opened.
///
/// ⚠ The account id is captured at OPEN and never re-resolved at Save: the buffers were typed
/// against that row, and a Save that looked the cell up again could land on a different row that
/// answered `(venue, label, tier)` by then. A row removed meanwhile is the store's refusal
/// (`vike_secrets::DbErrorKind::NoSuchAccount`), shown verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FormTarget {
    /// The venue whose form this is.
    pub(super) venue: String,
    /// The tier cell's form, or the venue's application group.
    pub(super) place: FormPlace,
    /// The account row a tier's form writes; `None` for the venue's application group.
    pub(super) account: Option<i64>,
}

/// How long an armed field Remove waits for its second click before it disarms.
pub(super) const RETIRE_ARM: std::time::Duration = std::time::Duration::from_secs(5);

/// Per-widget edit state, kept in egui's temp memory (keyed off the widget's own `Id`) rather
/// than threaded through the call site — see the module doc: keeping the edit state inside the
/// gateway widget leaves the caller nothing to thread. Edit-field buffers ALWAYS
/// start empty (never pre-filled from an existing secret — rule 2).
#[derive(Clone, Default)]
pub(super) struct EditState {
    /// WHICH ACCOUNT LABEL the grid is showing — each tier cell is then the account row of
    /// `(venue, this label, tier)`.
    ///
    /// On a box whose rows carry no label, [`AccountLabel::Default`] is the only account the strip
    /// offers a CHIP for — the Add-account form is the way out of it, and what that selects is a
    /// label no row carries until the operator creates one with its tier.
    pub(super) account: AccountLabel,
    /// WHICH VENUE the rail has selected and the detail pane is showing.
    ///
    /// Empty until the first frame resolves it — [`gateway_ui`] pins it to the grid's FIRST
    /// row whenever this names a venue the grid being rendered does not contain, which covers
    /// both the empty default and a roster that shrank under a persisted selection. Storing the
    /// venue by NAME rather than by index is what makes that repair possible: an index would
    /// silently point at a different venue after any reorder.
    pub(super) venue: String,
    /// The **Add account** field's buffer while that form is open; `None` when it is closed. A
    /// plain name, not a credential: rendered unmasked, deliberately, because an operator has to
    /// be able to read the label they are about to have to type into their policy file.
    pub(super) add_label: Option<String>,
    /// The previous **Create** click's refusal, rendered under the field —
    /// `vike_model::accounts::account_keys::AccountKeyError`'s own `Display`, so the message names the rule
    /// that was broken rather than restating one here that could drift from the validator.
    pub(super) label_error: Option<String>,
    /// The form currently open, if any.
    pub(super) target: Option<FormTarget>,
    /// One buffer per field of `form_fields(venue, place)`, same order.
    pub(super) buffers: Vec<String>,
    /// Has the PUBLIC half of [`Self::buffers`] been seeded from the store yet?
    ///
    /// ⚠ A latch and not a per-frame refresh, and that is the difference between an editable field
    /// and an unusable one: `render_edit_form` runs every frame, so re-seeding would overwrite each
    /// keystroke with the stored value and the field could never be changed. [`Self::open`] clears
    /// it, so every form gets exactly one seed. It says nothing about SECRETS — those are never
    /// seeded from anything, and the map this function is handed does not contain one.
    pub(super) prefilled: bool,
    /// The last Save's verdict, and the row it belongs BESIDE. Non-secret — venue and tier only,
    /// never a value. See [`Verdict`] for why it carries the row rather than being a bare string.
    pub(super) message: Option<Verdict>,
    /// The last ACCOUNT-ROW act's verdict, rendered in the strip beside the rows it is about.
    ///
    /// ⚠ A separate cell from [`EditState::message`] rather than a reuse of it, and the reason is
    /// [`Verdict`]'s own: that one carries the `(venue, tier)` CELL it belongs under, because a
    /// Save verdict rendered anywhere but beneath the row that was clicked was measured to be laid
    /// out below the window's floor and clipped. An account-ROW act belongs to no tier cell — its
    /// rows span venues — so it is rendered where it happens, in the strip.
    ///
    /// Non-secret by construction: it is either a verb plus an id, or `vike_secrets`' own account
    /// refusal, whose arms name ids, venues, tiers and credential NAMES from statements with no
    /// `value` column in them.
    pub(super) row_message: Option<String>,
    /// ⚠ **THE TYPED CONFIRM for a REMOVE, armed but never pre-filled** — `(row id, what the
    /// operator has typed)`.
    ///
    /// A DELETE is the one act on this panel the store cannot put back, and both of its siblings
    /// require the operator to type the row id for it (`vike-cli secrets account remove --confirm
    /// N`, and the node wire's `AccountRequest::confirm`). This is that ceremony in a GUI, in the
    /// shape `crates/vike-app-core/src/ui/tool_views/backend_settings.rs`'s `can_save` used for a
    /// `policy.toml` write until `docs/decisions/0086` point 7 deleted THAT one (a settings key is
    /// not an account row): the Remove button ARMS this cell and removes nothing, and the box
    /// starts EMPTY — pre-filling it from the id the panel already holds would reduce the ceremony
    /// to a click, which is precisely what the contract exists to prevent.
    ///
    /// Cleared by [`EditState::close`], so switching account or venue disarms it: the id typed
    /// against one selection must not stay armed against another.
    pub(super) remove_confirm: Option<(i64, String)>,
    /// **The two-click REMOVE of one credential field, armed**: `(the field it removes — its grey
    /// text, the same word the control shows — and the egui time of the arming click)`. The first
    /// click on a held field's Remove sets it and turns that control into "Confirm remove FIELD";
    /// the second click on THAT control sends. It disarms after [`RETIRE_ARM`] and on any other
    /// click, and [`EditState::close`] clears it, so a confirm armed against one selection can
    /// never fire against another. Nothing typed: the confirm is the second deliberate act
    /// (`docs/decisions/0144`'s ADDED note).
    pub(super) retire_armed: Option<(String, f64)>,
    /// The fields this open form has removed (their grey text), so a removed field reads "not set"
    /// in its placeholder until the form closes. Words only. Cleared by [`EditState::open`] and
    /// [`EditState::close`].
    pub(super) retired: Vec<String>,
    /// **The `account` rows, held across frames — because this panel is IMMEDIATE-MODE and the
    /// store is a FILE.**
    ///
    /// ⚠ [`account_rows_block`] used to read `vike_secrets::CredentialStore::accounts` unconditionally in
    /// its render body, which runs once per frame while the panel is visible: a `backend_in` stat,
    /// a fresh `sqlite3_open`, a prepare and a full-table scan, **60–120 times a second on the GUI
    /// thread**. Every other datum on that panel arrives already resolved; this was the only store
    /// read in the render path. It also had a cost beyond this process — the store runs
    /// `journal_mode = DELETE` (no WAL; it verifies the engine's answer and refuses otherwise), so
    /// a reader holding SHARED is exactly what a concurrent `vike-cli secrets set` has to wait out
    /// through `BUSY_TIMEOUT`.
    ///
    /// `None` means *ask the store on the next frame*. It is invalidated by the acts that change
    /// the answer and by nothing else: a row write here, and [`EditState::close`] (a different
    /// selection may sit under a different settings directory). Deliberately NOT a timer — a
    /// panel that refreshes on a clock would disagree with the store for up to one tick for no
    /// reason anybody could name, and the set of writers is small and known.
    ///
    /// ⚠ It holds the **`Result`**, with the error stringified so it can live across frames — never
    /// a flattened `Accounts`. *A store that exists and will not open* and *a store with nothing to
    /// say* are different answers, and the render body renders them differently.
    pub(super) account_rows: Option<Result<vike_secrets::Accounts, String>>,
}

/// **A Save's verdict, and the row it belongs to.**
///
/// ⚠ **The row is the whole point of this type.** The verdict used to be a bare
/// `(is_error, String)` rendered by [`gateway_ui`] AFTER the rail/detail block — and the
/// two-column arm allocates the rail `ui.available_height()` outright, so the one channel that
/// says `save failed: …` was laid out below the window's own floor and clipped. A failed Save was
/// therefore indistinguishable from a dead button, which is exactly what was reported. Carrying
/// the (venue, form) lets [`venue_detail`] draw the verdict UNDER THE ROW THAT WAS CLICKED,
/// beside the ✏ and where the form just was, which is the only place it can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Verdict {
    /// `true` ⇒ the save did not happen; rendered in [`ERROR_COLOR`].
    pub(super) is_error: bool,
    /// The sentence. Venue and tier only — a value never reaches it.
    pub(super) text: String,
    /// The venue whose row this verdict belongs under.
    pub(super) venue: String,
    /// The tier row (or the venue's application row) this verdict belongs under.
    pub(super) place: FormPlace,
}

impl EditState {
    /// Open `place`'s form of `venue`, writing account row `account` (a tier's form) or the venue's
    /// application rows (`None`). The buffers are sized from [`form_fields`] itself, so they cannot
    /// lag the form `render_edit_form` indexes them by.
    pub(super) fn open(&mut self, venue: &str, place: FormPlace, account: Option<i64>) {
        let n = form_fields(venue, place).len();
        self.buffers = vec![String::new(); n];
        self.target = Some(FormTarget { venue: venue.to_string(), place, account });
        self.prefilled = false;
        self.message = None;
        self.retire_armed = None;
        self.retired.clear();
    }

    /// Expire an armed Remove whose [`RETIRE_ARM`] ran out, at egui time `now`. Returns the
    /// seconds still left when it is STILL armed (so the caller can ask for a repaint at the
    /// deadline), `None` when nothing is armed or it just expired.
    pub(super) fn tick_retire_arm(&mut self, now: f64) -> Option<f64> {
        let (_, armed_at) = self.retire_armed.as_ref()?;
        let left = RETIRE_ARM.as_secs_f64() - (now - armed_at);
        if left <= 0.0 {
            self.retire_armed = None;
            None
        } else {
            Some(left)
        }
    }

    pub(super) fn close(&mut self) {
        self.target = None;
        self.buffers.clear();
        self.prefilled = false;
        // ⚠ …and the REMOVE confirm, for the same reason the buffers above are cleared: it was
        // typed against the selection that is going away, and a confirm that outlived its
        // selection would be an armed DELETE against a row the operator is no longer looking at.
        self.remove_confirm = None;
        self.retire_armed = None;
        self.retired.clear();
        // ⚠ …and the cached account rows: a different selection may sit under a different settings
        // directory, so a list resolved for the old one must never be rendered against the new.
        self.account_rows = None;
    }

    /// Switch the grid to another account.
    ///
    /// ⚠ **It CLOSES any open credential form, and that is a correctness requirement rather than
    /// tidiness.** The form's buffers are typed against the account that was selected when it
    /// opened; leaving it up across a switch would put a form for one account under another
    /// account's chip — a live key the operator believes is going to the account on screen, with
    /// the form giving no sign that anything moved.
    /// `crates/vike-gateway-panel/tests/panel/account_editor.rs`'s
    /// `switching_account_closes_an_open_credential_form` is the gate.
    pub(super) fn select_account(&mut self, account: AccountLabel) {
        if self.account == account {
            return;
        }
        self.account = account;
        self.close();
        self.message = None;
    }

    /// Switch the rail to another venue.
    ///
    /// ⚠ **It CLOSES any open credential form, for the same reason [`Self::select_account`]
    /// does.** The buffers were typed against the venue that was selected when the form opened,
    /// and a form left open under another venue's pane is a live key the operator believes is
    /// going to the venue on screen.
    /// `crates/vike-gateway-panel/src/view/state_tests.rs`'s
    /// `switching_venue_closes_an_open_form` is the gate, beside
    /// `crates/vike-gateway-panel/tests/panel/account_editor.rs`'s
    /// `switching_account_closes_an_open_credential_form` — the same rule on the other axis.
    pub(super) fn select_venue(&mut self, venue: &str) {
        if self.venue == venue {
            return;
        }
        self.venue = venue.to_string();
        self.close();
        self.message = None;
    }

    /// Accept an operator-typed account label and SELECT it, or refuse it with the validator's own
    /// reason.
    ///
    /// ⚠⚠ **IT WAS CALLED `create_account` AND IT CREATES NOTHING.** That name was the defect this
    /// rename fixes: it parses a string and changes a SELECTION — no row appears, nothing is
    /// written, and the strip renders a `(new)` chip for a label no account row carries. An account
    /// is a ROW with a venue and a tier, and creating one is `vike-cli secrets account add`
    /// (`vike_secrets::AccountEdit::Create`); a credential Save never creates one (decision 0156
    /// item 6). A method named `create_account` in front of neither is a name a reader has to
    /// disprove.
    ///
    /// ⚠ The ONLY repair applied is a `trim`, and the line between that and the repairs
    /// `AccountLabel::parse` deliberately refuses (it will not uppercase `alt`) is that whitespace
    /// is not part of ANY label's spelling — nobody is learning a wrong name from a stripped
    /// trailing space off a paste, whereas a silently uppercased label is a spelling that then
    /// does not match what they wrote in their policy file.
    pub(super) fn select_typed_account(&mut self, text: &str) {
        match AccountLabel::parse(text.trim()) {
            Ok(label) => {
                self.select_account(label);
                self.add_label = None;
                self.label_error = None;
            }
            Err(e) => self.label_error = Some(e.to_string()),
        }
    }
}

#[path = "state_tests.rs"]
#[cfg(test)]
mod state_tests;
