//! The REST SEAM: what the server asks of the venue half of the daemon for every verb that is not a
//! live stream.
//!
//! `crate::server` owns the framing, the scopes, the caps and the paging; a `RestLane` owns the venue
//! calls. A fetch (klines, funding, the dukascopy tick range, a catalog list or search, the option
//! contract index) comes back from the lane as ONE value, the whole answer the in-process seam would
//! have returned; the server splits it with `vike_gatewayhub_client::proto::FetchRows::into_pages`,
//! writes the pages and closes with the totals the client verifies its reassembly against. The lane
//! never sees a frame ceiling and the server never sees a venue.
//!
//! # What a lane promises
//!
//! * **`facts` is a BUILD fact**, like `crate::data::DataLane::venues`: which venues each lane serves,
//!   fixed at construction, never a venue's liveness. `Welcome.features` is computed from it once
//!   (`crate::server::features::served_features`), and the server refuses a verb the facts do not
//!   serve BEFORE it reaches the lane, naming the feature string. A lane therefore never has to answer
//!   a venue it does not hold.
//! * **Errors keep their variant and their words.** A fetch failure is a `FetchFailure` (the in-process
//!   `SourceError`: `Fetch` for a venue that was asked and failed, `Refused` for a request the source
//!   cannot express), and every other verb's failure is a `String` that reaches the peer unchanged.
//!   ⚠ **A failure is `Err`, never an empty answer**: an empty `Vec` is stored as "the venue had no
//!   rows", and an empty family resolve unsubscribes every live stream.
//! * **A fetch is cooperative.** `FetchCtx::is_cancelled` turns true when the client abandons the
//!   fetch or its connection dies, and the lane stops at its next venue page or hour-file boundary; a
//!   lane that never looks still finishes, and the server then discards the rows. `FetchCtx::set_state`
//!   lets a lane that waits on a venue budget say `Queued`, so a stall detector does not read waiting
//!   as dead.
//! * **State that lives for a connection lives in the `ConnectionScope`** the call carries and nowhere
//!   else: the read-once credential memo of the OANDA history lane, the planner behind a rolling
//!   family. It is dropped when the connection closes, so a lane keeps no per-client map of its own.
//! * **No credential value leaves the lane.** `credential_presence` answers a word, never a value.
//!
//! Wave 2's trade surface is not a mode of this seam.
//!
//! # The real lane (feature `data`)
//!
//! `lane::VenueRest` implements the seam over the venue bridges, one submodule per kind of call:
//! `kline_sources` (the kline and funding registries, MOVED from the datahub's backfill table),
//! `ticks` (the dukascopy `.bi5` range, an hour file at a time), `oanda` (the one practice
//! token, read once per CONNECTION), `catalog` (the eight public providers), `options` (ONE
//! persistent Deribit provider and its two feeds) and `family` (the polymarket rolling family, one
//! planner per `(connection, family)`). The seam types below build in every configuration; the real
//! lane links the bridges and so exists only with the feature.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::proto::{
    BudgetCounter, FetchFailure, FetchRows, FetchState, FetchWhat, OptionsCall, OptionsFeedSpec,
    OptionsReply, WireCatalogMode,
};
use vike_marketdata::OptionTicker;
use vike_model::AssetClass;

use crate::session::ConnectionScope;

/// Which venues each REST lane serves, as the lane declares them. Every list is ordered (the order is
/// the advertisement's) and every entry is a `vike_model::VENUES` slug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestFacts {
    /// `Fetch { Klines }` on the keyless lane.
    pub klines: Vec<String>,
    /// `Fetch { Funding }`.
    pub funding: Vec<String>,
    /// `Fetch { Ticks }`: the dukascopy tick range.
    pub ticks: Vec<String>,
    /// `CatalogDescribe` and the catalog lanes of `Fetch`.
    pub catalog: Vec<String>,
    /// `FetchCredentialed { Klines }`: the venues whose credential this daemon holds and spends.
    pub credentialed: Vec<String>,
    /// The Deribit options surface: `OptionsRest`, the `OptionIndex` fetch and both feeds.
    pub options: bool,
    /// `ResolveFamily`: the venues with a rotating recorder family.
    pub family: Vec<String>,
    /// `CredentialPresence`.
    pub presence: bool,
}

impl RestFacts {
    /// Whether any lane of the paged fetch family is served, which is what arms the progress and
    /// cancel contract.
    pub fn serves_any_fetch(&self) -> bool {
        !self.klines.is_empty()
            || !self.funding.is_empty()
            || !self.ticks.is_empty()
            || !self.catalog.is_empty()
            || !self.credentialed.is_empty()
            || self.options
    }
}

/// What a catalog venue is, as `CatalogDescribe` answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTraits {
    /// The provider's `asset_classes()`.
    pub asset_classes: Vec<AssetClass>,
    /// The provider's `mode()`.
    pub mode: WireCatalogMode,
}

