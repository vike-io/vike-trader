//! `account_confirmation` — **what a venue's own successful handshake said about the account it
//! authenticated as**, parked in `<project>/settings/state` until something that may write the
//! settings database folds it in.
//!
//! # The gap this closes
//!
//! `account.venue_account_id` records *somebody told the store*, never *the venue confirmed it*,
//! and `account.last_verified_at` had no writer anywhere in the tree —
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 names both as owed and §12
//! carries them. The measured incident behind them: a store held two dukascopy accounts that were
//! not in the owner's cabinet at all, and because nothing recorded when a credential last
//! authenticated, *never verified* and *verified three weeks ago* looked identical to *fine*.
//!
//! ⚠ **THE PAST TENSE IS LOAD-BEARING: `last_verified_at` HAS a writer, since 2026-09-15.** This
//! sentence said *has no writer anywhere in the tree*, present tense, and it is the sentence that
//! made the column a drop candidate — a later audit re-measured it rather than trusting it, which
//! is the only reason a live column was not dropped as dead. Stated by SYMBOL so the next audit
//! has something to check instead of the same silence:
//!
//! * **WRITTEN** by `crates/vike-secrets/src/db.rs`'s `set_venue_account_id`, under
//!   `BookSource::Handshake` and under that source ONLY — including the arm where the book is
//!   UNCHANGED, because a venue confirming what the row already says is exactly what this column
//!   is for;
//! * **READ** by that file's `account_columns` on every account read, so it is on the `Account`
//!   every caller receives;
//! * **RENDERED** by `vike-cli secrets accounts` (`crates/vike-cli/src/cmd/secrets.rs`), which
//!   prints `(never)` for a NULL — the distinction the incident above is about.
//!
//! What remains true is the sentence's PURPOSE: this module exists because the mount that performs
//! the handshake may not open the store, so the confirmation is parked rather than written. The
//! writer is at the other end of that park, not absent.
//!
//! Every venue adapter already performs an authenticated handshake at mount and that handshake
//! already carries the account identifier — dukascopy's JForex sidecar sends it outright
//! (`crates/bridges/dukascopy/src/proto.rs`'s `Envelope::Ready` `account` field, filled from
//! `IAccount.getAccountId()`). **Nothing new is asked of any venue.** What was missing is the write
//! path, and the reason it is a PARK rather than a write is the next section.
//!
//! # ⚠ THE CONSTRAINT THAT DECIDES THE SHAPE — a LIBRARY may not open the credential store
//!
//! `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` is a ratchet over exactly
//! one defect: a library reading or writing the credential store at a location its caller can
//! neither see nor override. `vike-mount` is where that defect was caught before — it opened the
//! store from a library file, at a directory taken from a process global
//! (`crates/vike-mount/src/account_directory.rs`'s module doc carries the incident). A mount that
//! performed `UPDATE account …` itself would be that same defect wearing a feature's clothes.
//!
//! So the mount RECORDS and something else FOLDS, and the something else is a BINARY an operator
//! ran:
//!
//! ```text
//! mount (a LIBRARY)         -> <project>/settings/state/account-confirmations.json   [may write]
//! vike-cli secrets confirm  -> <project>/settings/db/vike.db `account` row           [may write]
//! ```
//!
//! The record is durable, bounded (one entry per account, newest wins) and carries no secret. The
//! fold is an operator act with a verb of its own, exactly like `vike-cli secrets migrate`.
//!
//! ⚠ **This section named a DIFFERENT constraint until 2026-09-19, and that one has since been
//! lifted.** It read: *the deployed daemon cannot write the database* — `ProtectSystem=strict` with
//! `ReadWritePaths=<project>/settings/state`, putting `<project>/settings/db/vike.db` outside the
//! only writable path, so a daemon-side write would fail `EROFS`. **Re-measured against the running
//! the CI box daemon on 2026-09-19, that is no longer true**: the unit now grants
//! `ReadWritePaths=…/settings/state …/settings/db`, and `/proc/<MainPID>/mountinfo` shows
//! `…/settings/db … rw` INSIDE the daemon's own mount namespace — which is the half that decides,
//! since the unit says what was requested and the namespace says what is in force.
//!
//! The park survives that intact, because the sandbox was never the only reason — but a reader who
//! checked the old reason, found it lifted, and concluded the mount may now write the row would be
//! walking into `CREDENTIAL_STORE_PIN`. Hence the reordering: the rule above is the one that holds,
//! and the sandbox is history. (The settings ROOT is still read-only, so
//! `vike_app_core`'s `WireCommand::SetSetting` — which writes `settings/policy.toml` — still hits
//! the wall this paragraph used to describe. Two paths, two answers.)
//!
//! # ⚠ The address is never the row id
//!
//! `account.id` is a SQLite rowid, stable only for the life of ONE database FILE — ⚠ this sentence
//! read *"with no `AUTOINCREMENT`"* until stage 4 of the settings-store plane added one, and the
//! conclusion is untouched by that, because the high-water mark is a row of the very file the
//! recovery deletes: this tree documents a recovery that deletes the database and migrates again,
//! after which the same accounts come back numbered differently. A parked record outlives that, so
//! keying it on `id` would let a fold stamp a stranger's row.
//!
//! [`ConfirmationRecord::key_prefix`] is the address instead — the store's OWN account identity
//! (`crates/vike-secrets/src/schema.rs`'s `Classification::owner_prefix`: *an account's owner prefix
//! is recoverable from any one of its own rows*), and the same fact
//! `crates/vike-mount/src/dukascopy.rs` already keys its broker mapping on. The row id the mount saw
//! rides along as [`ConfirmationRecord::observed_row`] — EVIDENCE, so a fold can say the numbering
//! moved, never an address.
//!
//! ## ⚠ …and a LABELLED account has no key prefix at all, so it needs the SECOND address
//!
//! `crates/vike-secrets/src/db.rs`'s `read_account_keys` derives a prefix by stripping a
//! credential's `field` from the END of its NAME. For a labelled key the field is not a suffix —
//! the `__LABEL` sits after it — so **a labelled account's row carries an EMPTY prefix list**, and a
//! record addressed only by prefix could match no row for one. `Classification::owner_prefix` states
//! that `None` outright and calls it survivable, which it is for the MIGRATION (a labelled account
//! is unique by `(venue, tier, label)`, so the resolver's second lookup answers) and is NOT for a
//! parked record, which carries no such tuple.
//!
//! Dukascopy never met this, and could not: its two accounts are both UNLABELLED — the very case
//! prefix addressing exists for. Hyperliquid met it the day its mount began parking, and until this
//! shipped a labelled account's confirmation was simply not built.
//!
//! So a record carries EITHER address and the fold uses whichever is present:
//!
//! | the account | the address | why that one |
//! |---|---|---|
//! | UNLABELLED | [`ConfirmationRecord::key_prefix`] | it is the ONLY thing separating two rows that share `(venue, tier, label)` — dukascopy's pair |
//! | LABELLED | [`ConfirmationRecord::tier`] + [`ConfirmationRecord::label`] | the store's own `UNIQUE (venue, tier, label)`, which BITES exactly when a label exists |
//!
//! ⚠ **The tier is carried only alongside a label, and that is not tidiness.** The pair is the
//! store's unique index and `(venue, label)` alone is not — `(hyperliquid, demo, ALT)` and
//! `(hyperliquid, live, ALT)` are two accounts. It is left ABSENT for an unlabelled account
//! deliberately: the prefix already encodes the tier (`HYPERLIQUID_LIVE_`), and adding it would
//! change that record's [`ConfirmationRecord::address`] so a record written before this shipped and
//! one written after would be TWO entries for one account rather than newest-wins.
//!
//! # What is in a record, and why none of it is a secret
//!
//! A venue account id is the number the venue prints on its own page and echoes on its own wire; a
//! key PREFIX is a credential key's name with its field suffix removed; a row id is an opaque
//! integer. There is no field here that can hold a login or a password, and
//! [`ConfirmationRecord`]'s `Debug` is the derived one for that reason — there is nothing to redact.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The parked-confirmation file inside [`crate::state_path::STATE_SUBDIR`]:
/// `<project>/settings/state/account-confirmations.json`.
///
/// State in the sense `crate::state_path`'s own doc means — machine-written, no human edits it, and
/// deleting it loses only confirmations nothing has folded yet. It sits beside `alerts.json` and
/// `pace.json` rather than under `logs/` because a human occasionally opens it, and beside them
/// rather than in `<project>/tmp` because it is not re-derivable: the session that produced it is
/// over.
pub const CONFIRMATIONS_FILE: &str = "account-confirmations.json";

