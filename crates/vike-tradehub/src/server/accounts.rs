//! `server::accounts` — the node's ACCOUNT-ADMINISTRATION plane (`docs/decisions/0065`): the
//! capability handle ([`AccountAdminSource`]), the operator's barrier declaration
//! ([`AccountBarrier`]) and the admission decision (`account_admission`) that keeps the key a
//! desktop carries to place orders from being the key that writes key material. A write is
//! journalled under the connection's [`ChangeActor`], built once per connection by the session.
//!
//! Split out of `server.rs` as a pure move; the module doc there carries the barrier's reachability
//! argument, and each type below carries its own.

use vike_change_journal::ChangeActor;
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::refusal::HubRefusal;
use vike_tradehub_client::proto::Response;
use vike_tradehub_client::wire::{
    AccountListWire, AccountRequestWire, AccountRowWire, AccountVerbWire, AccountWriteWire,
    CredentialsHeldWire,
};

use crate::refusals::AccountVerbRefusal;

/// The credential store's own refusal, as this verb's answer (its words are safe to print: `apply`'s doc).
fn store_refused(error: impl std::fmt::Display) -> HubRefusal {
    AccountVerbRefusal::StoreRefused { why: error.to_string() }.into()
}

/// **WHERE the node's account verbs write, and the fact that this handle EXISTS at all is the
/// barrier's first part.**
///
/// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §3c Part 1: the
/// capability is an `Option` the BINARY constructs and hands to [`super::serve`], exactly as
/// `commands: Option<CommandSink>` and `settings: Option<SettingsShowSource>` already are.
///
/// > *"When it is `None` the process contains no path from a frame to the store, the feature string
/// > is absent from `served_features`, and there is nothing to forget to check."*
///
/// That is why this is a handle rather than a boolean beside a check: a refusal somebody has to
/// remember is a refusal somebody can forget, and the shipped default — every box that has not
/// declared the barrier — must be byte-identical to a build without the verb.
///
/// # What the BINARY decides before it builds one
///
/// `crate::hub_server::account_admin_source` is the one site, and it answers three questions in
/// order: has the operator DECLARED a barrier (`config.tradehub_account_admin`, three-valued —
/// unset/`off`, `loopback`, `contained`); if `loopback`, does [`super::bind_exposure`] agree that every
/// resolved address is loopback (a wide bind REFUSES the capability at boot, naming the flag and
/// the address); and is there a settings database this daemon can write. All three must answer yes.
/// WHO may use the surface is not a boot question: a device whose role is `admin` on the access list
/// (decision 0288; no shared admin key exists).
///
/// ⚠ **This server cannot ask the first two itself and must not try.** §3b measures why: `serve`
/// receives an ALREADY-BOUND `TcpListener` and never calls `local_addr`, [`super::bind_exposure`] runs
/// once in the binary before the listener exists, and the per-frame PEER answers wrongly in both
/// directions (through an SSH tunnel it is loopback and correct; inside a container correctly
/// published to `127.0.0.1` it is the bridge gateway, so a CONTAINED deployment would be refused).
/// The exposure is
/// knowable at BOOT and unknowable at the point of decision — unless it is handed in, which is what
/// this type is.
#[derive(Clone, Debug)]
pub struct AccountAdminSource {
    /// `<project>/settings` as the daemon's ONE boot walk resolved it (`vike_boot::Booted`'s
    /// `settings_dir`).
    ///
    /// ⚠ The SETTINGS directory, never a store path: `vike_secrets`'s routers ask
    /// `backend_in` about this directory, which is the same question the daemon's own credential
    /// READ asks — so the verb that lists the rows and the verb that writes one cannot disagree
    /// about which store they are talking about. A store path threaded in separately is exactly how
    /// they could.
    pub settings_dir: std::path::PathBuf,
    /// The `VIKE_NO_LIVE_KEYS` scope this daemon booted under (`vike_boot::Booted::store`'s), so
    /// the listing and the presence read on this surface answer under the same scope as every other
    /// read the process makes. Writes take no scope (a scope only narrows what is READ).
    pub scope: vike_secrets::CredentialScope,
    /// The barrier the operator DECLARED, carried so the server can say which one is in force
    /// without re-reading a flag. It decides nothing here — the binary already refused to build
    /// this handle if the declaration and the bind disagreed — and it is on the type so that a log
    /// line and a reply can be honest about what is holding the frames up.
    pub barrier: AccountBarrier,
}

