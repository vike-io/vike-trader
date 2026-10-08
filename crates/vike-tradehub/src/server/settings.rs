//! `server::settings` — the node's SETTINGS verbs: the read half (`Request::SettingsShow` and
//! `Request::Directory`, answered from [`SettingsShowSource`]) and the one write
//! (`SettingsShowSource::apply_set_setting`, reached from an accepted `WireCommand::SetSetting`).
//!
//! Split out of `server.rs` as a pure move: the module doc there carries the contract this file
//! serves (the freshness, redaction and scope arguments are on [`SettingsShowSource`] itself).

use vike_tradehub_client::liveness;
use vike_tradehub_client::proto::Response;
use vike_tradehub_client::wire::{
    WireDirectory, WireDirectoryAccount, WireDirectoryVenue, WireSettingsRow, WireSettingsShow,
};

/// **The budget this daemon gives the settings database's write lock: 1 s** — what
/// [`SettingsShowSource::apply_set_setting`] passes to `vike_config::write_setting_row`.
///
/// ⚠ A settings write here runs on a CONNECTION THREAD, and the shared writer's ~3 s default is
/// argued for a CLI that is the whole process. It is not one here, and the two ends of the range
/// are both real:
///
/// * **Not zero.** Unlike the GUI, this waiter has nothing to paint and nobody watching a frame,
///   and a refusal is not free to the peer: it costs a round trip and one of the
///   [`super::control::ControlLimits`] rate tokens that peer is metered on. Turning a millisecond overlap with
///   this box's own `vike-cli config set` into a wire refusal would spend a token on a race
///   nobody was in.
/// * **Not long.** The thing being held is a SOCKET. The client's own reply deadline is
///   [`liveness::CONTROL_REPLY_TIMEOUT`], and blocking past it is strictly worse than refusing:
///   the peer gives up on a write that then LANDS, with nobody left to hear the answer — the one
///   failure shape a settings write must not have. An accepted HOT write can already add
///   [`crate::hot_reload::HOT_APPLY_DEADLINE`] on this same thread AFTER the lock, so the worst
///   case a peer sees is the two together, which the assertion below holds inside a fifth of its
///   deadline.
///
/// And the peer has the one thing neither other caller has: a retry loop above it. A `Busy`
/// refusal here is an answer it can act on, not a dead end.
///
/// ⚠ **This said the budget below was DEAD on the shipped sandbox, and it is REVERSED (0086 point
/// 6, the owner's 2026-09-26 ruling — *"that is a bullshit rule"*).** It was true under
/// `docs/decisions/0068`: a settings write used to go through the FILE mechanics, and
/// `deploy/vike-tradehub.service`'s `ProtectSystem=strict`/`ReadWritePaths=` grant named only
/// `<project>/settings/state`, so the settings FILE directory was read-only in this daemon's own
/// mount namespace and every write refused at the lock before this budget was spent. That grant
/// widened on 2026-09-18 to include `settings/db` — 0068's own measurement — and this daemon now
/// writes through it directly, via [`vike_config::write_setting_row`], no file mechanics and no
/// sentinel lock involved at all. So the number below is genuinely exercised on the shipped
/// deployment: every accepted `SetSetting` on a real the build runner box now spends up to it.
pub const SETTINGS_LOCK_BUDGET: vike_config::LockBudget =
    vike_config::LockBudget::from_millis(1_000);

// The `PUSH_WRITE_TIMEOUT` idiom above: a RANGE at compile time, so a deliberate tweak stays free
// while "the bound was effectively removed" — in either direction — does not compile.
const _: () = assert!(
    SETTINGS_LOCK_BUDGET.max_wait_ms() > 0
        && SETTINGS_LOCK_BUDGET.max_wait_ms()
            + crate::hot_reload::HOT_APPLY_DEADLINE.as_millis() as u64
            <= liveness::CONTROL_REPLY_TIMEOUT.as_millis() as u64 / 5,
    "a SetSetting must never hold a peer near its own reply deadline, and must never refuse \
     without waiting at all — SETTINGS_LOCK_BUDGET's doc argues each edge"
);