/// **One venue handshake, recorded** — what the venue answered, and what the store said at the time.
///
/// Written by a mount that authenticated successfully; read by whatever folds it into the `account`
/// table. Nothing here is a credential — see the module doc's last section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationRecord {
    /// The venue id, as `vike_model::VENUES` spells it (`"dukascopy"`).
    pub venue: String,
    /// **The ADDRESS of an UNLABELLED account** — its credential-key OWNER PREFIX
    /// (`"DUKASCOPY_DEMO1_"`), which is the store's own account identity and survives a
    /// re-migration. See the module doc.
    ///
    /// ⚠ **EMPTY when [`ConfirmationRecord::label`] is set**, and that is the shape rather than a
    /// missing value: a labelled account's row carries no prefix at all (`read_account_keys` strips
    /// a `field` from the end of a NAME, and `__LABEL` sits after it), so there is nothing to put
    /// here and the label pair below is the address instead. A reader must therefore check `label`
    /// FIRST, never treat an empty prefix as *the store forgot one*.
    pub key_prefix: String,
    /// **The ADDRESS of a LABELLED account**, half one — the operator's label, exactly as
    /// `account.label` holds it. `None` — the ordinary answer — means the account is unlabelled and
    /// [`ConfirmationRecord::key_prefix`] addresses it.
    ///
    /// ⚠ Absent from the JSON when `None` (`skip_serializing_if`), so a record parked before this
    /// field existed reads back as an unlabelled one, which is exactly what it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// **The ADDRESS of a LABELLED account**, half two — one of `vike_secrets::schema`'s
    /// `ACCOUNT_TIERS`, as `account.tier` holds it.
    ///
    /// ⚠ **Raw text, and a file parked before the 2026-09-23 `sim` -> `paper` rename holds the OLD
    /// word.** This crate is layer 10 and cannot name `ACCOUNT_TIERS` (layer 15), let alone
    /// normalize against it, so the spelling is carried as written and normalized AT THE JOIN by
    /// the one consumer that resolves a record to a row — `vike-cli`'s `secrets confirm`, through
    /// `vike_secrets::account_tier_named`. Nothing rewrites this file, so the old spelling stays on
    /// disk indefinitely and the normalization is permanent rather than a migration window.
    ///
    /// Carried ONLY beside [`ConfirmationRecord::label`], never on its own: the pair is the store's
    /// `UNIQUE (venue, tier, label)` index, and `(venue, label)` alone is not unique —
    /// `(hyperliquid, demo, ALT)` and `(hyperliquid, live, ALT)` are two accounts. The module doc
    /// argues why it stays absent for an unlabelled account even though the information exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// **What the venue answered** — the account identifier its authenticated handshake carried.
    ///
    /// ⚠ Its FORM is a venue's own business and this type asserts nothing about it. For dukascopy
    /// it is whatever `IAccount.getAccountId()` returns, which the one frame this tree pins is
    /// login-shaped (`"DEMO2cGyrc"`) while the numbers an operator reads off Dukascopy's own page
    /// are numeric — an unsettled question §9 of the credential-schema spec states outright and
    /// which [`Verdict::Disagrees`] is careful not to pretend it has resolved.
    pub handshake_account_id: String,
    /// The `account.id` the mount resolved, as EVIDENCE rather than as an address. `None` when the
    /// mount could not be answered by a store at all (a `Backend::Files` box, an older schema, a
    /// process that declared no project).
    pub observed_row: Option<i64>,
    /// `account.venue_account_id` as the mount read it, so a fold can see whether the row moved
    /// between the handshake and the fold. `None` is *not yet known*.
    pub observed_book: Option<String>,
    /// When the handshake succeeded, epoch-ms UTC.
    ///
    /// ⚠ **This — not the fold's own clock — is what `account.last_verified_at` is stamped with.**
    /// The column answers *when did this credential last authenticate*, not *when did somebody run
    /// the CLI*, and a fold two days later that stamped its own `now` would answer the second
    /// question while looking like the first.
    pub at_ms: i64,
}

