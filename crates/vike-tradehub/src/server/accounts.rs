//! `server::accounts` — the node's ACCOUNT-ADMINISTRATION plane (`docs/decisions/0065`): the
//! capability handle ([`AccountAdminSource`]), the operator's barrier declaration
//! ([`AccountBarrier`]), the actor a write is journalled under ([`AccountActor`]) and the
//! admission decision (`account_admission`) that keeps the key a desktop carries to place orders
//! from being the key that writes key material.
//!
//! Split out of `server.rs` as a pure move; the module doc there carries the barrier's reachability
//! argument, and each type below carries its own.

use vike_tradehub_client::proto::{Response, Scope};
use vike_tradehub_client::wire::{
    AccountRequest, AccountVerb, WireAccountList, WireAccountRow, WireAccountWritten,
};

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
/// `crate::node::account_admin_source` is the one site, and it answers three questions in
/// order: has the operator DECLARED a barrier (`config.tradehub_account_admin`, three-valued —
/// unset/`off`, `loopback`, `contained`); if `loopback`, does [`super::bind_exposure`] agree that every
/// resolved address is loopback (a wide bind REFUSES the capability at boot, naming the flag and
/// the address); and is there an `VIKE_TRADEHUB_ADMIN_KEY` to authenticate against. All three must
/// answer yes.
///
/// ⚠ **This server cannot ask the first two itself and must not try.** §3b measures why: `serve`
/// receives an ALREADY-BOUND `TcpListener` and never calls `local_addr`, [`super::bind_exposure`] runs
/// once in the binary before the listener exists, and the per-frame PEER answers wrongly in both
/// directions (through an SSH tunnel it is loopback and correct; inside a container correctly
/// published to `127.0.0.1` it is the bridge gateway, so a CONTAINED deployment would be refused;
/// and the Telegram surface reaches [`super::control::accept_command`] with no peer at all). The exposure is
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
    /// was told to follow rather than invent one: the two destructive verbs
    /// ([`AccountVerb::Remove`], [`AccountVerb::SetCredential`]) are REFUSED unless `confirm`
    /// equals the exact thing being changed — the row id, or the credential key name. **Missing and
    /// mismatched get DISTINCT messages, each naming the expected spelling**, which is that arm's
    /// split. The client's job is to make the operator TYPE it and never pre-fill it; this
    /// method's job is to refuse anything else, so no client can quietly skip it.
    ///
    /// `vike_tradehub_client::wire::AccountVerb::required_confirm` is the ONE derivation of what
    /// the confirm must be, consulted by the client that prompts and by this method that refuses —
    /// so the two cannot answer differently about what the ceremony is.
    ///
    /// # What never leaves
    ///
    /// No reply carries a credential value on any path, and the refusals quote nothing the caller
    /// sent. `vike_secrets`' own refusals are safe to print by construction
    /// (`DbErrorKind::AccountHasCredentials` names key NAMES from a statement with no `value`
    /// column; `DbErrorKind::AccountLabelMalformed` echoes no token at all), and the one refusal
    /// composed here — an invalid credential KEY — names the key, which is not a secret, and never
    /// the value.
    ///
    /// # Errors
    /// `Err(reason)` is answered on the wire as [`Response::Error`]. Every one of them wrote
    /// nothing.
    pub(super) fn apply(
        &self,
        req: &AccountRequest,
        actor: &AccountActor<'_>,
    ) -> Result<Response, String> {
        // ⚠ THE CEREMONY FIRST, before the store is opened and before anything is validated —
        // `apply_set_setting`'s ordering, so a refused ceremony costs no lock and cannot be
        // distinguished from a refused one by timing what the store did.
        if let Some(expected) = req.verb.required_confirm() {
            match req.confirm.as_deref() {
                None => {
                    return Err(format!(
                        "`{}` changes something this node cannot put back — it must carry the \
                         typed confirm: re-send with confirm set to the exact value `{expected}`",
                        req.verb.word()
                    ));
                }
                Some(c) if c.trim() != expected => {
                    return Err(format!(
                        "account confirm mismatch: the confirm must equal the exact value \
                         `{expected}` — nothing was written. (What was sent is deliberately not \
                         quoted back: on this verb the field beside it carries a credential.)"
                    ));
                }
                Some(_) => {}
            }
        }

        match &req.verb {
            AccountVerb::List => self.list(),
            AccountVerb::Add { venue, tier, label } => {
                // The venue roster and the tier vocabulary are checked HERE rather than in the
                // store because the refusals print the roster, which is what makes them
                // actionable; the tier half (`vike_secrets::ACCOUNT_TIERS`, the second check
                // below) is the store's own vocabulary either way. ⚠ The reason this used to
                // give — "`vike-secrets` declares no `vike-*` dependency and cannot see
                // `vike_model::VENUES`" — is false since decision 0072: that crate's
                // `ensure_venue_rows` iterates that very roster. Where the check lives is now a
                // MESSAGE-QUALITY choice, not a layering one.
                if !vike_model::VENUES.contains(&venue.as_str()) {
                    return Err(format!(
                        "unknown venue '{venue}' — nothing was written. The roster is: {}",
                        vike_model::VENUES.join(", ")
                    ));
                }
                if !vike_secrets::ACCOUNT_TIERS.contains(&tier.as_str()) {
                    return Err(format!(
                        "unknown tier '{tier}' — nothing was written. It must be one of: {}. \
                         `paper` IS one of them: it is the account a {{VENUE}}_SIM_* credential \
                         mints, not the policy.venues.<venue> CEILING of the same name.",
                        vike_secrets::ACCOUNT_TIERS.join(" | ")
                    ));
                }
                self.write(
                    vike_secrets::AccountEdit::Create { venue, tier, label: label.as_deref() },
                    Some(format!(
                        "⚠ this arms NOTHING. A row is not a ceiling: policy.venues.{venue} is \
                         read ABOVE the credential store by the mount, so this venue stays PAPER \
                         until that line says otherwise."
                    )),
                    actor,
                )
            }
            // ⚠ **A WARNING, WHERE `docs/decisions/0065` §5 ASKS FOR A REFUSAL — a declared
            // deviation, not an oversight.** That record says of `rename`: *"plus a refusal when
            // the OLD label is named in `policy.toml`'s `[accounts.<venue>]` … with a REMOTE verb
            // the operator who renames and the operator who restarts need not be the same person,
            // so the refusal belongs at the edit."*
            //
            // What is here instead is this note, rendered on every rename that changed something,
            // naming the exact policy key. What the refusal would take, and why it was not built
            // with the rest: the label space `[accounts.<venue>]` addresses is the CREDENTIAL KEY
            // NAME grammar (`vike_model::accounts::account_keys::accounts_in_store` enumerates it), not the
            // `account` ROW's `label` column — the two agree on a labelled row and disagree on
            // every unlabelled one, and that table is the `accounts` field of
            // `crates/vike-config/src/policy.rs`'s `PolicyPatch`, which that file still documents
            // as parsed, validated, stored and *folded by nothing*. A refusal keyed on it would
            // therefore be refusing against a ceiling table whose consumer is mid-rollout, and
            // getting the label space wrong would refuse a LEGITIMATE rename — strictly worse than
            // the warning, because a refused rename has no override on this surface at all.
            //
            // What catches the ONE-STEP hazard: `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` refuses
            // the whole node to start, `vike_mount::arming`'s `Block::AccountNotInStore` reports
            // it, and the mount itself answers `DukascopyRefusal::NoSuchAccount`. Three independent
            // catches, all at the next restart.
            //
            // ⚠ **THIS ARM USED TO CLAIM THOSE THREE MADE A MISROUTE IMPOSSIBLE — "an outage, never
            // a misroute". THAT IS FALSE, and it was the sentence an operator would act on.**
            // Measured 2026-09-17. All three fire on a label that STOPS resolving; none of them
            // looks at a label that now resolves to a DIFFERENT row. Two accepted renames reach
            // that state with the policy file untouched:
            //
            //   0. row A = DUKASCOPY_DEMO1_* keys; row B = DUKASCOPY_DEMO2_* keys labelled `ALT`;
            //      `policy.accounts.dukascopy.ALT` armed → orders reach Dukascopy Europe IBS AS.
            //   1. rename B to `OLD` — `AccountKeysPinTheLabel` looks for keys ending in `__ALT`
            //      and dukascopy has none (it discriminates by a token INSIDE the base name,
            //      `DukascopyAccount::key_prefix`), so the one guard that could fire is
            //      STRUCTURALLY unable to on the only venue where a label selects a BROKER.
            //   2. rename A to `ALT` — `AccountLabelTaken` sees `ALT` free; `AccountLabelHeldAsBook`
            //      sees no active row booked `ALT`.
            //   3. restart → `resolve_account` matches row A on `r.label` → Dukascopy Bank SA.
            //
            // All three catches find an armed, unambiguous, resolvable account, so none fires.
            // `DukascopyAccount::broker`'s own doc states the stake: *a fallback would route an
            // order to a broker nobody chose.* The guard 0065 §5 specifies is what closes this; it
            // is NOT built here, and this comment is the honest statement of that gap rather than
            // the reassurance that used to sit in its place. The warning below is therefore the
            // only thing in front of the two-step case, and a warning is not a guard.
            AccountVerb::Rename { id, label } => self.write(
                vike_secrets::AccountEdit::Rename { id: *id, label: label.as_deref() },
                Some(
                    "⚠ if a policy.accounts.<venue>.<LABEL> row names this account by its OLD \
                     label, that row stops resolving and a mount addressing it is REFUSED at the \
                     next restart. Update the policy row too. ⚠ And if you then give the OLD \
                     label to a DIFFERENT account of the same venue, nothing refuses anything: \
                     the policy row resolves again, to the other account. On dukascopy the two \
                     demo accounts are different LEGAL ENTITIES, so that is an order routed to a \
                     broker nobody chose. Check which row the label names before you restart."
                        .to_string(),
                ),
                actor,
            ),
            AccountVerb::SetActive { id, active } => self.write(
                vike_secrets::AccountEdit::SetActive { id: *id, active: *active },
                (!*active).then(|| {
                    "⚠ a RUNNING daemon does not notice: the arming snapshot is read ONCE at boot, \
                     so its engines keep their credentials and keep trading until it restarts. The \
                     row and its credential keys are still there, which is what makes this \
                     reversible."
                        .to_string()
                }),
                actor,
            ),
            AccountVerb::Remove { id } => {
                self.write(vike_secrets::AccountEdit::Remove { id: *id }, None, actor)
            }
            AccountVerb::SetCredential { key, value } => self.set_credential(key, value, actor),
            AccountVerb::SetBook { id, venue_account_id, replace } => {
                self.set_book(*id, venue_account_id.as_deref(), *replace, actor)
            }
        }
    }

    /// **WRITE one row's `venue_account_id`** — the wire twin of `vike-cli secrets set-book`.
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
    /// `vike_model::account_confirmation`'s whole module exists to remove. The CLI's own call site
    /// carries the same note for the same reason.
    fn set_book(
        &self,
        id: i64,
        venue_account_id: Option<&str>,
        replace: bool,
        actor: &AccountActor<'_>,
    ) -> Result<Response, String> {
        let done = vike_secrets::set_venue_account_id_in(
            &self.settings_dir,
            id,
            venue_account_id,
            replace,
            vike_secrets::BookSource::Operator,
        )
        .map_err(|e| e.to_string())?;

        let row = WireAccountRow {
            id: done.before.id,
            venue: done.before.venue.clone(),
            tier: done.before.tier.clone(),
            label: done.before.label.clone(),
            // The column AFTER the write — `before` is what the transaction found, and a reply that
            // echoed it would tell the operator their write had not happened.
            venue_account_id: done.venue_account_id.clone(),
            active: done.before.active,
            last_verified_at: done.before.last_verified_at.clone(),
            keys: Vec::new(),
        };
        let note = (!done.changed).then(|| {
            "the row already named exactly this book, so nothing was written. Re-asserting a known \
             book is not a mistake and is not refused."
                .to_string()
        });
        self.journal_book_write(&done, actor);
        Ok(Response::AccountWritten(Box::new(WireAccountWritten {
            verb: if venue_account_id.is_some() { "set-book" } else { "clear-book" }.to_string(),
            row: Some(row),
            changed: done.changed,
            note,
        })))
    }

    /// The listing. **NO value is selected anywhere on this path** — `read_accounts` never touches
    /// the `credential` table at all, and `read_account_keys`' statement is
    /// `SELECT name, field, account_id`.
    fn list(&self) -> Result<Response, String> {
        let store = vike_secrets::db_path_in(&self.settings_dir);
        let accounts =
            vike_secrets::resolve_accounts_in(&self.settings_dir).map_err(|e| e.to_string())?;
        let rows = match &accounts {
            vike_secrets::Accounts::Known(rows) => rows,
            vike_secrets::Accounts::Unanswerable(why) => {
                return Err(format!(
                    "{why} — so this node has no account table to list. Move the box into the \
                     settings database first: `vike-cli secrets init`."
                ));
            }
        };
        let keyed = vike_secrets::resolve_account_keys_in(&self.settings_dir)
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let rows = rows
            .iter()
            .map(|a| WireAccountRow {
                id: a.id,
                venue: a.venue.clone(),
                tier: a.tier.clone(),
                label: a.label.clone(),
                venue_account_id: a.venue_account_id.clone(),
                active: a.active,
                last_verified_at: a.last_verified_at.clone(),
                keys: keyed.get(&a.id).map(|k| k.names.clone()).unwrap_or_default(),
            })
            .collect();
        Ok(Response::AccountList(Box::new(WireAccountList {
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
        actor: &AccountActor<'_>,
    ) -> Result<Response, String> {
        let done =
            vike_secrets::edit_account_in(&self.settings_dir, edit).map_err(|e| e.to_string())?;
        let row = done.after.as_ref().or(done.before.as_ref()).map(|a| WireAccountRow {
            id: a.id,
            venue: a.venue.clone(),
            tier: a.tier.clone(),
            label: a.label.clone(),
            venue_account_id: a.venue_account_id.clone(),
            active: a.active,
            last_verified_at: a.last_verified_at.clone(),
            keys: done.keys.clone(),
        });
        // The durable record, before the reply: a completed write with no ledger line is the shape
        // `run_set_book`'s ordering note argues against.
        self.journal_lifecycle(&done, actor);
        Ok(Response::AccountWritten(Box::new(WireAccountWritten {
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
    /// It is a CALL SITE of `vike_secrets::save_credentials_to_store` — the ONE upsert — never a
    /// second writer, which is `docs/decisions/0036`'s reason 1 answered on the schema that exists.
    /// The key NAME is validated against `vike_model::credential_keys` exactly as
    /// `vike-cli secrets set` validates it (reason 3, reused verbatim), and the classifier is the
    /// same `vike_bridge_core::credentials::classify_credential_name` every other caller passes.
    ///
    /// ⚠ **The value's whole life in this process**: one `String` inside the deserialized frame, on
    /// this connection's own thread, for the length of this call — handed to the upsert, where it
    /// becomes a bind parameter. Nothing caches it, no response carries it, no log line and no
    /// error message reaches it, and `AccountRequest`'s hand-written `Debug` prints `<set>` in its
    /// place. ⚠ This workspace does not ZEROIZE, and 0065 §4 declares that residual rather than
    /// waiving it: the value also exists in the frame read buffer and in whatever `serde_json`
    /// allocated parsing it, and Rust drops without wiping.
    fn set_credential(
        &self,
        key: &str,
        value: &str,
        actor: &AccountActor<'_>,
    ) -> Result<Response, String> {
        // ⚠ The refusal names the KEY and never the value — and the suggestions are SUPPRESSED for
        // a labelled key, because the nearest name to `HYPERLIQUID_LIVE_API_KEY__ALT` is a
        // DIFFERENT ACCOUNT's live key. `lookup_keys` is the same validator the CLI uses.
        if !is_settable_credential_key(key) {
            return Err(format!(
                "'{key}' is not a credential key this workspace reads, so nothing was written — a \
                 typo'd name would sit in the store forever with nothing looking for it. \
                 `vike-cli config show` lists the names this workspace reads."
            ));
        }
        // ⚠ An ABSENT store is REFUSED, not created. `docs/decisions/0036`'s rule, and its sharpest
        // instance on this surface: the settings database is the ONLY credential store (the FILE
        // store was removed on 2026-10-07), and creating it is `vike-cli secrets init`'s act
        // alone — an empty one minted here would skip the carry of any credential file on the box.
        if matches!(vike_secrets::backend_in(&self.settings_dir), vike_secrets::Backend::Absent) {
            return Err(format!(
                "this node has no credential store at all, and nothing here will create one: a \
                 store is an operator's only copy of their live venue keys. On the box, {}, then \
                 retry.",
                vike_secrets::CREATE_STORE_REMEDY
            ));
        }
        let backend = vike_secrets::save_credentials_to_store(
            &self.settings_dir,
            vike_secrets::Table::Credential,
            &[(key.to_string(), value.to_string())],
            Some(&vike_bridge_core::credentials::classify_credential_name),
        )
        .map_err(|e| e.to_string())?;
        // The durable record — `Change::credential_write`, whose signature takes NO value
        // parameter, which is the enforcement rather than a convention. The actor is the WIRE's,
        // carrying the non-secret key fingerprint this connection authenticated under.
        self.journal_credential(key, actor);
        let store = match backend {
            vike_secrets::Backend::Database(db) => db.display().to_string(),
            // Unreachable: the writer refuses with no database, and the guard above refused first.
            vike_secrets::Backend::Absent => {
                vike_secrets::db_path_in(&self.settings_dir).display().to_string()
            }
        };
        Ok(Response::AccountWritten(Box::new(WireAccountWritten {
            verb: "set-credential".to_string(),
            row: None,
            changed: true,
            note: Some(format!(
                "{key} written to {store}. ⚠ this arms NOTHING on its own — policy.venues is read \
                 ABOVE the credential store by the mount, and a RUNNING daemon keeps its \
                 boot-time credentials until it restarts."
            )),
        })))
    }
}

/// **Is `key` a credential name this workspace actually reads?** — the wire's half of
/// `docs/decisions/0036`'s reason 3 (*a typo'd key NAME silently creates a key nothing reads*),
/// reused verbatim rather than re-derived.
///
/// Two shapes are settable, and they are the same two `vike-cli secrets set` accepts:
///
/// * a name in `vike_model::credential_keys::lookup_keys` — the grid;
/// * a LABELLED account's key, `{BASE}__{LABEL}`, whose BASE resolves. The grammar is
///   `vike_model::account_keys`': split at the FIRST `ACCOUNT_SEPARATOR`, which is a DOUBLE
///   underscore — so a single-underscore near-miss (`..._API_KEY_ALT`, which nothing reads) is NOT
///   one of these and is correctly refused.
///
/// ⚠ **The refusal above deliberately offers NO suggestions**, which is the one place this differs
/// from the CLI's message: the CLI computes nearest names and SUPPRESSES them for a labelled key,
/// because the nearest name to `HYPERLIQUID_LIVE_API_KEY__ALT` is a DIFFERENT ACCOUNT's live key.
/// On a remote surface the same hazard applies to every shape, and there is no terminal to print a
/// grid into — so the message names the command that prints one instead.
pub(super) fn is_settable_credential_key(key: &str) -> bool {
    if vike_model::credential_keys::lookup_keys().iter().any(|k| k == key) {
        return true;
    }
    match key.split_once(vike_model::accounts::account_keys::ACCOUNT_SEPARATOR) {
        Some((base, label)) => {
            vike_model::credential_keys::key_owner(base).is_some()
                && vike_model::accounts::account_keys::AccountLabel::parse(label).is_ok()
        }
        None => false,
    }
}

/// **WHO performed an account write**, resolved once per connection and threaded in — the peer, the
/// granted scope and the NON-SECRET fingerprint of the key whose mac verified.
///
/// The same three cells [`super::control::accept_command`] already records for a settings write, and they exist
/// here for the reason `docs/decisions/0065` §4.3 gives: `vike_model::change_journal::Actor::Wire`
/// *"already carries a stable NON-SECRET key fingerprint, taken under a domain separator that is
/// deliberately not one of the two protocol signing domains, so a remote credential write is
/// attributable to a KEY without the key. That is an existing asset to use, not a thing to build."*
///
/// A borrowed struct rather than three parameters, so a call site cannot transpose the peer and the
/// key id — two `Option<&str>` cells that render identically when they are wrong.
pub struct AccountActor<'a> {
    /// `TcpStream::peer_addr` as [`super::handle_connection`] bound it, rendered. `None` on a surface with
    /// no socket.
    pub peer: Option<&'a str>,
    /// The granted scope's word — always `admin` on this path, carried rather than assumed so a
    /// widening shows up in the ledger instead of being invisible in it.
    pub scope: &'a str,
    /// `NodeKeys::key_id` for the scope that authenticated. `None` is unreachable on this path (the
    /// scope was granted, so its key is non-empty) and is carried honestly rather than unwrapped —
    /// an absent key must record NO id, never an invented one.
    pub key_id: Option<&'a str>,
}

impl AccountActor<'_> {
    fn to_actor(&self) -> vike_model::change_journal::Actor {
        vike_model::change_journal::Actor::wire(self.peer, Some(self.scope), self.key_id)
    }
}

impl AccountAdminSource {
    /// The durable CHANGE JOURNAL for this daemon's project — `<settings_dir>/state/changes`.
    ///
    /// Derived from [`AccountAdminSource::settings_dir`], the SAME already-resolved directory the
    /// write itself lands in, rather than from a fresh walk — so the ledger and the store it
    /// describes can never resolve to two different projects.
    /// [`super::settings::SettingsShowSource::change_journal`]
    /// is the same derivation for the settings plane.
    fn change_journal(&self) -> vike_model::change_journal::ChangeJournal {
        use vike_model::change_journal::{ChangeJournal, Proc};
        use vike_model::paths::state_path::STATE_SUBDIR;

        // `Proc::current` reads `current_exe`, so it is resolved ONCE per process rather than per
        // write. Neither the value nor the read can change during a run.
        static PROCESS: std::sync::OnceLock<Proc> = std::sync::OnceLock::new();
        let process = PROCESS.get_or_init(|| Proc::current(env!("CARGO_PKG_VERSION")));
        ChangeJournal::in_state_dir(&self.settings_dir.join(STATE_SUBDIR), process.clone())
    }

    /// Record one lifecycle write. Key NAMES only, and the SIGNATURE is the enforcement:
    /// `Change::account_lifecycle` takes no value parameter, exactly as `credential_write` and
    /// `account_book` do not.
    /// Record one BOOK write into the durable ledger — the wire twin of `vike-cli secrets`'
    /// `record_book_write`, through the same `Change::account_book` constructor so the two surfaces
    /// cannot produce differently-shaped rows for the same act.
    ///
    /// ⚠ **Nothing is recorded when nothing changed**, and the rule is sharper than it looks: a
    /// ledger line for a no-op reads as a RE-POINTING that did not happen, which is the one thing a
    /// reader of this channel must not be told. `changed` is a claim about the BOOK alone.
    fn journal_book_write(&self, done: &vike_secrets::BookWrite, actor: &AccountActor<'_>) {
        use vike_model::change_journal::{Change, Outcome};

        if !done.changed {
            return;
        }
        let change = Change::account_book(
            Outcome::Applied,
            actor.to_actor(),
            vike_secrets::DB_FILE,
            done.before.id,
            &done.before.venue,
            &done.before.tier,
            done.before.venue_account_id.as_deref(),
            // `None` is a CLEAR — old present, new absent — which `AccountBookTarget::cleared`
            // reads. A repair therefore shows as *cleared, assigned* rather than as one edit.
            done.venue_account_id.as_deref(),
        );
        self.append(&change, "account book");
    }

    fn journal_lifecycle(&self, done: &vike_secrets::AccountWrite, actor: &AccountActor<'_>) {
        use vike_model::change_journal::{Change, Outcome};

        // Nothing to record when nothing changed: a ledger line for a no-op reads as an edit that
        // did not happen, which is `vike-cli`'s `record_book_write` rule and for its reason.
        if !done.changed {
            return;
        }
        let Some(row) = done.after.as_ref().or(done.before.as_ref()) else { return };
        let keys: Vec<&str> = done.keys.iter().map(String::as_str).collect();
        let change = Change::account_lifecycle(
            Outcome::Applied,
            actor.to_actor(),
            vike_secrets::DB_FILE,
            done.verb,
            row.id,
            &row.venue,
            &row.tier,
            done.before.as_ref().and_then(|b| b.label.as_deref()),
            done.after.as_ref().and_then(|a| a.label.as_deref()),
            done.after.as_ref().is_some_and(|a| a.active),
            &keys,
        );
        self.append(&change, "account lifecycle");
    }

    /// Record one credential write. ⚠ `Change::credential_write` takes NO value parameter — that is
    /// the enforcement, and it is why this function cannot leak one however it is called.
    fn journal_credential(&self, key: &str, actor: &AccountActor<'_>) {
        use vike_model::change_journal::{Change, Outcome, TIER_UNTIERED, VENUE_MULTI};

        // ⚠ The venue/tier cells are the DOCUMENTED spellings for a write this surface cannot
        // decompose rather than invented ones: this verb takes a KEY NAME, and deriving a venue
        // from it would be a second classifier beside
        // `vike_bridge_core::credentials::classify_credential_name` — the exact duplication
        // `vike_secrets`' injected-classifier seam exists to avoid. The key NAME is in the record
        // and is what a reader actually searches for.
        let change = Change::credential_write(
            Outcome::Applied,
            actor.to_actor(),
            vike_secrets::DB_FILE,
            VENUE_MULTI,
            TIER_UNTIERED,
            &[key],
        );
        self.append(&change, "credential write");
    }

    /// Append, and report a failure at `error` WITHOUT failing the call.
    ///
    /// The store write already happened, so reporting failure would send a caller down an error
    /// path for a write that succeeded — `vike_secrets::save_credentials_to_store_journalled`'s
    /// ordering rule, and `error` is the one level `deploy/vike-tradehub-project.service`'s
    /// `VIKE_LOG_FILE_LEVEL=warn` still lets through.
    fn append(&self, change: &vike_model::change_journal::Change, what: &str) {
        let journal = self.change_journal();
        if let Err(e) = journal.append(vike_model::now_ms(), change) {
            tracing::error!(
                error = %e,
                dir = %journal.dir().display(),
                "vike-tradehub node: {what} NOT recorded to the change journal (the write DID land)"
            );
        }
    }
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
pub(super) fn account_admission(has_capability: bool, scope: Scope) -> Result<(), String> {
    if !has_capability {
        // ⚠ The capability does not exist. The message names the DECLARATION rather than a flag to
        // flip, because flipping it is not sufficient: the key has to exist too, and on `loopback`
        // the bind has to agree.
        return Err("account administration is not armed on this node: it holds no account \
                    writer at all. It is armed by DECLARING the barrier this wire's \
                    confidentiality comes from — the `config.tradehub_account_admin` setting = \
                    \"loopback\" (checked against the bind) or \"contained\" (asserted by the \
                    operator) — plus a VIKE_TRADEHUB_ADMIN_KEY in the node-key store. This \
                    node advertised no `account-verbs` capability, which is how a conforming \
                    client knows without asking."
            .into());
    }
    if scope != Scope::Account {
        // ⚠ The capability EXISTS and this peer is not Admin. Named as an authorization refusal
        // rather than as an absence, for the reason above.
        return Err("account administration requires the ADMIN scope — a Control key cannot \
                    reach key material on this node. Authenticate with \
                    VIKE_TRADEHUB_ADMIN_KEY."
            .into());
    }
    Ok(())
}
