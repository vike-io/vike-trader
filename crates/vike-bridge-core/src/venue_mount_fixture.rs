//! The test doubles for the venue mount contract, in three parts. `test-support`-gated like
//! `scripted`, so no shipped build compiles them.
//!
//! * [`MountFixture`] — owned inputs a test borrows `MountInputs` / `MountRequest` from.
//! * [`captured`] — the ONE `tracing` capture double a bridge's mount tests read the lines a mount
//!   logged with (a bridge crate links no `tracing-subscriber`), and [`found_tier_events`], the
//!   diagnostics among them that carry `found_tier`.
//! * [`PlantedMount`] and [`PLANTED_DECLARATION`] — the ONE planted contract row, and the ONE
//!   baseline declaration every planted venue in the tree starts from. A test that plants a venue
//!   (`vike-mount`'s contract, node, clock and startup tests; this crate's own object-safety pin)
//!   overrides the single fact it is about — `VenueDeclaration { addresses_accounts: true,
//!   ..PLANTED_DECLARATION }` — so a field added to [`VenueDeclaration`] is given its test default
//!   HERE, once, rather than in a full struct literal per planted venue. A planted venue whose
//!   `resolve`/`mount` carry state of their own (counters, a log, a scripted phase) is still its
//!   own type, and still builds its declaration from [`PLANTED_DECLARATION`].

use std::collections::HashMap;
use std::time::Duration;

use vike_exec::EventSender;
use vike_model::HaltAdmit;
use vike_model::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;

use crate::account_directory::AccountDirectory;
use crate::venue_mount::{
    BookIdentity, ClockDecl, CredentialProbe, DeclaredGridSource, MountInputs, MountOutcome,
    MountRequest, ProcessExclusive, ProcessFacts, Resolution, VenueDeclaration, VenueMount,
};

/// Owned inputs. Fields are public: a test sets `account` for a labelled account or `accounts` for
/// a read store.
pub struct MountFixture {
    pub vars: HashMap<String, String>,
    pub account: AccountLabel,
    pub settings: VenueSettings,
    pub accounts: AccountDirectory,
    pub process: ProcessFacts,
}

impl MountFixture {
    /// A fixture over `pairs`, for the default account, with an unread account table.
    #[must_use]
    pub fn new(pairs: &[(&str, &str)]) -> Self {
        MountFixture {
            vars: pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
            account: AccountLabel::Default,
            settings: VenueSettings::default(),
            accounts: AccountDirectory::unread(),
            process: ProcessFacts::default(),
        }
    }

    #[must_use]
    pub fn inputs(&self, live_permitted: bool) -> MountInputs<'_> {
        MountInputs {
            account: &self.account,
            secrets: &self.vars,
            settings: &self.settings,
            live_permitted,
            accounts: &self.accounts,
            process: &self.process,
        }
    }

    /// A request with reconciliation off, no trigger, no recorder, no profile and the default
    /// halt-admit mode — the offline shape every paper-agreement test uses.
    #[must_use]
    pub fn request<'a>(
        &'a self,
        live_permitted: bool,
        symbol: &'a str,
        events: &'a EventSender,
    ) -> MountRequest<'a> {
        MountRequest {
            inputs: self.inputs(live_permitted),
            symbol,
            declared_legs: &[],
            events,
            recon_enabled: false,
            recon_trigger: None,
            properties_rec: None,
            risk_profile: None,
            market_slippage: None,
            halt_admit: HaltAdmit::default(),
        }
    }
}

/// What a planted venue declares unless a test says otherwise: no named accounts, no exclusive
/// resource, no reconnect trigger, no grid to fetch, no book the store could name and no clock to
/// read — nothing a test that is about something else would look at.
///
/// Override the one fact a test is about with struct-update syntax, in a `const`, a `static` or a
/// method body alike: `VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }`. Do
/// NOT respell the other five fields; that is the copy this constant replaced, twenty times over.
pub const PLANTED_DECLARATION: VenueDeclaration = VenueDeclaration {
    addresses_accounts: false,
    process_exclusive: None,
    takes_recon_trigger: false,
    grid_source: DeclaredGridSource::NoGrid,
    book_identity: BookIdentity::Undeterminable { why: "a planted venue names no book" },
    clock: ClockDecl::NotWired { reason: "a planted venue reads no clock", unmeasured_risk: None },
};

/// A claim on `resource` whose refusals are plain text, for a planted row that only needs to BE
/// exclusive. A planted row that needs to OBSERVE its refusal (a log of every `held_by_another`
/// rendered, say) builds its own `ProcessExclusive`; the function pointers cannot capture.
#[must_use]
pub fn planted_exclusive(resource: &'static str) -> ProcessExclusive {
    ProcessExclusive {
        resource,
        held_by_another: |label, holder| format!("{label} yields to {holder}"),
        already_claimed: |label| format!("{label}: already claimed"),
    }
}