/// **The operator's DECLARATION about this listener's confidentiality**, which is a three-valued
/// key rather than a boolean, and 0065 §3c Part 3 is the argument.
///
/// The node wire is PLAINTEXT and authenticates the CONNECTION rather than each frame
/// (`crates/vike-tradehub/src/server.rs`'s module doc), so confidentiality comes entirely from REACHABILITY. The process can CHECK
/// exactly one shape of that and cannot check the other:
///
/// | value | what the operator asserts | what the process does |
/// |---|---|---|
/// | unset / `off` | nothing | no capability, no key read, no feature advertised — byte-identical to a build without the verb |
/// | `loopback` | *this listener is on loopback and reached through a tunnel* | **CHECKS it** — armed only when [`super::bind_exposure`] classifies every resolved address as loopback; a wide bind REFUSES the capability at boot |
/// | `contained` | *the barrier is outside this process* — a `127.0.0.1`-published container port, a private interface | armed regardless of the bind; the process does NOT verify it and logs exactly what has been asserted |
///
/// ⚠ **A boolean would collapse the two assertions into one word and make the CHECKABLE case
/// uncheckable**, which is the whole of what this split buys. `loopback` is the only barrier the
/// process can check, so it is the only one that is checked — and the refusal lands precisely where
/// there is evidence for it.
///
/// ⚠ **`contained` is not a warning wearing a value's clothes.**
/// `docs/decisions/0026-containerisation-additive-backend-image.md` refused to let the daemon INFER
/// a container's containment (*"a `/.dockerenv` probe that auto-allowed the bind would be the daemon
/// inferring consent from its own environment"*); it did not refuse to let an operator DECLARE it.
/// The declaration sits in the same class as `flags.tradehub_allow_public_bind` — made once on the
/// box, in a file the daemon cannot rewrite from the wire on any shipped deployment
/// (`ProtectSystem=strict` with `ReadWritePaths` naming `settings/state` alone, so a settings write
/// refuses with `EROFS` inside its own namespace).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountBarrier {
    /// The listener is on loopback and reached through a tunnel. CHECKED against
    /// [`super::bind_exposure`].
    Loopback,
    /// The barrier is outside this process. Asserted, never verified.
    Contained,
}

impl AccountBarrier {
    /// The settings-file spelling — one derivation, so the parser and every message agree.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AccountBarrier::Loopback => "loopback",
            AccountBarrier::Contained => "contained",
        }
    }

    /// Parse the three-valued declaration. `None` for unset, blank, `off`, and for **anything
    /// unrecognised**.
    ///
    /// ⚠ **An unrecognised value is OFF, not an error and not a guess.** A typo'd `loopbak` must
    /// never arm a credential surface, and the safe direction is the one where the capability does
    /// not exist. The binary logs the unrecognised value so an operator who typed one is told
    /// rather than left believing the barrier is up — a refusal that starts nothing is worse here
    /// than a daemon that keeps trading headless, which is the same trade
    /// [`super::BindDecision::Refuse`] makes.
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Option<AccountBarrier> {
        match raw.map(str::trim)? {
            "loopback" => Some(AccountBarrier::Loopback),
            "contained" => Some(AccountBarrier::Contained),
            _ => None,
        }
    }
}