/// WHERE the node's [`vike_tradehub_client::proto::Request::SettingsShow`] answer comes from (split-plane REQ-7, read half):
/// the boot-resolved settings DIRECTORY plus the daemon's own startup environment sweep, threaded
/// in by the BINARY the way the identity block was for B3 — the server holds a handle, the binary
/// owns the I/O and the env read (the settings-registry rule).
///
/// ## Freshness — the CLI's semantics, deliberately
///
/// `vike-cli config show` re-reads the settings files on every invocation, so this source does
/// too: each request runs `vike_config::describe` (which re-loads the TOMLs) over the SAME
/// directory the daemon resolved at boot and the SAME env sweep it booted with. Two consequences,
/// both intended:
///
/// - A file edited AFTER boot answers with the NEWER value — i.e. the rows show what a restart
///   would load, which is exactly the view the WRITE half's restart-to-apply flow needs (and the
///   same window `main.rs`'s startup disclosure already accepts for `vike_config::boot_lines`).
/// - Settings that no longer LOAD (a typo'd key written after boot) answer an honest
///   [`Response::Error`] naming the file, never a fabricated table.
///
/// The env half does NOT re-sweep: a daemon's environment is fixed at spawn, so the boot sweep IS
/// the current one, and re-sweeping would make a library read global state its caller cannot see.
///
/// ## Scope — why [`vike_tradehub_client::proto::Scope::Read`] suffices (the redaction argument)
///
/// The payload is the FILES half of `config show` ONLY — `vike_config::file_rows`, the
/// shared builder whose rows are **redacted on construction** (`vike_config::show`'s
/// `resolve_file_row` applies `vike_config::is_secret_key` to every key; no settings
/// field is credential-shaped today, and the insurance is pinned by that module's
/// `a_secret_settings_value_never_enters_the_file_row` plus this crate's
/// `the_daemons_real_effective_rows_cross_the_wire` daemon test, whose planted secret must never
/// reach the serialized payload). The CLI's ENV-REGISTRY half — several hundred rows whose
/// credential grid discloses `<set>`/`<unset>` per credential KEY — is deliberately NOT served.
/// ⚠ That withholds the KEY GRID; it is not a claim that nothing here tells an Observe peer which
/// venues hold live credentials, because [`vike_tradehub_client::proto::Request::Directory`] does, on the owner's ruling (the
/// last section below). What remains in THIS reply is paths, addresses and flags: the same
/// disclosure class as the identity block and snapshot an Observe peer already reads. Read-only,
/// so no audit record either — the audit trail records COMMANDS, and this verb executes nothing
/// (same treatment as `Snapshot`/`StrategyStatus`).
///
/// ## It answers a second read, [`vike_tradehub_client::proto::Request::Directory`], and that is NOT this section's bargain
///
/// The same boot-resolved directory feeds `SettingsShowSource::directory`, which reads two
/// DIFFERENT tables (`venue` and `account`) and carries no settings row at all, so the redaction
/// argument above does not cover it. ⚠ **It serves the enumeration decision 0065 section 5 keeps
/// `Scope::Account`-only** (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`):
/// each active account's venue and tier says which venues hold live-tier accounts. It is licensed
/// by the owner's ruling of 2026-09-30, not by anything it leaves out, and it carries no credential
/// value, no key name and nothing from the `credential` table. That method's doc is the authority
/// for what is served and why.
#[derive(Clone, Debug)]
pub struct SettingsShowSource {
    /// `<project>/settings` as the daemon's ONE boot walk resolved it (`vike_boot::Booted`'s
    /// `settings_dir`); `None` = no project above the working directory (every row reports its
    /// compiled-in default — an honest answer, loudly rendered).
    pub settings_dir: Option<std::path::PathBuf>,
    /// The daemon's startup `std::env::vars()` sweep, owned by the binary (the same map its own
    /// settings load consumed), so the env layer here can never disagree with the one the daemon
    /// booted on.
    pub env: std::collections::HashMap<String, String>,
    /// The HOT-APPLY seam (REQ-7 v2): the queue whose other end the daemon's summary tick drains
    /// ([`crate::hot_reload`]). `None` — a daemon that wired no tick-side applier, and every
    /// test fixture that predates the seam — keeps v1 behaviour: every accepted write answers
    /// restart-to-apply.
    pub hot: Option<crate::hot_reload::HotApplyHandle>,
}

