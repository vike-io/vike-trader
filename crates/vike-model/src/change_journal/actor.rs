//! Who made a change and how it went: `Outcome`, `Actor`, and the writing process, `Proc`.

use serde::Serialize;

use super::clean_ident;

/// What happened to the change being recorded.
///
/// `Refused` is a first-class outcome rather than an omission: "somebody tried to raise the ceiling
/// and was refused" is exactly as much of an audit fact as a successful write, and a journal that
/// records only successes cannot answer whether anyone tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The change is in effect now.
    Applied,
    /// The change is on disk, and the RUNNING process keeps its boot-time value until restarted —
    /// `vike_tradehub::server::control::Accepted`'s `SettingsWritten { restart_required: true }`.
    AppliedPendingRestart,
    /// The change was refused and nothing was written.
    Refused,
    /// **The change did NOT take effect, and something on disk changed anyway** — the state is
    /// neither the old one nor the intended one, and the `reason` cell says what to put back.
    ///
    /// ⚠ It exists because [`Outcome::Refused`] was being used for it, and `Refused`'s own
    /// definition — *"the change was refused and nothing was written"* — was then the opposite of
    /// what had happened. A ledger row a reader BRANCHES on may not say the opposite of the event
    /// it records; that is the failure this whole ledger exists to remove, and a writer reproducing
    /// it in its own error path is worse than a gap.
    ///
    /// Today's one producer is `vike_config`'s `SettingsWriteError::Stranded` (the settings write
    /// whose replacement could not be landed after the previous file had been moved aside, so the
    /// target is ABSENT and the operator's bytes are at `<file>.toml.bak`), reached through
    /// `vike_config::journal_outcome`. It is spelled generally because the SHAPE is not
    /// settings-specific: any two-step landing has this third answer, and the ledger's vocabulary
    /// should not have to grow a variant per writer.
    Stranded,
}

/// WHO made the change — the truth, which is not a person.
///
/// ⚠ **There are no human accounts in this system.** The daemon authenticates a KEY, not a user;
/// the GUI is whoever is at the machine; the CLI is whoever ran it. Recording a `user` field would
/// be recording a fiction, and an audit record that invents an actor is worse than one that admits
/// it cannot name one. So this enum records the CHANNEL and whatever that channel actually knows.
///
/// Internally tagged on `origin`, so a line reads `{"origin":"wire","peer":"…","scope":"control"}`
/// and a reader can branch on one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum Actor {
    /// A remote peer on the control socket.
    Wire {
        /// The TCP peer address. `None` because `TcpStream::peer_addr` can fail — the whole
        /// tradehub server module carries it as an `Option` for that reason.
        #[serde(skip_serializing_if = "Option::is_none")]
        peer: Option<String>,
        /// The authenticated scope (`"control"`). `vike_tradehub_client::proto::Scope`'s name.
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// A stable, NON-SECRET identifier for the key that authenticated —
        /// ⚠ **never the key itself, and never a prefix of it.**
        ///
        /// Supplied by `vike_node_proto::auth`'s `NodeKeys::key_id`: `nk-` plus 16 hex
        /// characters of an HMAC-SHA256 tag taken under a domain separator that is deliberately
        /// NOT one of the two protocol signing domains, so a value recorded here can never be
        /// replayed as an auth tag. That module is the authority for the construction and for its
        /// one declared residual.
        ///
        /// ⚠ Still `Option`, and the absence is load-bearing in two ways: a control surface that
        /// authenticates no key at all (the Telegram channel) records NO field rather than a
        /// borrowed or placeholder id, and a key that is not configured is never fingerprinted —
        /// an id for a credential nobody set would be a lie in an append-only ledger.
        #[serde(skip_serializing_if = "Option::is_none")]
        key_id: Option<String>,
    },
    /// The desktop GUI (`vike-app`).
    Gui,
    /// A command-line invocation.
    Cli {
        /// The binary that ran (`"vike-cli"`).
        bin: String,
    },
    /// The VENUE changed it — an OAuth grant the venue rotated, not something a human did.
    Venue {
        /// The venue id, as `crate::venues::VENUES` spells it.
        venue: String,
    },
    /// Process startup, recording what the effective values ARE rather than a change to them.
    Boot,
}

impl Actor {
    /// A control-socket peer. Every cell is capped and control-stripped, because `peer` and `scope`
    /// are remote-influenced text landing in a structured line.
    pub fn wire(peer: Option<&str>, scope: Option<&str>, key_id: Option<&str>) -> Self {
        Actor::Wire {
            peer: peer.map(clean_ident),
            scope: scope.map(clean_ident),
            key_id: key_id.map(clean_ident),
        }
    }

    /// A command-line invocation by `bin`.
    pub fn cli(bin: &str) -> Self {
        Actor::Cli { bin: clean_ident(bin) }
    }

    /// The venue itself rotated something.
    pub fn venue(venue: &str) -> Self {
        Actor::Venue { venue: clean_ident(venue) }
    }
}

/// The PROCESS that wrote the record — `{"bin":"vike-tradehub","pid":4711,"ver":"0.1.0"}`.
///
/// It is on every record rather than on the file, because one file is written by several processes:
/// the daemon and the GUI append to the same month, and "which binary wrote this" is the first
/// question an incident review asks of a line it did not expect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proc {
    /// The executable's file stem (`"vike-tradehub"`, `.exe` already stripped on Windows).
    pub bin: String,
    /// The OS process id — what ties a record to a log line and to a core file.
    pub pid: u32,
    /// The build version, as the caller states it (`env!("CARGO_PKG_VERSION")`, or
    /// `vike_buildinfo::summary`'s richer line).
    pub ver: String,
}

impl Proc {
    /// State the process identity outright — the form a caller that already knows it should use.
    pub fn new(bin: &str, pid: u32, ver: &str) -> Self {
        Self { bin: clean_ident(bin), pid, ver: clean_ident(ver) }
    }

    /// Read the identity off the running process.
    ///
    /// ⚠ This is the one call in the module that touches ambient process state, and it is exempt
    /// from the module doc's purity rule for a reason that does not generalize: **the ambient state
    /// IS the record.** A caller-supplied "which binary am I" would be a caller-supplied claim, and
    /// a record whose most forgeable field is the one identifying the writer is not worth writing.
    /// `current_exe` is not an environment read (`crates/vike-ops/tests/settings/settings_registry.rs`'s
    /// scanner keys on `env::var`) and not a clock read
    /// (`crates/vike-ops/tests/hygiene/clock_pin.rs`'s `AMBIENT_CLOCK_READERS`), so neither ratchet is
    /// touched.
    ///
    /// An unreadable `current_exe` yields `"unknown"` rather than failing: a journal that refuses
    /// to record because it could not name itself is a journal that goes silent exactly when the
    /// process is in trouble.
    pub fn current(ver: &str) -> Self {
        let bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "unknown".to_string());
        Self::new(&bin, std::process::id(), ver)
    }
}