impl AccountAdminSource {
    /// Answer one [`vike_tradehub_client::proto::Request::Account`] — the ONE place a frame becomes
    /// a store write.
    ///
    /// # The CEREMONY, and whose shape it is
    ///
    /// `apply_set_setting`'s policy contract verbatim, because that is the precedent this surface
    /// was told to follow rather than invent one: the destructive verbs
    /// ([`AccountVerbWire::Remove`], [`AccountVerbWire::SetCredential`], [`AccountVerbWire::RetireCredential`])
    /// are REFUSED unless `confirm` equals the exact thing being changed — the row id, or the
    /// credential key name. **Missing and
    /// mismatched get DISTINCT messages, each naming the expected spelling**, which is that arm's
    /// split. The client's job is to make the operator TYPE it, or to make a SECOND deliberate act
    /// (the GUI's two-click, which fills it with the key the armed button names —
    /// `docs/decisions/0144`'s ADDED note); this method's job is to refuse anything else, so no
    /// client can quietly skip it.
    ///
    /// `vike_tradehub_client::wire::AccountVerbWire::required_confirm` is the ONE derivation of what
    /// the confirm must be, consulted by the client that prompts and by this method that refuses —
    /// so the two cannot answer differently about what the ceremony is.
    ///
    /// # What never leaves
    ///
    /// No reply carries a credential value on any path, and the refusals quote nothing the caller
    /// sent. `vike_secrets`' own refusals are safe to print by construction
    /// (`StoreErrorKind::AccountHasCredentials` names key NAMES from a statement with no `value`
    /// column; `StoreErrorKind::AccountLabelMalformed` echoes no token at all), and the one refusal
    /// composed here — an invalid credential KEY — names the key, which is not a secret, and never
    /// the value.
    ///
    /// # Errors
    /// `Err(refusal)` is answered on the wire as the refusal frame ([`Response::Refused`],
    /// [`AccountVerbRefusal`]). Every one of them wrote nothing.
    pub(super) fn apply(
        &self,
        req: &AccountRequestWire,
        actor: &ChangeActor,
    ) -> Result<Response, HubRefusal> {
        // ⚠ THE CEREMONY FIRST, before the store is opened and before anything is validated —
        // `apply_set_setting`'s ordering, so a refused ceremony costs no lock and cannot be
        // distinguished from a refused one by timing what the store did.
        if let Some(expected) = req.verb.required_confirm() {
            match req.confirm.as_deref() {
                None => {
                    let verb = req.verb.word();
                    return Err(AccountVerbRefusal::ConfirmMissing { verb, expected }.into());
                }
                Some(c) if c.trim() != expected => {
                    return Err(AccountVerbRefusal::ConfirmMismatch { expected }.into());
                }
                Some(_) => {}
            }
        }

        match &req.verb {
            AccountVerbWire::List => self.list(),
            AccountVerbWire::Add { venue, tier, label } => {
                // The venue roster and the tier vocabulary are checked HERE rather than in the
                // store because the refusals print the roster, which is what makes them
                // actionable; the tier half (the second check below) is `vike_model::AccountTier`'s strict
                // parser — the wire carries the word as a `String` (decision 0091 §3) and THIS is
                // the edge where it becomes a tier (decision 0129). ⚠ The reason this used to
                // give — "`vike-secrets` declares no `vike-*` dependency and cannot see
                // `vike_model::VENUES`" — is false since decision 0072: that crate's
                // `ensure_venue_rows` iterates that very roster. Where the check lives is now a
                // MESSAGE-QUALITY choice, not a layering one.
                if !vike_model::VENUES.contains(&venue.as_str()) {
                    let (venue, roster) = (venue.clone(), vike_model::VENUES.join(", "));
                    return Err(AccountVerbRefusal::UnknownVenue { venue, roster }.into());
                }
                let Ok(tier) = tier.parse::<vike_model::AccountTier>() else {
                    let (tier, legal) = (tier.clone(), vike_model::AccountTier::legal_words());
                    return Err(AccountVerbRefusal::UnknownTier { tier, legal }.into());
                };
                // ⚠ A new row is born ACTIVE, and since decision 0119 an active account trades at
                // its own tier: this verb ARMS at the next restart, and the reply says so.
                let note = if tier == vike_model::AccountTier::Paper {
                    "this row is ACTIVE at tier `paper`: it arms no real exec.".to_string()
                } else {
                    let running = "A RUNNING tradehub starts it within a second, on any venue, with no \
                                   restart (decision 0169), through the gates a restart would cross \
                                   (the preflight of a venue it had not mounted included).";
                    format!(
                        "⚠ this row is ACTIVE and ARMS {venue} at tier `{tier}` (decision 0119: an \
                         account trades at its own tier while active), once its credential keys \
                         are in the store. {running} Two ACTIVE non-paper tiers for one account do \
                         not run it (`two active tiers`); deactivate the row (SetActive false) to \
                         keep it off."
                    )
                };
                self.write(
                    vike_secrets::AccountEdit::Create { venue, tier, label: label.as_deref() },
                    Some(note),
                    actor,
                )
            }
            // ⚠ **A WARNING, WHERE `docs/decisions/0065` §5 ASKS FOR A REFUSAL — a declared
            // deviation, not an oversight.** That record says of `rename`: *"plus a refusal when
            // the OLD label is named in `policy.toml`'s `[accounts.<venue>]` … with a REMOTE verb
            // the operator who renames and the operator who restarts need not be the same person,
            // so the refusal belongs at the edit."*
            //
            // What is here instead is this note, rendered on every rename that changed something.
            // Decision 0119 deleted the `[accounts.<venue>]` table that record keyed the refusal
            // on: an account is armed by its OWN `account` row, so the label a rename changes IS
            // the arming key, and what can still name the OLD label is a `mount` row's `account`,
            // a position or order the journal holds under the route key, or a remote peer. The
            // store sees only the first, and refusing on it would refuse a plain rename that moves
            // no account anywhere.
            //
            // What catches the ONE-STEP hazard: `crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` refuses
            // the whole node to start, `vike_tradehub_core::arming`'s `Block::AccountNotInStore` reports
            // it, and the mount itself answers `DukascopyRefusal::NoSuchAccount`. Three independent
            // catches, all at the next restart, and the failure is an outage.
            //
            // ⚠ **THOSE THREE ARE NOT WHAT STOPS A MISROUTE.** All three fire on a label that STOPS
            // resolving; none looks at one that resolves to a DIFFERENT row, and two renames reached
            // that state with the mount profile untouched:
            //
            //   0. row A = an unlabelled `demo` dukascopy account holding one JForex login; row B =
            //      a `demo` account labelled `ALT` holding the other, ACTIVE → account `ALT` armed,
            //      its orders reach the legal entity B's login belongs to.
            //   1. rename B to `OLD`: the login travels with the row (decision 0156 item 6).
            //   2. rename A to `ALT`: REFUSED now (`vike_secrets::StoreErrorKind::AccountLabelGivenUp`).
            //      Accepted, the running engine keeps B's session and warns `key_changed`; the next
            //      adapter start signs `ALT` with A's login, the OTHER legal entity, under B's
            //      journalled positions and orders.
            //
            // The writer holds that refusal, so the CLI and the panel cross it too, on every venue.
            AccountVerbWire::Rename { id, label } => self.write(
                vike_secrets::AccountEdit::Rename { id: *id, label: label.as_deref() },
                Some(
                    "⚠ if a mount profile (`account = \"<LABEL>\"`) or a client names this account \
                     by its OLD label, that name stops resolving and a mount addressing it is \
                     REFUSED at the next restart. Update the profile too. The OLD label stays this \
                     account's: no other account of this venue may take it, so that name can never \
                     start reaching another account. A RUNNING account keeps running under its OLD \
                     label (one engine, one lease); the new label takes effect at its next stop and \
                     start."
                        .to_string(),
                ),
                actor,
            ),
            AccountVerbWire::SetActive { id, active } => self.write(
                vike_secrets::AccountEdit::SetActive { id: *id, active: *active },
                Some(if *active {
                    "⚠ this ARMS the account at its own tier (decision 0119: an account trades at \
                     its own tier while active), once its credential keys are in the store. A \
                     RUNNING tradehub starts the account within a second, at its own tier, on any \
                     venue, with no restart (decision 0169), through the gates a restart would \
                     cross. A refusal (a lease held elsewhere, a gatewayhub that does not answer) \
                     leaves that one account NOT RUNNING with its reason; one that is only a matter \
                     of time is retried. Two ACTIVE non-paper tiers for one account do not run it \
                     (`two active tiers`)."
                        .to_string()
                } else {
                    "⚠ a RUNNING tradehub stops the account within a second, with no restart \
                     (decision 0169): its resting orders are cancelled, its session and lease \
                     close, new orders for it are refused, and the node lists it as NOT RUNNING \
                     (`account inactive`). A position it holds is NOT closed and not hidden: close \
                     it with `close-all` BEFORE deactivating. The row and its credential keys \
                     are still there, which is what makes this reversible."
                        .to_string()
                }),
                actor,
            ),
            AccountVerbWire::Remove { id } => {
                self.write(vike_secrets::AccountEdit::Remove { id: *id }, None, actor)
            }
            AccountVerbWire::SetCredential { key, value } => self.set_credential(key, value, actor),
            AccountVerbWire::RetireCredential { key } => self.retire_credential(key, actor),
            AccountVerbWire::HeldCredentials { names } => self.held_credentials(names),
            AccountVerbWire::SetBook { id, venue_account_id, replace } => {
                self.set_book(*id, venue_account_id.as_deref(), *replace, actor)
            }
        }
    }