impl ConfirmationRecord {
    /// The tuple that identifies this record for replacement: one entry per addressed account,
    /// newest wins. See [`merge`].
    ///
    /// ⚠ It carries BOTH address shapes rather than picking one, because a record is keyed by
    /// whichever it actually holds and the two can never collide: a labelled record's `key_prefix`
    /// is empty and an unlabelled one's `label`/`tier` are `None`. Keying on `(venue, key_prefix)`
    /// alone — which is what this returned until labels were addressable — would collapse EVERY
    /// labelled account of one venue onto the single key `(venue, "")`, so a second labelled
    /// account's confirmation would silently evict the first's.
    pub fn address(&self) -> (&str, &str, Option<&str>, Option<&str>) {
        (self.venue.as_str(), self.key_prefix.as_str(), self.tier.as_deref(), self.label.as_deref())
    }
}

/// **What the stored book and the venue's answer say about each other.** Computed by
/// [`verdict`]; the disposition of each arm is argued there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The row names no book yet and the venue has just named one. The fold WRITES it and stamps
    /// `last_verified_at`.
    Learns,
    /// The row already names exactly what the venue answered. The fold writes no book and stamps
    /// `last_verified_at` — which is the whole point of having a column that is not the book.
    Confirms,
    /// ⚠ **THE WRONG-BROKER ALARM.** The row names one book and the venue answered a different one.
    ///
    /// The fold writes NOTHING — not the book, and not the timestamp either — and reports. Both
    /// halves are argued at [`verdict`].
    Disagrees {
        /// What the row says, so a report can name both strings without re-reading the store.
        stored: String,
    },
}

