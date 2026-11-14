//! **What a startup preflight probe of a venue answers**: the outcome of a venue clock read
//! ([`ServerTimeGap`]), of a credential read ([`CredentialGap`]) and of the network probe
//! ([`NetReading`]), as plain values with no bridge type behind them.
//!
//! ONE definition for both daemons: the gatewayhub's probes answer in these
//! (`crates/vike-gatewayhub/src/trade/adapters/probe.rs` maps them onto the lease wire's `ServerTimeGapWire`,
//! `CredentialGapWire` and its own `NetReading`), and the tradehub's go/no-go report is written against
//! them (`crates/vike-tradehub-core/src/preflight.rs`'s module doc carries the four outcomes of the
//! clock leg, ① a reading and ②-④ below, and the "refused versus silent" split of the credential
//! leg, which is what lets only a REFUSAL stop a venue). No serde: the wire spells its own shapes.

/// Why a clock leg produced no number (outcomes ②-④, one variant each): different facts, never one
/// row. Declared variants take `&'static str`, so a runtime failure cannot fake a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTimeGap {
    /// ② The venue PUBLISHES a clock and this attempt got none. Carries the venue's error text,
    /// never a URL or a credential. Also an UNWIRED leg's answer (the tradehub's `NO_PROBE`).
    Unreachable(String),
    /// ③ DECLARED, nothing at stake: no clock leg, with the row's own reason (the venue's
    /// `ClockDeclaration::NotWired`); a drifted clock could not cost this venue an order.
    NotChecked(&'static str),
    /// ④ DECLARED, ORDERS at stake: no clock leg, but the venue's auth binds the clock into the
    /// order path — an unmeasured HAZARD, rendered a WARN, never NOT-APPLICABLE (polymarket printed
    /// "nothing to check here" over the roster's one order-affecting gap).
    UnmeasuredRisk {
        /// Why there is no leg (the row's own sentence).
        reason: &'static str,
        /// What a drifted clock costs AT THIS VENUE, in that venue's own terms.
        at_stake: &'static str,
    },
}

/// Why a credential probe failed — the twin of [`ServerTimeGap`] that makes the report's venue
/// disposition enforceable: only "refused us" may FAIL; "never heard back" never does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialGap {
    /// The venue ANSWERED inside the bound and refused (or a definite transport error): a FAIL
    /// stopping the venue's accounts. Venue error text, never a key or secret. Also an UNWIRED leg's
    /// answer (the tradehub's `NO_PROBE`): our wiring defect, and strict is safe.
    Rejected(String),
    /// No answer inside the probe's bound (`CREDENTIAL_PROBE_TIMEOUT`), or the leg's budget ran out
    /// first. NOT evidence about the credentials: a WARN that NEVER demotes.
    Unanswered {
        /// How long the probing thread actually waited, in ms (the row states its own bound).
        waited_ms: u64,
        /// What the wait was on, in operator language. Never contains a secret.
        detail: String,
    },
}

impl From<String> for CredentialGap {
    /// A bare error string is the CONFIRMED half; only the bounded runner (which owns the clock the
    /// wait is measured on) constructs [`CredentialGap::Unanswered`].
    fn from(e: String) -> Self {
        CredentialGap::Rejected(e)
    }
}

/// The network leg's reading of a probe handle: three states and their numbers, so the report names
/// no bridge type. The report's wording is unchanged by the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetReading {
    /// No round has completed: the handle's `internet_up` is still the optimistic initial value, which
    /// is not a measurement.
    Unknown,
    /// The last round observed the internet up, after `rounds` completed rounds.
    Alive {
        /// Completed probe rounds; `None` = the daemon said alive without a count (one that predates it), which
        /// the report prints as not said, never as a number nobody measured.
        rounds: Option<u64>,
    },
    /// The last round observed the internet down.
    Dead {
        /// Epoch ms the flag last went down.
        since_ms: i64,
    },
}