    /// **WRITE one row's `venue_account_id`** — the wire twin of `vike-cli account set-book`.
    ///
    /// ⚠ **It does NOT go through [`Self::write`] / `AccountEdit`, and that is deliberate rather
    /// than an inconsistency.** `crates/vike-ops/tests/settings_secrets/credential_writer_gate/gates.rs`'s
    /// `GROWTH_GUIDANCE` already classified this column once, when `set_venue_account_id` was
    /// admitted: the rule it attached was *"a SECOND FUNCTION for a second column is the shape to
    /// refuse here: this one grew a parameter instead."* Routing the book through `AccountEdit`
    /// would be exactly that second function, in the other direction — a second way to write a
    /// column that already has one, free to disagree with it about the refusal below.
    ///
    /// So this calls the SAME `vike_secrets::set_venue_account_id_in` the CLI calls, with the same
    /// arguments, and every refusal it raises is raised identically on both surfaces.
    ///
    /// ⚠ **`BookSource::Operator`, never `Handshake`.** A value that arrived over this wire was
    /// typed by a human into a GUI; stamping it as a handshake would make a hand-entered row
    /// indistinguishable from one a venue confirmed, which is the false confidence
    /// `vike_secrets::account_confirmation`'s whole module exists to remove. The CLI's own call site
    /// carries the same note for the same reason.
    fn set_book(
        &self,
        id: i64,
        venue_account_id: Option<&str>,
        replace: bool,
        actor: &ChangeActor,
    ) -> Result<Response, HubRefusal> {
        // The writer records the book write itself (`Change::account_book`, nothing when nothing
        // changed); a failed append comes back for THIS surface to log, and never fails the write.
        let (done, journal_error) = vike_secrets::set_venue_account_id_in(
            &self.settings_dir,
            id,
            venue_account_id,
            replace,
            vike_secrets::BookSource::Operator,
            self.account_journal(actor),
        )
        .map_err(store_refused)?;
        log_journal_error(journal_error.as_ref(), "account book");
        // The key NAMES the row owns, read as `list` reads them (names, never values). The book is already written, so
        // a read that fails is said in the note: never a failed reply, never a row that seems to hold no key.
        let (keys, unread) = match self.store().account_keys() {
            Ok(keyed) => {
                (keyed.and_then(|mut k| k.remove(&id)).map(|k| k.names).unwrap_or_default(), None)
            }
            Err(e) => {
                tracing::error!(error = %e, "vike-tradehub: account {id}'s key names unread after its book write");
                let why = "the row's credential key names could not be read, so this reply lists none; \
                           `list` reads them again";
                (Vec::new(), Some(why.to_string()))
            }
        };

        let row = AccountRowWire {
            id: done.before.id,
            venue: done.before.venue.clone(),
            tier: done.before.tier.as_str().to_string(),
            label: done.before.label.clone(),
            // The column AFTER the write — `before` is what the transaction found, and a reply that
            // echoed it would tell the operator their write had not happened.
            venue_account_id: done.venue_account_id.clone(),
            active: done.before.active,
            last_verified_at: done.before.last_verified_at.clone(),
            keys,
        };
        let unchanged = (!done.changed).then(|| {
            "the row already named exactly this book, so nothing was written. Re-asserting a known \
             book is not a mistake and is not refused."
                .to_string()
        });
        let note = [unchanged, unread].into_iter().flatten().reduce(|a, b| format!("{a} {b}"));
        Ok(Response::AccountWritten(Box::new(AccountWriteWire {
            verb: if venue_account_id.is_some() { "set-book" } else { "clear-book" }.to_string(),
            row: Some(row),
            changed: done.changed,
            note,
        })))
    }