/// A planted row's wired clock read: [`VenueMount::server_time_ms`] without `&self` and the timeout.
pub type PlantedClockRead = fn(&MountInputs<'_>) -> Result<i64, String>;

/// A planted row's startup probe: [`VenueMount::credential_probe`] without `&self`.
pub type PlantedProbeOffer = fn(&MountInputs<'_>) -> Option<CredentialProbe>;

/// **The planted contract row**: names `venue`, declares `declaration`, resolves to `resolution`
/// whatever its inputs say, and mounts PAPER. The two optional legs default to the trait's own
/// answers — no clock leg (`Err("no clock leg")`) and no startup probe (`None`) — and a test that
/// plants a venue with one of them sets `server_time` / `credential_probe` to a function pointer
/// (a non-capturing closure coerces; a `static` row that needs a counter names its own static from
/// inside the closure).
///
/// Every field is public and `new` is `const`, so a row is `const`-constructible and overridden by
/// struct update — `PlantedMount { declaration: …, ..PlantedMount::new("planted", resolution) }`
/// — which is what lets it sit in a `static` registry (`&'static dyn VenueMount`).
pub struct PlantedMount {
    pub venue: &'static str,
    pub declaration: VenueDeclaration,
    pub resolution: Resolution,
    /// The wired clock read, or `None` for the trait's default refusal.
    pub server_time: Option<PlantedClockRead>,
    /// The startup credential probe offered, or `None` for a venue that is not probed.
    pub credential_probe: Option<PlantedProbeOffer>,
}

impl PlantedMount {
    /// A row over [`PLANTED_DECLARATION`] with neither optional leg.
    #[must_use]
    pub const fn new(venue: &'static str, resolution: Resolution) -> Self {
        PlantedMount {
            venue,
            declaration: PLANTED_DECLARATION,
            resolution,
            server_time: None,
            credential_probe: None,
        }
    }
}

impl VenueMount for PlantedMount {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueDeclaration {
        self.declaration
    }
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        self.resolution
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        MountOutcome::paper()
    }
    fn server_time_ms(&self, inputs: &MountInputs<'_>, _timeout: Duration) -> Result<i64, String> {
        match self.server_time {
            Some(read) => read(inputs),
            None => Err("no clock leg".into()),
        }
    }
    fn credential_probe(&self, inputs: &MountInputs<'_>) -> Option<CredentialProbe> {
        self.credential_probe.and_then(|offer| offer(inputs))
    }
}

/// **One `tracing` event, as a test reads it back.** `fields` holds every structured field except
/// `message`, rendered the way `%value` / `?value` / a plain string render, and `message` is the
/// formatted sentence.
#[derive(Debug, Clone)]
pub struct CapturedEvent {
    pub level: tracing::Level,
    pub fields: std::collections::BTreeMap<String, String>,
    pub message: String,
}

impl CapturedEvent {
    /// A structured field's text, or `None` when the event carries no such field.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

/// The events among `events` that carry a `found_tier` field — the diagnostics a demo-pinned arm
/// says about a LIVE tier it will not use (`venue_mount::report_unused_live_tier`), as distinct
/// from the mount announcement, which carries `tier` and never `found_tier`.
#[must_use]
pub fn found_tier_events(events: &[CapturedEvent]) -> Vec<&CapturedEvent> {
    events.iter().filter(|e| e.field("found_tier").is_some()).collect()
}

/// The recording subscriber behind [`captured`]. Hand-rolled — a bridge crate links no
/// `tracing-subscriber` — and `register_callsite` answers `sometimes`, so the per-callsite
/// `Interest` cache cannot make a capture silently empty depending on test order.
#[derive(Clone, Default)]
struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<CapturedEvent>>>);

struct FieldReader<'a>(&'a mut CapturedEvent);

impl tracing::field::Visit for FieldReader<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.message = format!("{value:?}");
        } else {
            self.0.fields.insert(field.name().to_string(), format!("{value:?}"));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.message = value.to_string();
        } else {
            self.0.fields.insert(field.name().to_string(), value.to_string());
        }
    }
}

impl tracing::Subscriber for Recorder {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut seen = CapturedEvent {
            level: *event.metadata().level(),
            fields: std::collections::BTreeMap::new(),
            message: String::new(),
        };
        event.record(&mut FieldReader(&mut seen));
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(seen);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Run `f` and return every `tracing` event it emitted ON THIS THREAD, in order. A mount that
/// spawns a thread of its own logs from that thread, which this does not see — the offline
/// bridge-mount tests that use it call a synchronous seam and so never need to.
pub fn captured<R>(f: impl FnOnce() -> R) -> (R, Vec<CapturedEvent>) {
    let recorder = Recorder::default();
    let out = tracing::subscriber::with_default(recorder.clone(), f);
    let events =
        std::mem::take(&mut *recorder.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    (out, events)
}