/// A running fetch's handle on the server: cancellation in, state out. Cheap to clone; the lane gets a
/// reference and may hand clones to the threads it fans out to.
#[derive(Debug, Clone)]
pub struct FetchCtx {
    cancel: Arc<AtomicBool>,
    state: Arc<AtomicU8>,
}

const STATE_QUEUED: u8 = 0;
const STATE_RUNNING: u8 = 1;

impl Default for FetchCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl FetchCtx {
    /// A context that is running and not cancelled.
    pub fn new() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            state: Arc::new(AtomicU8::new(STATE_RUNNING)),
        }
    }

    /// Whether the fetch should stop: the client sent `FetchCancel`, its connection died, or the
    /// daemon is stopping. Once true it stays true.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Say whether the fetch is waiting (a venue budget, a lock) or working. The next progress frame
    /// reports it.
    pub fn set_state(&self, state: FetchState) {
        let v = match state {
            FetchState::Queued => STATE_QUEUED,
            FetchState::Running => STATE_RUNNING,
        };
        self.state.store(v, Ordering::SeqCst);
    }

    /// The state the progress frames report.
    pub fn state(&self) -> FetchState {
        if self.state.load(Ordering::SeqCst) == STATE_QUEUED {
            FetchState::Queued
        } else {
            FetchState::Running
        }
    }

    /// Raise the cancel flag. The server's call, not the lane's.
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Where an options feed's rows go. Each call is cheap and never blocks, like a `LiveDataSink`.
pub trait OptionsFeedSink: Send + Sync {
    /// One decoded `markprice.options` batch (mark and implied vol only).
    fn marks(&self, rows: Vec<OptionTicker>);
    /// One decoded `ticker` row.
    fn ticker(&self, row: OptionTicker);
}

/// A running options feed, owned by the server for the life of the socket that opened it.
pub trait OptionsFeedHandle: Send {
    /// Stop the feed and join its threads. Idempotent.
    fn shutdown(&mut self);
}

/// The venue half of every non-stream verb. See the module doc.
pub trait RestLane: Send + Sync {
    /// Which venues each lane serves. A build fact, independent of any venue's liveness.
    fn facts(&self) -> RestFacts;

    /// One fetch (`credentialed` is `FetchCredentialed`, a `cd_venue=` klines fetch): the whole answer,
    /// of the row kind `what` names (`Klines` -> `Bars`, `Funding` -> `Funding`, `Ticks` -> `Quotes`,
    /// the catalog and option-index lanes -> `Instruments`). The server has already refused a lane or a
    /// venue `facts` does not serve.
    fn fetch(
        &self,
        scope: &ConnectionScope,
        what: &FetchWhat,
        credentialed: bool,
        ctx: &FetchCtx,
    ) -> Result<FetchRows, FetchFailure>;

    /// One catalog venue's traits.
    fn catalog_describe(
        &self,
        scope: &ConnectionScope,
        venue: &str,
    ) -> Result<CatalogTraits, String>;

    /// One call to the persistent options provider.
    fn options(&self, scope: &ConnectionScope, call: &OptionsCall) -> Result<OptionsReply, String>;

    /// Start one options feed delivering into `sink`. The server has already refused an empty,
    /// over-long or blank focus set (`OptionsFeedSpec::refusal`).
    fn open_options_feed(
        &self,
        scope: &ConnectionScope,
        spec: &OptionsFeedSpec,
        sink: Arc<dyn OptionsFeedSink>,
    ) -> Result<Box<dyn OptionsFeedHandle>, String>;

    /// The symbols a rotating family should be streaming at `now_ms`. `Err` means UNKNOWN, never
    /// empty; a legitimately empty set is `Ok(empty)`.
    fn resolve_family(
        &self,
        scope: &ConnectionScope,
        venue: &str,
        family: &str,
        now_ms: i64,
    ) -> Result<Vec<String>, String>;

    /// Whether the credential `venue`'s credentialed lane reads is stored. Total: a word, never a
    /// value and never an error.
    fn credential_presence(&self, venue: &str) -> CredentialPresence;

    /// The budget counters the status report carries (wave 1: shadow, counted and disclosed, nothing
    /// refused). A lane with no budget returns none.
    fn budget_counters(&self) -> Vec<BudgetCounter> {
        Vec::new()
    }
}

/// The failure a lane returns when a fetch was cancelled before it asked a venue. The server answers
/// `FetchCancelled` whatever the lane returns once the context says cancelled, so the words are for a
/// log line and a test; they are never a venue's.
#[cfg(feature = "data")]
pub(crate) fn cancelled(stage: &str) -> FetchFailure {
    FetchFailure::Fetch(format!("gatewayhub fetch cancelled {stage}"))
}

#[cfg(feature = "data")]
pub mod catalog;
#[cfg(feature = "data")]
pub mod family;
#[cfg(feature = "data")]
pub mod kline_sources;
#[cfg(feature = "data")]
pub mod lane;
#[cfg(feature = "data")]
pub mod oanda;
#[cfg(feature = "data")]
pub mod options;
#[cfg(feature = "data")]
pub mod ticks;