    /// The listing. **NO value is selected anywhere on this path** — `read_accounts` never touches
    /// the `credential` table at all, and `read_account_keys`' statement is
    /// `SELECT name, field, account_id`.
    fn list(&self) -> Result<Response, HubRefusal> {
        let store = vike_secrets::db_path_in(&self.settings_dir);
        let handle = self.store();
        let accounts = handle.accounts().map_err(store_refused)?;
        let rows = match &accounts {
            vike_secrets::AccountRows::Known(rows) => rows,
            vike_secrets::AccountRows::Unanswerable(why) => {
                return Err(AccountVerbRefusal::NoAccountTable { why: why.to_string() }.into());
            }
        };
        let keyed = handle.account_keys().map_err(store_refused)?.unwrap_or_default();
        let rows = rows
            .iter()
            .map(|a| AccountRowWire {
                id: a.id,
                venue: a.venue.clone(),
                tier: a.tier.as_str().to_string(),
                label: a.label.clone(),
                venue_account_id: a.venue_account_id.clone(),
                active: a.active,
                last_verified_at: a.last_verified_at.clone(),
                keys: keyed.get(&a.id).map(|k| k.names.clone()).unwrap_or_default(),
            })
            .collect();
        Ok(Response::AccountList(Box::new(AccountListWire {
            store: store.display().to_string(),
            rows,
        })))
    }

    /// One lifecycle write, through `vike_secrets::edit_account_in` — the Backend-aware router,
    /// never the db function directly, so this exercises the store choice as well as the write.
    ///
    /// ⚠ Every refusal it can return is safe to put on the wire verbatim: `vike-secrets`' account
    /// refusals name ids, venues, tiers and credential key NAMES, and the one that could have
    /// echoed an operator-supplied token (`AccountLabelMalformed`) deliberately does not.
    fn write(
        &self,
        edit: vike_secrets::AccountEdit<'_>,
        note: Option<String>,
        actor: &ChangeActor,
    ) -> Result<Response, HubRefusal> {
        // The durable record is the writer's own (`Change::account_lifecycle`, before the reply and
        // nothing for a no-op); a failed append is logged here and never fails a write that landed.
        let (done, journal_error) =
            vike_secrets::edit_account_in(&self.settings_dir, edit, self.account_journal(actor))
                .map_err(store_refused)?;
        log_journal_error(journal_error.as_ref(), "account lifecycle");
        if done.changed {
            // The row is the switch and the watcher polls it; the kick only saves the GUI the wait (decision
            // 0169 item 3), and a kick that finds nothing to do starts and stops nothing.
            crate::account_follow::kick();
        }
        let row = done.after.as_ref().or(done.before.as_ref()).map(|a| AccountRowWire {
            id: a.id,
            venue: a.venue.clone(),
            tier: a.tier.as_str().to_string(),
            label: a.label.clone(),
            venue_account_id: a.venue_account_id.clone(),
            active: a.active,
            last_verified_at: a.last_verified_at.clone(),
            keys: done.keys.clone(),
        });
        Ok(Response::AccountWritten(Box::new(AccountWriteWire {
            verb: done.verb.to_string(),
            // A REMOVE left no row; `after`/`before` above already falls back, and a row is still
            // reported for it so the reply names what went.
            row: done.after.is_none().then(|| row.clone()).flatten().or(row),
            changed: done.changed,
            note: note.filter(|_| done.changed),
        })))
    }