impl SettingsShowSource {
    /// The durable CHANGE JOURNAL for this daemon's project —
    /// `<settings_dir>/state/changes` — or `None` when the boot walk found no project.
    ///
    /// Derived from [`SettingsShowSource::settings_dir`], the SAME already-resolved directory the
    /// settings write itself lands in, rather than from a fresh walk. That is
    /// `vike_model::paths::state_path::user_data_dir_beside`'s rule and it is load-bearing here: the bare
    /// walk is `$VIKE_SETTINGS_DIR`-blind, all three shipped units set that variable precisely so
    /// the answer stops depending on `WorkingDirectory=`, and a journal resolved a second way would
    /// describe one project's ceiling while sitting in another project's folder.
    ///
    /// `None` — no project above the working directory — is the same honest degradation every row
    /// of the `SettingsShow` reply already makes: nothing is journalled, and the `tracing` line
    /// remains, exactly as before the journal existed.
    pub fn change_journal(&self) -> Option<vike_model::change_journal::ChangeJournal> {
        use vike_model::change_journal::{ChangeJournal, Proc};
        use vike_model::paths::state_path::STATE_SUBDIR;

        // `Proc::current` reads `current_exe`, so it is resolved ONCE per process rather than per
        // settings write. Neither the value nor the read can change during a run.
        static PROCESS: std::sync::OnceLock<Proc> = std::sync::OnceLock::new();
        let process = PROCESS.get_or_init(|| Proc::current(env!("CARGO_PKG_VERSION")));
        let state = self.settings_dir.as_deref()?.join(STATE_SUBDIR);
        Some(ChangeJournal::in_state_dir(&state, process.clone()))
    }

