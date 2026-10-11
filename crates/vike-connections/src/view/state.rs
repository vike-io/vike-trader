//! The panel's per-widget edit state (`EditState`) and a Save's verdict (`Verdict`).

use vike_model::accounts::account_keys::AccountLabel;

use crate::keys::edit_fields;

/// Per-widget edit state, kept in egui's temp memory (keyed off the widget's own `Id`) rather
/// than threaded through the call site — see the module doc: keeping the edit state inside the
/// connections widget leaves the caller nothing to thread. Edit-field buffers ALWAYS
/// start empty (never pre-filled from an existing secret — rule 2).
#[derive(Clone, Default)]
pub(super) struct EditState {
    /// WHICH ACCOUNT the grid is showing and the form would write.
    ///
    /// On a box with no labelled account, [`AccountLabel::Default`] is the only account the strip
    /// offers a CHIP for — the Add-account form is the way out of it, and what that selects is a
    /// label holding nothing until a credential is saved into it. So while this is `Default`, and
    /// it is until an operator deliberately leaves, every key name composed below is the key name
    /// that shipped: `vike_model::accounts::account_keys::account_key` returns each one unchanged.
    pub(super) account: AccountLabel,
    /// WHICH VENUE the rail has selected and the detail pane is showing.
    ///
    /// Empty until the first frame resolves it — [`connections_ui`] pins it to the grid's FIRST
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
    /// (venue, env_label) currently open in the form, if any.
    pub(super) target: Option<(String, String)>,
    /// One buffer per field of `edit_fields(venue, env_label)`, same order.
    pub(super) buffers: Vec<String>,
    /// Has the PUBLIC half of [`Self::buffers`] been seeded from the store yet?
    ///
    /// ⚠ A latch and not a per-frame refresh, and that is the difference between an editable field
    /// and an unusable one: `render_edit_form` runs every frame, so re-seeding would overwrite each
    /// keystroke with the stored value and the field could never be changed. [`Self::open`] clears
    /// it, so every form gets exactly one seed. It says nothing about SECRETS — those are never
    /// seeded from anything, and the map this function is handed does not contain one.
    pub(super) prefilled: bool,
    /// The last Save's verdict, and the tier row it belongs BESIDE. Non-secret — venue/env tier
    /// only, never a value. See [`Verdict`] for why it carries the cell rather than being a
    /// bare string.
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
    /// refusal, whose arms name ids, venues, tiers and credential key NAMES from statements with
    /// no `value` column in them.
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
    /// **The `account` rows, held across frames — because this panel is IMMEDIATE-MODE and the
    /// store is a FILE.**
    ///
    /// ⚠ [`account_rows_block`] used to call `vike_secrets::resolve_accounts_in` unconditionally in
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

/// **A Save's verdict, and the cell it belongs to.**
///
/// ⚠ **The cell is the whole point of this type.** The verdict used to be a bare
/// `(is_error, String)` rendered by [`connections_ui`] AFTER the rail/detail block — and the
/// two-column arm allocates the rail `ui.available_height()` outright, so the one channel that
/// says `save failed: …` was laid out below the window's own floor and clipped. A failed Save was
/// therefore indistinguishable from a dead button, which is exactly what was reported. Carrying
/// the (venue, tier) lets [`venue_detail`] draw the verdict UNDER THE ROW THAT WAS CLICKED,
/// beside the ✏ and where the form just was, which is the only place it can be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Verdict {
    /// `true` ⇒ the save did not happen; rendered in [`ERROR_COLOR`].
    pub(super) is_error: bool,
    /// The sentence. Venue and tier only — a value never reaches it.
    pub(super) text: String,
    /// The venue whose row this verdict belongs under.
    pub(super) venue: String,
    /// The tier (`SIM`/`DEMO`/`LIVE`) whose row this verdict belongs under.
    pub(super) tier: String,
}

impl EditState {
    pub(super) fn open(&mut self, venue: &str, env_label: &str) {
        let n = edit_fields(venue, env_label).len();
        self.buffers = vec![String::new(); n];
        self.target = Some((venue.to_string(), env_label.to_string()));
        self.prefilled = false;
        self.message = None;
    }

    pub(super) fn close(&mut self) {
        self.target = None;
        self.buffers.clear();
        self.prefilled = false;
        // ⚠ …and the REMOVE confirm, for the same reason the buffers above are cleared: it was
        // typed against the selection that is going away, and a confirm that outlived its
        // selection would be an armed DELETE against a row the operator is no longer looking at.
        self.remove_confirm = None;
        // ⚠ …and the cached account rows: a different selection may sit under a different settings
        // directory, so a list resolved for the old one must never be rendered against the new.
        self.account_rows = None;
    }

    /// Switch the grid to another account.
    ///
    /// ⚠ **It CLOSES any open credential form, and that is a correctness requirement rather than
    /// tidiness.** The form's buffers are typed against the account that was selected when it
    /// opened; leaving it up across a switch would let a Save compose those characters into the
    /// NEW account's key names — a live key written into the wrong account, with the form giving
    /// no sign that anything moved. `crates/vike-connections/tests/panel/account_editor.rs`'s
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
    /// and [`account_fields`] composes them into whatever venue is selected at SAVE time — so a
    /// switch with a form left open is a live key written under another venue's key names.
    /// `crates/vike-connections/src/view/state_tests.rs`'s
    /// `switching_venue_closes_an_open_form` is the gate, beside
    /// `crates/vike-connections/tests/panel/account_editor.rs`'s
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
    /// rename fixes: it parses a string and changes a SELECTION — no row appears, no key is
    /// written, and the strip renders a `(new)` chip for a label no store enumeration knows about.
    /// The account the GRID means comes into existence when its first `__LABEL` credential is
    /// saved; the account the STORE means is a ROW, and creating one of those is
    /// `vike-cli secrets account add` (`vike_secrets::AccountEdit::Create`). A method named
    /// `create_account` in front of neither is a name a reader has to disprove.
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