    /// **The verb that carries a credential VALUE.**
    ///
    /// It is a CALL SITE of `vike_secrets::save_credentials_to_store` — the ONE by-name upsert —
    /// never a second writer, which is `docs/decisions/0036`'s reason 1 answered on the schema that
    /// exists. Its one client is the desktop's "Data providers" group, so the NAME is held to the
    /// DEPLOYMENT names the wire may set ([`is_settable_credential_key`]: the data-provider keys and
    /// the attribution codes), which that upsert files BY NAME as deployment rows — no account, no
    /// venue, no tier. ⚠ **A credential write never creates an account** (decision 0156 item 6): an
    /// account's credential belongs to an EXISTING account and is addressed by the account and a
    /// FIELD, never by a composed name, so every other name — a venue key name, a labelled
    /// `…__LABEL` one — is REFUSED here with the route that does write it (the account verbs, then
    /// `vike-cli secrets set --id <ID> <FIELD>` or the Connections panel).
    ///
    /// ⚠ **The value's whole life in this process**: one `String` inside the deserialized frame, on
    /// this connection's own thread, for the length of this call — handed to the upsert, where it
    /// becomes a bind parameter. Nothing caches it, no response carries it, no log line and no
    /// error message reaches it, and `AccountRequestWire`'s hand-written `Debug` prints `<set>` in its
    /// place. ⚠ This workspace does not ZEROIZE, and 0065 §4 declares that residual rather than
    /// waiving it: the value also exists in the frame read buffer and in whatever `serde_json`
    /// allocated parsing it, and Rust drops without wiping.
    fn set_credential(
        &self,
        key: &str,
        value: &str,
        actor: &ChangeActor,
    ) -> Result<Response, HubRefusal> {
        // ⚠ The refusal names the KEY and never the value, and it offers no nearest-name
        // suggestion: an account's credential has no name to suggest.
        if !is_settable_credential_key(key) {
            let providers = vike_venues::credential_keys::DATA_PROVIDER_KEYS.join(", ");
            let key = key.to_string();
            return Err(AccountVerbRefusal::CredentialNameNotSettable { key, providers }.into());
        }
        // ⚠ An ABSENT store is REFUSED, not created. `docs/decisions/0036`'s rule, and its sharpest
        // instance on this surface: the settings database is the ONLY credential store (the FILE
        // store was removed on 2026-10-07), and creating it is `vike-cli secrets init`'s act
        // alone — an empty one minted here would skip the carry of any credential file on the box.
        if matches!(
            vike_secrets::backend_in(&self.settings_dir),
            vike_secrets::StoreBackend::Absent
        ) {
            let remedy = vike_secrets::CREATE_STORE_REMEDY;
            return Err(AccountVerbRefusal::NoCredentialStore { remedy }.into());
        }
        // The durable record is the writer's — `Change::credential_write`, whose signature takes NO
        // value parameter, which is the enforcement rather than a convention. The actor is the
        // WIRE's, carrying the non-secret key fingerprint this connection authenticated under.
        //
        // ⚠ The venue/tier cells are the DOCUMENTED spellings for a DEPLOYMENT row rather than
        // invented ones: the only names this verb writes belong to no account and so to no tier,
        // and deriving a venue from a name would be a second classifier of names. The key NAME is
        // in the record and is what a reader actually searches for.
        let (backend, journal_error) = vike_secrets::save_credentials_to_store(
            &self.settings_dir,
            vike_secrets::StoreTable::Credential,
            &[(key.to_string(), value.to_string())],
            vike_secrets::CredentialUpsertIntent::Add,
            vike_secrets::ChangeStampCredential {
                actor: actor.clone(),
                venue: vike_change_journal::VENUE_MULTI,
                tier: vike_change_journal::TIER_UNTIERED,
                process: change_journal_process().clone(),
                now_ms: vike_model::now_ms(),
            },
        )
        .map_err(store_refused)?;
        log_journal_error(journal_error.as_ref(), "credential write");
        let store = match backend {
            vike_secrets::StoreBackend::Database(db) => db.display().to_string(),
            // Unreachable: the writer refuses with no database, and the guard above refused first.
            vike_secrets::StoreBackend::Absent => {
                vike_secrets::db_path_in(&self.settings_dir).display().to_string()
            }
        };
        Ok(Response::AccountWritten(Box::new(AccountWriteWire {
            verb: "set-credential".to_string(),
            row: None,
            changed: true,
            note: Some(if vike_venues::credential_keys::DATA_PROVIDER_KEYS.contains(&key) {
                // A data-provider key files under no venue and no account: nothing arms, and the
                // lane that reads it re-reads the store every cycle.
                format!(
                    "{key} written to {store}. It arms no account: the datahub's tool-feeds lane \
                     re-reads it from the store each poll cycle (about a minute), so no restart \
                     is needed — provided that lane runs from this node's project."
                )
            } else {
                // An attribution code — the only other name `is_settable_credential_key` admits —
                // files under no account either: it tags orders, it arms nothing.
                format!(
                    "{key} written to {store}. It arms no account: an attribution code is a \
                     DEPLOYMENT name a venue's mount reads at boot, so a RUNNING daemon keeps the \
                     one it loaded until it restarts."
                )
            }),
        })))
    }

    /// **The credential DELETE** — the wire twin of `vike-cli secrets rm`, a CALL SITE of
    /// `vike_secrets::retire_credential_in_store` (decision 0144) and of nothing else:
    /// the row the node's store answers for `key` is marked retired and kept at rest.
    ///
    /// The refusals are the store's own, and the three a delete owes NAMES to are worded here from
    /// the typed `StoreErrorKind` the `io::Error` carries (a hub key, a name the store does not hold
    /// live, a spelling another spelling still answers for), none of which quotes a path or a value.
    /// The record is the one the call writes (`credential_retire`, names only); a journal failure is
    /// logged at `error` and never fails a retire that landed.
    fn retire_credential(&self, key: &str, actor: &ChangeActor) -> Result<Response, HubRefusal> {
        // The same name validator as the write, PLUS the names the store already holds for an
        // account ([`Self::held_account_key_names`]): the wire can no longer SET an account's
        // credential by name (decision 0156 item 6), but a GUI can still retire one it lists. A
        // retire never reaches any other name, and a hub key fails it here ("not a credential
        // key") before the store's own refusal would name it.
        if !is_settable_credential_key(key) && !self.held_account_key_names()?.contains(key) {
            return Err(AccountVerbRefusal::RetireUnknownName { key: key.to_string() }.into());
        }
        if matches!(
            vike_secrets::backend_in(&self.settings_dir),
            vike_secrets::StoreBackend::Absent
        ) {
            let remedy = vike_secrets::CREATE_STORE_REMEDY;
            return Err(AccountVerbRefusal::RetireNoCredentialStore { remedy }.into());
        }
        let (_, journal_error) = vike_secrets::retire_credential_in_store(
            &self.settings_dir,
            vike_secrets::StoreTable::Credential,
            key,
            vike_secrets::ChangeStampCredential {
                actor: actor.clone(),
                venue: vike_change_journal::VENUE_MULTI,
                tier: vike_change_journal::TIER_UNTIERED,
                process: change_journal_process().clone(),
                now_ms: vike_model::now_ms(),
            },
        )
        .map_err(|e| retire_refusal(key, &e))?;
        if let Some(err) = &journal_error {
            tracing::error!(
                error = %err,
                dir = %err.dir.display(),
                "vike-tradehub: credential retire NOT recorded to the change journal (the \
                 row IS retired)"
            );
        }
        Ok(Response::AccountWritten(Box::new(AccountWriteWire {
            verb: "retire-credential".to_string(),
            row: None,
            changed: true,
            note: Some(format!(
                "{key} removed: the node's store answers it as NOT SET from now on and keeps the \
                 row at rest (a deleted value stays in the database file — revoke a leaked key at \
                 its provider). A RUNNING daemon keeps the credentials it loaded at boot until it \
                 restarts; a lane that re-reads the store each cycle sees the removal at its next \
                 read."
            )),
        })))
    }

