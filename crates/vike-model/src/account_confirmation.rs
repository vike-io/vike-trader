//! `account_confirmation` — **what a venue's own successful handshake said about the account it
//! authenticated as**, parked in `<project>/settings/state` until something that may write the
//! settings database folds it in.
//!
//! # The gap this closes
//!
//! `account.venue_account_id` records *somebody told the store*, never *the venue confirmed it*,
//! and `account.last_verified_at` has no writer anywhere in the tree —
//! `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4.5 names both as owed and §12
//! carries them. The measured incident behind them: a store held two dukascopy accounts that were
//! not in the owner's cabinet at all, and because nothing recorded when a credential last
//! authenticated, *never verified* and *verified three weeks ago* looked identical to *fine*.
//!
//! Every venue adapter already performs an authenticated handshake at mount and that handshake
//! already carries the account identifier — dukascopy's JForex sidecar sends it outright
//! (`crates/bridges/dukascopy/src/proto.rs`'s `Envelope::Ready` `account` field, filled from
//! `IAccount.getAccountId()`). **Nothing new is asked of any venue.** What was missing is the write
//! path, and the reason it is a PARK rather than a write is the next section.
//!
//! # ⚠ THE CONSTRAINT THAT DECIDES THE SHAPE — the deployed daemon cannot write the database
//!
//! MEASURED on the the CI box deployment, whose unit is `deploy/vike-tradehub.service` with that box's
//! own root substituted in: it runs under
//! `ProtectSystem=strict` with `ReadWritePaths=<project>/settings/state`. The settings database is
//! at `<project>/settings/db/vike.db` — **outside the only writable path** — so a daemon-side
//! `UPDATE account …` fails `EROFS`. It is the same wall `vike_app_core`'s `WireCommand::SetSetting`
//! already hits. A design in which the mount writes the row is therefore dead on every shipped
//! deployment, and widening the unit is a decision the owner has not taken.
//!
//! So the mount RECORDS and something else FOLDS:
//!
//! ```text
//! mount (daemon, sandboxed)   -> <project>/settings/state/account-confirmations.json   [writable]
//! vike-cli secrets confirm    -> <project>/settings/db/vike.db `account` row           [not sandboxed]
//! ```
//!
//! The record is durable, bounded (one entry per account, newest wins) and carries no secret. The
//! fold is an operator act with a verb of its own, exactly like `vike-cli secrets migrate`.
//!
//! # ⚠ The address is the KEY PREFIX, never the row id
//!
//! `account.id` is a SQLite rowid with no `AUTOINCREMENT`, stable only for the life of ONE database
//! FILE: this tree documents a recovery that deletes the database and migrates again, after which
//! the same accounts come back numbered differently. A parked record outlives that, so keying it on
//! `id` would let a fold stamp a stranger's row.
//!
//! [`ConfirmationRecord::key_prefix`] is the address instead — the store's OWN account identity
//! (`crates/vike-secrets/src/schema.rs`'s `Classification::owner_prefix`: *an account's owner prefix
//! is recoverable from any one of its own rows*), and the same fact
//! `crates/vike-mount/src/dukascopy.rs` already keys its broker mapping on. The row id the mount saw
//! rides along as [`ConfirmationRecord::observed_row`] — EVIDENCE, so a fold can say the numbering
//! moved, never an address.
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
    /// **The ADDRESS** — the account's credential-key OWNER PREFIX (`"DUKASCOPY_DEMO1_"`), which is
    /// the store's own account identity and survives a re-migration. See the module doc.
    pub key_prefix: String,
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
    /// The pair that identifies this record for replacement: one entry per `(venue, key_prefix)`,
    /// newest wins. See [`merge`].
    pub fn address(&self) -> (&str, &str) {
        (self.venue.as_str(), self.key_prefix.as_str())
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
    let mut by_address: BTreeMap<(String, String), ConfirmationRecord> =
        existing.into_iter().map(|r| ((r.venue.clone(), r.key_prefix.clone()), r)).collect();
    by_address.insert((fresh.venue.clone(), fresh.key_prefix.clone()), fresh);
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
            handshake_account_id: id.to_string(),
            observed_row: Some(7),
            observed_book: book.map(str::to_string),
            at_ms: 1_787_356_800_000,
        }
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