    /// Build the [`vike_tradehub_client::proto::Request::SettingsShow`] reply: describe → the shared row builder → wire rows.
    /// A load failure answers [`Response::Error`] naming the cause.
    pub(super) fn response(&self) -> Response {
        // ⚠ **THROUGH THE SETTINGS DATABASE, for the reason this reply exists at all.**
        // `docs/decisions/0086` makes the rows THE settings layer, so a reply built without the
        // store would describe compiled-in defaults for every key while the daemon runs armed from
        // its rows — and this is the surface the GUI's Connections panel renders as "what the box is
        // running on". A panel that can disagree with the process it is attached to is the defect
        // `vike_config::Origin` exists to make impossible, wearing a different carrier.
        //
        // A store that will not OPEN degrades rather than failing the reply: this is a DISCLOSURE
        // surface on a daemon that is already running, so a refusal here removes the panel an
        // operator would use to find out why. `Description::store_refusal` carries the fact instead
        // and the reply renders it.
        //
        // ⚠ **This reply RE-RESOLVES on every request, and there is no longer an "adoption window"
        // to name.** Before `docs/decisions/0086` a fresh read could answer from the rows while the
        // core was still folding a resolution built from the four files, because crossing over was
        // an operator act performed with the daemon running. There is one source now, so a fresh
        // read and the running core's own resolution can only ever differ by WHICH ROW a write
        // landed after this daemon's own last read — an ordinary race a write's own `old -> new`
        // report already covers, not a second layer this reply has to call out.
        let store = self.settings_dir.as_deref().map(vike_secrets::read_settings_in);
        if let Some(Err(e)) = &store {
            tracing::warn!(error = %e, "the settings database could not be read; describing without it");
        }
        let mut store_refusal_scratch = String::new();
        let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal_scratch);
        match vike_config::describe_with_source(self.settings_dir.as_deref(), source, &self.env) {
            Ok(d) => {
                let rows = vike_config::file_rows(&d, None, false)
                    .into_iter()
                    .map(|r| {
                        let read_by = r.read_cell().to_string();
                        WireSettingsRow {
                            section: r.file.to_string(),
                            key: r.key,
                            value: r.value,
                            origin: r.origin,
                            read_by,
                        }
                    })
                    .collect();
                let settings_dir = d.settings_dir.map(|p| p.display().to_string());
                Response::SettingsShow(Box::new(WireSettingsShow { settings_dir, rows }))
            }

            Err(e) => Response::Error(format!("settings could not be loaded: {e}")),
        }
    }

    /// Build the [`vike_tradehub_client::proto::Request::Directory`] reply from the settings database beside
    /// [`SettingsShowSource::settings_dir`]: every `venue` row whose key is on this node's roster
    /// (`vike_model::VENUES`) and every ACTIVE `account` row. Neither reader selects a
    /// credential or a key name.
    ///
    /// ⚠ **Only venues this node can mount are listed, and the ACCOUNT list is not filtered the same
    /// way.** The store keeps a `venue` row the roster has dropped ON PURPOSE (`vike_secrets`'s
    /// `ensure_venue_rows` never deletes one, because a live `account` row may still reference it),
    /// so listing every row would offer a picker a venue this node cannot mount. An ACTIVE account
    /// on a dropped venue is still listed: it is a fact the operator needs (the node holds an
    /// account it can no longer run), and a caller falls back to the account's own venue key.
    ///
    /// ⚠ **This reply IS the enumeration
    /// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` section 5 keeps
    /// `Scope::Account`-only, and the observe key may read it because the owner ruled so on
    /// 2026-09-30 — not because key names are left out.** That record keeps the account LISTING
    /// behind the admin key because *a listing enumerates which venues hold live credentials*.
    /// Every active row here carries its venue and its tier, so an observe peer learns which venues
    /// hold live-tier accounts; and for an unlabelled account on a standard venue, the venue and
    /// tier are enough to construct the key names (`{VENUE}_{TIER}_API_KEY`). Only the name STRINGS
    /// and the values are withheld. The licence is the owner's answer to *may the observe key read
    /// the account list — venue, label and mode, no API keys?*; the broker's own number for the
    /// book (`venue_account_id`) was added by the Trade window plan and the owner did not object.
    ///
    /// What the reply does NOT carry is any credential value, any credential key name, or anything
    /// from the `credential` table: neither reader selects any of them. **Nor does it carry
    /// `armed`**, which the store derives at write time and nothing on the mount path reads, and
    /// which is outside the ruling: `WireDirectoryAccount`'s doc carries why.
    /// `WireDirectoryVenue` and `WireDirectoryAccount` are the whole of that contract — a field
    /// added to either is a new disclosure to the observe key, which wants the owner's ruling and
    /// not a review comment. `crates/vike-tradehub/tests/daemon/directory.rs`'s
    /// `the_directory_carries_no_credential_value_and_no_key_name` holds the line over the raw
    /// bytes of the frame, and `the_directory_reply_carries_exactly_these_fields_and_never_armed`
    /// pins the exact field set.
    ///
    /// ⚠ **An account table this node cannot read is an ERROR, never an empty list.** A project
    /// with no database (its credential store still a FILE, or none at all) and a database older
    /// than the `account` table both answer `vike_secrets::Accounts::Unanswerable`. Sent as an
    /// empty `accounts`, that reads as *this node has no accounts* while the snapshot may be
    /// running some, so each answers `Response::Error` — as `AccountAdminSource::list` refuses
    /// with its reason — and the caller falls back to the snapshot. A database that exists and
    /// will not read is the same error. The read creates nothing.
    ///
    /// ⚠ **No error here carries a filesystem path.** `DbError`'s and `NoAccountTable`'s `Display`
    /// name the database and the credential file by absolute path, so each reason is this
    /// function's own sentence, and what the store said goes to the log
    /// ([`SettingsShowSource::directory_unreadable`]).
    ///
    /// ⚠ **A successful reply can still carry accounts and no venues.**
    /// `vike_secrets::read_venues_in` answers an empty list for a database with no `venue` table,
    /// while `vike_secrets::resolve_accounts_in` answers `Known` for any store at schema 2 or
    /// above. A caller must fall back to an account's OWN venue key when its venue has no row —
    /// and to the key when the row has no title.
    pub(super) fn directory(&self) -> Response {
        let Some(dir) = self.settings_dir.as_deref() else {
            return Response::Error(
                "directory unavailable: this node resolved no settings directory at boot".into(),
            );
        };
        let venues = match vike_secrets::read_venues_in(dir) {
            Ok(rows) => rows,
            Err(e) => return Self::directory_unreadable(&e),
        };
        let accounts = match vike_secrets::resolve_accounts_in(dir) {
            Ok(vike_secrets::Accounts::Known(rows)) => rows,
            Ok(vike_secrets::Accounts::Unanswerable(why)) => {
                let reason = match why {
                    vike_secrets::NoAccountTable::NoStore { .. } => {
                        "no settings database on this node"
                    }
                    vike_secrets::NoAccountTable::OlderSchema { .. } => {
                        "this node's settings database predates the account table"
                    }
                };
                return Response::Error(format!("directory unavailable: {reason}"));
            }
            Err(e) => return Self::directory_unreadable(&e),
        };
        Response::Directory(Box::new(WireDirectory {
            venues: venues
                .into_iter()
                // The node's own roster, the same `VENUES` the account-add arm validates a venue
                // against: a row the roster has dropped is one this node cannot mount.
                .filter(|v| vike_model::VENUES.contains(&v.name.as_str()))
                .map(|v| WireDirectoryVenue { name: v.name, title: v.title })
                .collect(),
            accounts: accounts
                .into_iter()
                .filter(|a| a.active)
                .map(|a| WireDirectoryAccount {
                    id: a.id,
                    venue: a.venue,
                    label: a.label,
                    tier: a.tier,
                    venue_account_id: a.venue_account_id,
                })
                .collect(),
        }))
    }

    /// The reply for a settings database that exists and will not read: this node's own sentence
    /// for the peer, and the store's own words — which name the database file by absolute path —
    /// in the log, where the operator is the reader.
    fn directory_unreadable(error: &dyn std::fmt::Display) -> Response {
        tracing::warn!(%error, "directory: the settings database could not be read");
        Response::Error("directory unavailable: the settings database could not be read".into())
    }

    /// Lower ONE accepted `WireCommand::SetSetting` into the settings DATABASE (0086 point 6, the
    /// owner's 2026-09-26 REVERSAL of the file-era refusal): hand the key and value straight to
    /// `vike_config::write_setting_row`, the SAME row-native planner `vike-cli config set` calls,
    /// inside this process, using the `settings/db` write access this daemon's own unit already
    /// grants (the 2026-09-18 ruling). `Err(reason)` is the wire refusal ([`Response::Error`], via
    /// `AcceptError::Refused`); `Ok` carries the audit raw material.
    ///
    /// ⚠ **This said "UNREACHABLE on the shipped deployment" and named a typed-confirm ceremony,
    /// both of which are GONE.** `docs/decisions/0068`'s "the daemon writes no settings row" is
    /// REVERSED by 0086 point 6 (*"that is a bullshit rule"*) — the GUI, MCP and a CLI run from the
    /// operator's own PC must be able to change a live limit without an SSH session on the server —
    /// and 0086 point 7 deletes the retype-confirm ceremony for every key, everywhere: *"confirmation
    /// over confirmation … a nightmare"*. `file` and `confirm` are never consulted here —
    /// `write_setting_row` derives the section from `key` alone and refuses no differently for
    /// them — and they leave the wire in two steps: since step 1 a command may OMIT both (the wire
    /// type decodes them as absent, `crates/vike-tradehub/tests/daemon/settings_write.rs`'s
    /// `a_settings_write_with_no_file_and_no_confirm_lands`), and step 2 deletes them once a
    /// release carrying that tolerance is deployed (the released v0.1.35 daemon still requires
    /// `file`, so every client keeps sending it until then). The daemon's OTHER settings paths are
    /// unchanged: this write still opens no path beside `settings/db`, and every value still passes
    /// the loader's own bounds check twice (`vike_config::write::validate_row_write`) before a byte
    /// commits. Before either, the planner refuses a credential-shaped key without opening the
    /// database (`vike_config::refuse_credential_key`), in the same words `vike-cli config set`
    /// prints: a credential never travels as a settings row from this channel either (decision
    /// 0036).
    ///
    /// ## Restart vs hot-apply (v2)
    ///
    /// The returned `restart_required` is decided by [`crate::hot_reload::classify`] — the
    /// per-key HOT-vs-RESTART table (policy is NEVER hot: it is refused into `Restart` before the
    /// table is consulted, the sealed-policy doctrine), resolved from `key` alone now that there is
    /// no wire `file` to derive it from. A HOT key with the seam wired (`self.hot`) enqueues the
    /// apply for the daemon's summary tick and waits, bounded by
    /// [`crate::hot_reload::HOT_APPLY_DEADLINE`], for its verdict: **`false` is returned ONLY
    /// when the tick confirmed the apply executed.** Everything else — a restart-class key, a
    /// hot key on a daemon with no seam wired, an apply failure, a deadline — answers `true`,
    /// which is always the safe direction (the row is committed; the next boot loads it). This
    /// is the one spot that decides it, beside the write, exactly as the v1 doc promised.
    pub(super) fn apply_set_setting(
        &self,
        key: &str,
        value: &str,
    ) -> Result<(vike_config::write::RowReport, bool), String> {
        let Some(dir) = self.settings_dir.as_deref() else {
            return Err(
                "settings write unavailable: this node resolved no settings directory at boot \
                 (no project above its working directory)"
                    .to_string(),
            );
        };
        // The row lock's wait budget stays [`SETTINGS_LOCK_BUDGET`]'s own argued number (a peer
        // holding a socket, with a reply deadline above it and a retry below it) — only the TYPE
        // changed, from the settings-FILE lock's `LockBudget` to a plain `Duration` the database
        // primitive takes directly.
        let busy = std::time::Duration::from_millis(SETTINGS_LOCK_BUDGET.max_wait_ms());
        let write =
            vike_config::write_setting_row(dir, key, value, busy).map_err(|e| e.to_string())?;
        let restart_required = match vike_config::SettingsFile::of_key(key)
            .map(|file| crate::hot_reload::classify(file, key))
        {
            Some(crate::hot_reload::HotClass::Hot { .. }) => match &self.hot {
                Some(seam) => !seam.request_apply(key, crate::hot_reload::HOT_APPLY_DEADLINE),
                None => true,
            },
            // `None` is unreachable here — the write above already resolved this exact key's
            // section — and is folded into the conservative, always-safe answer rather than
            // unwrapped: a boot-time RESTART default is the direction that never mis-reports a live
            // value as applied.
            Some(crate::hot_reload::HotClass::Restart) | None => true,
        };
        Ok((write, restart_required))
    }
}