    /// **Which of these names does the store hold live** — the PRESENT-NAMES read behind the GUI's
    /// "set" / "not set" marks. `vike_secrets::CredentialStore::present_names` selects a boolean
    /// SQLite computes from the row, so no value becomes a Rust value in this process on this path,
    /// and the reply is names. Each asked name goes through the same validator as the write — or is
    /// a name the store already holds for an account ([`Self::held_account_key_names`]), as for the
    /// retire — and the list is short: it is a lookup of the handful of names a screen shows, not a
    /// way to enumerate the store. It reads under [`AccountAdminSource::scope`].
    fn held_credentials(&self, names: &[String]) -> Result<Response, HubRefusal> {
        if names.len() > MAX_HELD_CREDENTIAL_NAMES {
            let (max, asked) = (MAX_HELD_CREDENTIAL_NAMES, names.len());
            return Err(AccountVerbRefusal::LookupTooMany { max, asked }.into());
        }
        // Read only when a name needs it: a lookup of settable names alone touches no account row.
        let account_names = if names.iter().all(|n| is_settable_credential_key(n)) {
            std::collections::BTreeSet::new()
        } else {
            self.held_account_key_names()?
        };
        if let Some(bad) =
            names.iter().find(|n| !is_settable_credential_key(n) && !account_names.contains(*n))
        {
            return Err(AccountVerbRefusal::LookupUnknownName { name: bad.clone() }.into());
        }
        let scope = vike_secrets::CredentialKeyScope::of(names);
        let held = self.store().present_names(&scope).map_err(store_refused)?;
        Ok(Response::CredentialsHeld(Box::new(CredentialsHeldWire {
            held: held.into_iter().collect(),
        })))
    }
}

/// How many names one [`AccountVerbWire::HeldCredentials`] may ask about.
const MAX_HELD_CREDENTIAL_NAMES: usize = 16;

/// **The refusal a credential retire answers with**, worded from the typed `StoreErrorKind` the
/// `io::Error` carries (`vike_secrets::retire_credential_in_store` keeps the `StoreError` as its
/// source). The three refusals a delete owes names to get one sentence each, naming the KEY and
/// never a path; anything else is the store's own text, which is safe to print by construction
/// (a path, a key NAME, a reason — never a value).
fn retire_refusal(key: &str, error: &std::io::Error) -> HubRefusal {
    use vike_secrets::StoreErrorKind;
    let kind = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<vike_secrets::StoreError>())
        .map(|db| &db.kind);
    let key = key.to_string();
    match kind {
        Some(StoreErrorKind::HubKeyNotRetired { .. }) => {
            AccountVerbRefusal::RetireHubKey { key }.into()
        }
        Some(StoreErrorKind::NotHeld { .. }) => AccountVerbRefusal::RetireNotHeld { key }.into(),
        Some(StoreErrorKind::RetireLeavesASpelling { others, .. }) => {
            AccountVerbRefusal::RetireLeavesASpelling {
                key,
                others: others.join(", "),
                which: if others.len() == 1 { "that name" } else { "those names" },
            }
            .into()
        }
        _ => store_refused(error),
    }
}

/// **May the WIRE set `key`, by name?** — the wire's half of `docs/decisions/0036`'s reason 3
/// (*a typo'd key NAME silently creates a key nothing reads*), narrowed to the DEPLOYMENT names
/// this surface writes.
///
/// A credential belongs to its ACCOUNT and is addressed by account and FIELD, never by a name
/// (decision 0156 item 6), and a credential write never creates an account — so no venue key name
/// and no labelled `…__LABEL` name is settable here at all. What is left is a CLOSED set of names
/// that belong to no account, each filed by name as a deployment row:
///
/// * one of the two DATA-provider keys, `vike_venues::credential_keys::DATA_PROVIDER_KEYS`, which
///   the GUI's "Data providers" group edits on this box (decision 0144's ADDED note; read by the
///   datahub's tool-feeds lane from this box's store). Exactly two names, not
///   `STORE_KEYS_OUTSIDE_THE_GRID`: the pager token and the Telegram bot's stay a
///   `vike-cli secrets set` on the box;
/// * a venue's attribution code (`vike_venues::credential_keys::attribution_key_owner` names its
///   venue) — a deployment name a venue's mount reads at boot.
///
/// ⚠ **The refusal deliberately offers NO suggestions**: there is no terminal to print a grid into
/// on a remote surface, and the nearest name to an account's credential is nothing the wire may
/// write — so the message names the route that writes an account's field instead.
pub(super) fn is_settable_credential_key(key: &str) -> bool {
    vike_venues::credential_keys::DATA_PROVIDER_KEYS.contains(&key)
        || vike_venues::credential_keys::attribution_key_owner(key).is_some()
}