/// **The verdict** — pure, over the two strings.
///
/// # Why a disagreement is REPORTED rather than folded, and rather than thrown
///
/// * **It is not folded.** Overwriting a stored book with the handshake's would re-point an armed
///   account at another broker in silence, which is the failure `vike_secrets::
///   set_venue_account_id`'s `BookAlreadyKnown` refusal exists for; that refusal takes an explicit
///   `--replace` from an operator, and a background fold has nobody to ask.
/// * **It is not thrown.** The credential-schema spec's §8 rules this case directly for the
///   constraint's twin: *a violation discovered AT A HANDSHAKE is REPORTED, not thrown* — failing
///   the write would be a session that worked and cannot record what it learned, which is
///   `docs/decisions/0013-degrade-vs-refuse.md`'s stranding shape wearing a constraint's clothes.
///   The mount has already authenticated; taking the venue away over a bookkeeping disagreement
///   would degrade a capability that was working.
/// * **The TIMESTAMP goes with it.** `last_verified_at` is a claim about a ROW, not about a
///   credential in the abstract, and a row the venue has just contradicted is the single row that
///   must not read *verified today* in a listing. Stamping it would be a NEW way for a wrong row to
///   look fine — the exact failure §1 of the spec is about, which this whole path exists to remove.
///   So a disagreeing record folds nothing and STAYS parked until an operator resolves it.
///
/// # ⚠ What this cannot tell apart, declared rather than assumed away
///
/// A disagreement is not proof of a wrong broker. The credential-schema spec §9 leaves the FORM of
/// dukascopy's handshake identifier explicitly unsettled — the sidecar sends
/// `IAccount.getAccountId()`, the one frame this tree pins is login-shaped, and the books an
/// operator wrote by hand off Dukascopy's own page are numeric — so a numeric stored book against a
/// login-shaped handshake id lands here as a disagreement while being a FORM mismatch. This function
/// compares two strings and says so; classifying the shapes would be a heuristic over a value nobody
/// has measured, and guessing wrong in the permissive direction is the failure the alarm exists to
/// catch. The cure is one command, once: see [`Verdict::Disagrees`]'s consumers.
pub fn verdict(handshake_account_id: &str, stored_book: Option<&str>) -> Verdict {
    match stored_book {
        None => Verdict::Learns,
        Some(stored) if stored == handshake_account_id => Verdict::Confirms,
        Some(stored) => Verdict::Disagrees { stored: stored.to_string() },
    }
}

/// The file's on-disk shape. An OBJECT rather than a bare array, so a later field (a schema tag, a
/// second kind of parked record) is an additive change instead of a format break.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Parked {
    #[serde(default)]
    records: Vec<ConfirmationRecord>,
}

/// Replace the entry addressing the same `(venue, key_prefix)` and keep every other, sorted by
/// address so the file is stable to diff.
///
/// NEWEST WINS, and it is a replace rather than an append: the question the fold asks is *what did
/// this account's venue last answer*, and a growing log of identical answers would make the file
/// unbounded in the one process that runs forever.
fn merge(existing: Vec<ConfirmationRecord>, fresh: ConfirmationRecord) -> Vec<ConfirmationRecord> {
    // ⚠ The key is [`ConfirmationRecord::address`]'s OWNED twin, and it must stay the same tuple:
    // keying here on `(venue, key_prefix)` while sorting there on four fields would let a file be
    // stable to diff and still hold two entries for one account.
    let key = |r: &ConfirmationRecord| {
        (r.venue.clone(), r.key_prefix.clone(), r.tier.clone(), r.label.clone())
    };
    let mut by_address: BTreeMap<_, ConfirmationRecord> =
        existing.into_iter().map(|r| (key(&r), r)).collect();
    by_address.insert(key(&fresh), fresh);
    by_address.into_values().collect()
}

/// Every parked record, or an EMPTY list when there is no file.
///
/// ⚠ An absent file is an ANSWER (nothing has been confirmed yet) and a MALFORMED one is an error,
/// for the reason `vike_secrets::resolve` splits those two cases: a parse failure that read as
/// *nothing parked* would hide a fold that is silently doing nothing.
pub fn read(state_dir: Option<&Path>) -> std::io::Result<Vec<ConfirmationRecord>> {
    let Some(dir) = state_dir else { return Ok(Vec::new()) };
    let path = dir.join(CONFIRMATIONS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let parked: Parked = serde_json::from_str(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a readable confirmation file: {e}", path.display()),
        )
    })?;
    Ok(parked.records)
}

/// Park one confirmation, replacing any earlier one for the same account. Returns the file written.
///
/// Read-modify-write through `crate::state_path::write_path`, which creates the state directory
/// lazily and refuses a symlinked TARGET. The write is atomic (temp file beside it, then rename)
/// for the reason every other state writer is: a mount that is killed mid-write must not leave a
/// truncated file that then reads as *malformed* forever.
pub fn park(state_dir: Option<&Path>, fresh: ConfirmationRecord) -> std::io::Result<PathBuf> {
    let path = crate::state_path::write_path(state_dir, CONFIRMATIONS_FILE)?;
    // A malformed existing file is NOT a reason to lose this confirmation, and it is not a reason to
    // silently discard whatever it held either — `read` has already refused to parse it, so the
    // caller was told; here the choice is between dropping the new record and starting a fresh file.
    // Starting fresh keeps the fold working, and the parked records are a cache of something the
    // venues will say again at the next mount.
    let existing = read(state_dir).unwrap_or_default();
    write_all(&path, merge(existing, fresh))?;
    Ok(path)
}