impl AccountAdminSource {
    /// The credential store this surface READS — the account listing and the presence lookup — over
    /// [`AccountAdminSource::settings_dir`], the SAME already-resolved directory every write here
    /// lands in, under [`AccountAdminSource::scope`]. No I/O until a read.
    fn store(&self) -> vike_secrets::CredentialStore {
        vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(&self.settings_dir)),
            self.scope,
        )
    }

    /// **The NAMES of every live credential row the store holds for an ACCOUNT** —
    /// `vike_secrets::CredentialStore::account_keys`, whose statement selects `name`, `field` and
    /// `account_id` and never a value — under [`AccountAdminSource::scope`]. These are the names
    /// [`AccountVerbWire::RetireCredential`] and [`AccountVerbWire::HeldCredentials`] still accept although
    /// the wire can no longer SET one (decision 0156 item 6): a GUI may retire, or ask after, a
    /// credential it lists. A box with no store, or no `account` table, holds none.
    fn held_account_key_names(&self) -> Result<std::collections::BTreeSet<String>, HubRefusal> {
        let keyed = self.store().account_keys().map_err(store_refused)?;
        Ok(keyed.into_iter().flat_map(|k| k.into_values()).flat_map(|a| a.names).collect())
    }

    /// WHO is writing, for the account writers' own record (`vike_secrets::ChangeStamp`): the
    /// WIRE actor, this process's identity and the instant. The writer appends the record to
    /// `<settings_dir>/state/changes` — the journal beside the SAME directory the write lands in,
    /// so the ledger and the store it describes can never resolve to two different projects.
    fn account_journal(&self, actor: &ChangeActor) -> vike_secrets::ChangeStamp {
        vike_secrets::ChangeStamp {
            actor: actor.clone(),
            process: change_journal_process().clone(),
            now_ms: vike_model::now_ms(),
        }
    }
}

/// Report a record the writer could not append, at `error`, WITHOUT failing the call.
///
/// The store write already happened, so reporting failure would send a caller down an error path
/// for a write that succeeded — `vike_secrets::save_credentials_to_store`'s ordering rule, and
/// `error` is the one level a `warn` file level (the live box's `preferences.log_file_level` row)
/// still lets through.
fn log_journal_error(error: Option<&vike_secrets::ChangeAppendError>, what: &str) {
    if let Some(err) = error {
        tracing::error!(
            error = %err.source,
            dir = %err.dir.display(),
            "vike-tradehub: {what} NOT recorded to the change journal (the write DID land)"
        );
    }
}

/// The identity every journal record of this process carries. `ChangeProcess::current` reads `current_exe`,
/// so it is resolved ONCE per process rather than per write; neither the value nor the read can
/// change during a run. One site for every journal context this surface hands `vike-secrets`'s
/// writers.
fn change_journal_process() -> &'static vike_change_journal::ChangeProcess {
    use vike_change_journal::ChangeProcess;

    static PROCESS: std::sync::OnceLock<ChangeProcess> = std::sync::OnceLock::new();
    PROCESS.get_or_init(|| ChangeProcess::current(env!("CARGO_PKG_VERSION")))
}

/// **The account plane's ADMISSION decision — part 2 of `docs/decisions/0065`'s barrier, split out
/// so it is decidable without a socket.**
///
/// This is the one place that says whether a frame reaching
/// [`vike_tradehub_client::proto::Request::Account`] may proceed, and it
/// is a free function for a measured reason rather than a stylistic one: `Request::Account` has
/// exactly ONE construction site in the tree — the arm that consumes it — so nothing builds the
/// frame and no test could drive the decision where it used to live, INSIDE that arm's `match`. A
/// 2026-09-17 mutation proved the cost: widening the accepting arm to `(Some(src), _)` and deleting
/// the refusal — i.e. any authenticated peer, Observe included, reaching `SetCredential` — left
/// 2911 tests green. 0065 calls this half load-bearing (it is what keeps the key a desktop carries
/// to place orders from being the key that writes key material), and it had no ratchet under it.
///
/// `has_capability` is `accounts.is_some()`, part 1 of the barrier: a box that DECLARED nothing
/// holds no writer, so the frame is refused because there is nothing to refuse WITH. The two
/// refusals are deliberately DIFFERENT strings — an absence and an authorization failure are not the
/// same fact, and an operator debugging one must not be sent looking for the other.
///
/// Part 3 (confidentiality) is decided at BOOT and is not knowable here, so it is carried on the
/// handle rather than asked at the frame — see [`AccountAdminSource`]'s own doc.
pub(super) fn account_admission(has_capability: bool, scope: Scope) -> Result<(), HubRefusal> {
    if !has_capability {
        // ⚠ The capability does not exist. The message names the DECLARATION rather than a flag to
        // flip, because flipping it is not sufficient: on `loopback` the bind has to agree.
        return Err(AccountVerbRefusal::NotArmed.into());
    }
    if scope != Scope::Account {
        // ⚠ The capability EXISTS and this peer is not Admin. Named as an authorization refusal
        // rather than as an absence, for the reason above.
        return Err(AccountVerbRefusal::AdminScopeRequired.into());
    }
    Ok(())
}