/// Replace the whole parked set — what a fold calls once it has consumed some of it.
///
/// Separate from [`park`] because a fold REMOVES entries and a mount only ever adds one, and a
/// single read-modify-write door shared by both would let a fold racing a mount drop a confirmation
/// that had just arrived. They still race (this is a file, not a transaction); what the split buys
/// is that the loser is one mount's record, which the next mount re-parks.
pub fn replace_all(
    state_dir: Option<&Path>,
    records: Vec<ConfirmationRecord>,
) -> std::io::Result<PathBuf> {
    let path = crate::state_path::write_path(state_dir, CONFIRMATIONS_FILE)?;
    write_all(&path, records)?;
    Ok(path)
}

fn write_all(path: &Path, mut records: Vec<ConfirmationRecord>) -> std::io::Result<()> {
    records.sort_by(|a, b| a.address().cmp(&b.address()));
    let body = serde_json::to_string_pretty(&Parked { records })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Atomic: same directory (so the rename cannot cross a filesystem), then replace. ⚠ `rename`
    // FAILS on Windows when the destination exists, unlike POSIX, so the destination is removed
    // first — the same shape every other atomic writer in this workspace uses.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{body}\n"))?;
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(venue: &str, prefix: &str, id: &str, book: Option<&str>) -> ConfirmationRecord {
        ConfirmationRecord {
            venue: venue.to_string(),
            key_prefix: prefix.to_string(),
            label: None,
            tier: None,
            handshake_account_id: id.to_string(),
            observed_row: Some(7),
            observed_book: book.map(str::to_string),
            at_ms: 1_787_356_800_000,
        }
    }

    /// The LABELLED twin of [`record`] — an account with no credential-key prefix, addressed by the
    /// store's `UNIQUE (venue, tier, label)` instead.
    fn labelled(venue: &str, tier: &str, label: &str, id: &str) -> ConfirmationRecord {
        ConfirmationRecord {
            key_prefix: String::new(),
            label: Some(label.to_string()),
            tier: Some(tier.to_string()),
            ..record(venue, "", id, None)
        }
    }

    /// **TWO LABELLED ACCOUNTS OF ONE VENUE ARE TWO ENTRIES**, which is the whole reason
    /// [`ConfirmationRecord::address`] carries more than `(venue, key_prefix)`.
    ///
    /// Both carry an EMPTY prefix by construction, so the old two-field key collapsed them onto
    /// `(venue, "")` and the second confirmation silently evicted the first — one account's venue
    /// answer filed against nothing, with no error and no way to notice.
    #[test]
    fn two_labelled_accounts_do_not_evict_each_other() {
        let a = labelled("hyperliquid", "live", "ALT", "0xaaa");
        let b = labelled("hyperliquid", "live", "SPREAD", "0xbbb");
        let merged = merge(merge(Vec::new(), a.clone()), b.clone());
        assert_eq!(merged.len(), 2, "two accounts, two records: {merged:?}");
        assert!(merged.contains(&a) && merged.contains(&b));
    }

    /// …and the SAME labelled account twice is still ONE entry, newest wins — the property the
    /// merge exists for, held across the wider key.
    #[test]
    fn one_labelled_account_answering_twice_is_one_entry() {
        let first = labelled("hyperliquid", "live", "ALT", "0xaaa");
        let again = ConfirmationRecord {
            handshake_account_id: "0xbbb".to_string(),
            ..labelled("hyperliquid", "live", "ALT", "0xaaa")
        };
        let merged = merge(merge(Vec::new(), first), again.clone());
        assert_eq!(merged, vec![again], "newest wins");
    }

    /// **THE SAME LABEL AT TWO TIERS IS TWO ACCOUNTS**, which is why the tier rides along with the
    /// label and `(venue, label)` alone would not do. `UNIQUE (venue, tier, label)` is the store's
    /// index and this key mirrors it.
    #[test]
    fn one_label_at_two_tiers_is_two_entries() {
        let demo = labelled("hyperliquid", "demo", "ALT", "0xdemo");
        let live = labelled("hyperliquid", "live", "ALT", "0xlive");
        assert_eq!(merge(merge(Vec::new(), demo), live).len(), 2);
    }

    /// **A RECORD PARKED BEFORE LABELS WERE ADDRESSABLE STILL READS**, as the unlabelled account it
    /// was — the property `#[serde(default)]` buys, and the one that decides whether a box with an
    /// existing `account-confirmations.json` keeps its dukascopy evidence across an upgrade.
    ///
    /// ⚠ And the new fields are ABSENT from the JSON rather than `null`, so a record written by this
    /// build and one written by the previous build for the SAME unlabelled account are byte-identical
    /// — which is what keeps them one merge entry instead of two.
    #[test]
    fn a_record_written_before_labels_existed_still_reads_and_round_trips() {
        let old = r#"{"venue":"dukascopy","key_prefix":"DUKASCOPY_DEMO1_",
            "handshake_account_id":"DEMO2cGyrc","observed_row":7,
            "observed_book":"3709890","at_ms":1787356800000}"#;
        let parsed: ConfirmationRecord = serde_json::from_str(old).expect("an older record reads");
        assert_eq!(parsed.key_prefix, "DUKASCOPY_DEMO1_");
        assert_eq!(parsed.label, None, "no label ⇒ the prefix addresses it, exactly as before");
        assert_eq!(parsed.tier, None);

        let written = serde_json::to_string(&parsed).expect("serializes");
        assert!(!written.contains("label"), "an unlabelled record writes NO label key: {written}");
        assert!(!written.contains("tier"), "…and no tier key either: {written}");
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "vike-confirm-{tag}-{}-{}",
                std::process::id(),
                crate::clock::now_ms()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The three verdicts, and the one that matters: a DIFFERENT book is neither a learn nor a
    /// confirm, whatever the two strings look like.
    #[test]
    fn a_different_book_is_a_disagreement() {
        assert_eq!(verdict("3709890", None), Verdict::Learns);
        assert_eq!(verdict("3709890", Some("3709890")), Verdict::Confirms);
        assert_eq!(
            verdict("DEMO2cGyrc", Some("3716974")),
            Verdict::Disagrees { stored: "3716974".to_string() }
        );
    }

    /// ⚠ The FORM residual, pinned as a test rather than only as prose: a login-shaped handshake id
    /// against a numeric stored book is reported as a disagreement, because this function compares
    /// two strings and the spec (§9) has not settled which form dukascopy's handshake carries.
    /// Classifying the shapes would be a guess over a value nobody has measured.
    #[test]
    fn a_form_mismatch_is_reported_as_a_disagreement_rather_than_classified() {
        let v = verdict("DEMO1abcd", Some("3709890"));
        assert!(matches!(v, Verdict::Disagrees { .. }), "no shape heuristic lives here: {v:?}");
    }

    /// One entry per account: a second handshake for the same key prefix REPLACES the first rather
    /// than growing the file, and a different account's entry is untouched.
    #[test]
    fn a_second_handshake_replaces_its_own_entry_and_no_other() {
        let s = Scratch::new("merge");
        let dir = Some(s.0.as_path());
        park(dir, record("dukascopy", "DUKASCOPY_DEMO1_", "A", None)).expect("park 1");
        park(dir, record("dukascopy", "DUKASCOPY_DEMO2_", "B", None)).expect("park 2");
        park(dir, record("dukascopy", "DUKASCOPY_DEMO1_", "A2", Some("A"))).expect("park 3");
        let got = read(dir).expect("read");
        assert_eq!(got.len(), 2, "one entry per (venue, key_prefix): {got:?}");
        let demo1 = got.iter().find(|r| r.key_prefix == "DUKASCOPY_DEMO1_").expect("demo1");
        assert_eq!(demo1.handshake_account_id, "A2", "newest wins");
        let demo2 = got.iter().find(|r| r.key_prefix == "DUKASCOPY_DEMO2_").expect("demo2");
        assert_eq!(demo2.handshake_account_id, "B", "the other account is untouched");
    }

    /// An ABSENT file is an answer (nothing parked), never an error — the ordinary state of every
    /// box that has not mounted a confirming venue.
    #[test]
    fn an_absent_file_reads_as_nothing_parked() {
        let s = Scratch::new("absent");
        assert!(read(Some(s.0.as_path())).expect("absent is an answer").is_empty());
        // …and so is having no state directory at all, which is a process that declared no project.
        assert!(read(None).expect("no state dir is an answer").is_empty());
    }

    /// A MALFORMED file is an ERROR rather than "nothing parked": a parse failure that read as an
    /// empty set would make a fold that is silently doing nothing look like a fold with nothing to
    /// do.
    #[test]
    fn a_malformed_file_is_an_error_rather_than_an_empty_set() {
        let s = Scratch::new("malformed");
        std::fs::write(s.0.join(CONFIRMATIONS_FILE), b"{ not json").expect("plant");
        let err = read(Some(s.0.as_path())).expect_err("malformed is loud");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// A fold consumes entries through [`replace_all`], and what it leaves is what a later read
    /// sees.
    #[test]
    fn replace_all_is_what_a_fold_leaves_behind() {
        let s = Scratch::new("replace");
        let dir = Some(s.0.as_path());
        park(dir, record("dukascopy", "DUKASCOPY_DEMO1_", "A", None)).expect("park 1");
        park(dir, record("dukascopy", "DUKASCOPY_DEMO2_", "B", None)).expect("park 2");
        let keep: Vec<ConfirmationRecord> = read(dir)
            .expect("read")
            .into_iter()
            .filter(|r| r.key_prefix.ends_with("DEMO2_"))
            .collect();
        replace_all(dir, keep).expect("replace");
        let got = read(dir).expect("read back");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key_prefix, "DUKASCOPY_DEMO2_");
    }

    /// Nothing in a record can hold a credential: the `Debug` is derived precisely because there is
    /// nothing to redact, and this test is what makes that claim checkable if a field is ever added.
    #[test]
    fn a_record_carries_no_field_a_credential_could_land_in() {
        let r = record("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd", Some("3709890"));
        let json = serde_json::to_value(&r).expect("serializes");
        // ⚠ **SORTED, and that is not tidiness — an order-sensitive assertion here FAILS IN ONE
        // BUILD AND PASSES IN ANOTHER.** `serde_json::Map` is a `BTreeMap` (sorted keys) by default
        // and an `IndexMap` (declaration order) under its `preserve_order` feature, and features are
        // UNIFIED across a build: `cargo test -p vike-model` got sorted keys while the workspace
        // nextest lane got declaration order, because something else in that graph turns the feature
        // on. Measured, both directions, on the same commit. What this test is about is the SET of
        // fields, so it sorts and the serializer's own business stays the serializer's.
        let mut fields: Vec<&str> =
            json.as_object().expect("object").keys().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            vec![
                "at_ms",
                "handshake_account_id",
                "key_prefix",
                "observed_book",
                "observed_row",
                "venue"
            ],
            "a NEW field here needs an argument for why a credential cannot reach it"
        );
    }
}
