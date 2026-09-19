//! **The explicit, per-venue instrument-catalog refresh** — the control the
//! `vike-catalog` cache was designed for and never got.
//!
//! # What was already built, and what was missing
//!
//! `crates/vike-catalog/src/persist.rs`'s module doc has stated the intended behaviour since the
//! cache was written: *"Loaded instantly at startup; rewritten only on an explicit user refresh
//! (no auto/background re-fetch). QueryBacked venues are never cached."* and *"Per-venue refresh
//! stamp shown in the Data Manager Catalog/Sources view."*
//!
//! MEASURED before this module existed: [`vike_catalog::CatalogCache`],
//! [`vike_catalog::VenueStamp`], [`vike_catalog::save_cache`] and [`vike_catalog::load_cache`]
//! were referenced by **nothing outside their own crate**. The policy, the cache format and the
//! stamp type were all built; the thing that triggers a refresh and the surface that shows the
//! stamp were not. This module is that half, and it deliberately adds no second scheme: the cache
//! it reads and writes is that type, at that spelling.
//!
//! # ⚠ What a refresh can reach — and the answer changed, because the route did
//!
//! A `CatalogProvider` lives in its venue's BRIDGE crate, and rulings 1 and 2 of the 2026-09-09
//! rename design took the venue bridges out of the desktop's dependency graph — the tombstone in
//! `crates/vike-desktop/Cargo.toml` names the six catalog edges that went, *"all of them for the
//! Symbol picker alone"*. **This module's own doc used to conclude from that "a binary can only
//! refresh the venues it LINKS", and that stopped being true** the day
//! `vike_datahub_client::proto::Request::VenueCatalog` shipped: an `Observe` verb by which a
//! process asks the backend's datahub to list a venue whose bridge it does not carry
//! (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`).
//!
//! So there are TWO routes and [`vike_catalog::catalog_source_for`] is the one decision site:
//!
//! * `Some(CatalogSource::Direct)` — this binary links the provider. Fetch it here, ask no server.
//! * `Some(CatalogSource::ServerBacked)` — this binary does not link it, and it is publicly
//!   enumerable, so the datahub can. [`crate::catalog_wire`] is that leg.
//! * `None` — **no route exists for anybody**: the venue is credentialed (a server will not spend
//!   the operator's identity on a client's request) or it publishes no bulk list at all. The row
//!   renders the REASON and no control.
//!
//! ⚠ **That last arm is why [`RefreshAvailability`] grew from three variants to six.** It had
//! `NotInThisBuild` as the whole answer for thirteen venues, and that sentence — *"this venue's
//! catalog lives in its bridge crate, which this binary does not link"* — is now true only in the
//! narrow local-provider sense, misleading for the eight that route to a server, and the WRONG KIND
//! of fact for the five that route nowhere. [`vike_catalog::catalog_availability`] is what tells
//! those five apart, and the sentence each renders is the SERVER-side type's
//! (`vike_datahub_client::catalog::CatalogRefusal::describe`), so a credentialed venue reads the
//! same words whether it was refused here or at a daemon's door.
//!
//! [`VenueCatalogRow::source`] is where the decision lands. It was written by `rows()` and read by
//! NOBODY before this, and it was also wrong for five venues (an `is_some()` test on the local
//! provider called alpaca/oanda/ctrader/ig/ibkr `ServerBacked`, where the routing table answers
//! "no route for anybody").
//!
//! # The failure rule this module exists to enforce
//!
//! **A refresh that fails, times out or returns nothing must never replace the list it already
//! had.** The picker reads one universe; silently swapping a venue's symbols for an empty list
//! makes every one of that venue's instruments un-pickable, with nothing on screen to say why.
//! [`merge_refresh`] returns BEFORE touching the cache on both of those paths, and
//! `crates/vike-app-core/tests/catalog_refresh.rs` mutation-proves it.
//!
//! ⚠ **A REFUSAL is a third thing and must not be folded into either.** `Failed` calls a venue
//! property an error; `Empty` renders *"the venue answered with nothing"* — the exact lie 0062's
//! decision 5 exists to prevent, and a lie for five roster venues rather than two, since
//! alpaca/oanda/ctrader/ig all answer `Ok(vec![])` when credentials are absent. Hence
//! [`RefreshOutcome::Refused`].
//!
//! # The rate budget, and why it is the same number on both routes
//!
//! One press is a venue REST call, and the heavier providers are bounded crawls rather than a
//! single GET (Polymarket pages the Gamma browse; OKX fetches per `instType` and then one listing
//! per option family). So a press is refused while one is in flight, and again inside
//! [`REFRESH_COOLDOWN_MS`] of the last completed one — which makes a held-down button a no-op
//! instead of a crawl repeated as fast as the network answers.
//!
//! ⚠ On the `Direct` route that call leaves THIS box; on the `ServerBacked` route it leaves the
//! DATAHUB's, against its budget. ⚠ That used to read "which is the whole cost argument behind
//! `VIKE_DATAHUB_VENUE_CATALOG` being off by default", and `docs/decisions/0066` retired both
//! halves of that clause: the lane is ON by default now, and its decision 2 found that the switch
//! never bounded the cost at all — the per-venue buckets, the TTL and each bridge's own pager do,
//! and none of them moved. [`REFRESH_COOLDOWN_MS`] is 60 s, which is
//! `vike_datahub::catalog::CATALOG_VENUE_REFILL` exactly — so a human pressing as fast as this
//! button allows tracks that lane's refill rate and can never drain its two-token burst. The one
//! wire path that spends a token at all is a provider FAILURE (every refusal is decided before the
//! bucket); [`crate::catalog_wire`]'s module doc carries the whole gate order.
//!
//! ⚠ **[`CatalogRefresh::spawn_initial`] bypasses [`refresh_block`] and stays LOCAL-ONLY.** It is
//! the one path that fetches without a press, and routing its cold venues to a server would be a
//! startup that opens eight concurrent authenticated dials and spends eight of somebody else's
//! tokens — a poll wearing a first run. A `ServerBacked` venue is fetched only when a human asks.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use vike_catalog::{
    BaselineCatalog, BaselineVenue, Catalog, CatalogAnswer, CatalogAvailability, CatalogCache,
    CatalogMode, CatalogProvider, CatalogSource, Instrument, VenueStamp, catalog_answer,
    catalog_availability, catalog_source_for, load_cache, merge_sources, save_cache,
};
use vike_datahub_client::catalog::CatalogRefusal;

use crate::catalog_wire::{CatalogDial, CatalogFetchReport, fetch_venue_catalog};

/// How long after a COMPLETED refresh the same venue refuses another one.
///
/// A minute, and the argument is the budget rather than the UI: a press spends a venue's public
/// API allowance — from the box that also signs orders on the `Direct` route, and from the
/// DATAHUB's box on the `ServerBacked` one — and the expensive providers are bounded crawls. A
/// user who presses twice because they could not tell whether the first press worked is the
/// failure this pairs with the stamp: the stamp answers the question, the cooldown makes the second
/// press harmless either way. Sixty seconds is short enough that a genuine re-check (a venue just
/// listed something) is never blocked for long.
///
/// ⚠ It is also `vike_datahub::catalog::CATALOG_VENUE_REFILL` exactly, and that coincidence is
/// load-bearing rather than decorative: it is what makes a human retrying a failed `ServerBacked`
/// fetch track the server's refill rate instead of draining its burst. See the module doc.
pub const REFRESH_COOLDOWN_MS: i64 = 60_000;

/// What the row says for a venue whose catalog is searched live, per query.
pub const QUERY_BACKED_NOTE: &str =
    "searched live per query — this venue publishes no bulk list, so there is nothing to refresh";

/// What the row says for a venue this binary links no provider for AND
/// [`vike_catalog::catalog_source_for`] can route nowhere — i.e. one it cannot classify at all.
///
/// ⚠ **This used to be the note for thirteen of the fourteen roster venues and is now the
/// fail-closed arm for a venue not on the roster.** Every classified venue reaches one of the five
/// other [`RefreshAvailability`] arms.
pub const NOT_LINKED_NOTE: &str = "not in this build — this venue's catalog lives in its bridge crate, which this binary does \
     not link, and no route to it could be resolved";

/// What the row says for a venue that routes to the backend's datahub.
pub const SERVER_BACKED_NOTE: &str =
    "listed by the connected backend's datahub — this binary links no bridge for it";

/// **What THIS BOX adds to the shared credential refusal** — whether the keys that refusal names
/// are here, and if not, what would put them here.
///
/// `docs/decisions/0066`'s decision 9 says the refusal must stop reading *"this cannot be
/// refreshed"* and start naming what would arm it. The ACT is named by the shared server-side
/// sentence, which a daemon also prints; this is the half that is a fact about the local box and is
/// therefore allowed to name a local SCREEN.
///
/// ⚠ **Never an empty list and never a bare disabled control**, which is the other half of that
/// ruling: a venue with no keys gets a sentence, a venue with keys gets a different sentence, and
/// neither gets a button — because this binary links none of those three bridges and a control that
/// silently returned `Ok(vec![])` is the failure 0062's decision 5 exists to prevent.
#[must_use]
pub fn credential_note(venue: &str, own_keys: bool) -> String {
    if own_keys {
        format!(
            "Your `{}` credentials ARE saved on this box, so that command will list it now.",
            venue.to_uppercase()
        )
    } else {
        format!(
            "No `{}` credentials are saved on this box yet — enter, save and verify them in \
             Connections first, and that command arms.",
            venue.to_uppercase()
        )
    }
}

// ------------------------------------------------------------------------------------------------
// The pure core
// ------------------------------------------------------------------------------------------------

/// What one refresh attempt DID to the cache. Every arm carries the number the operator needs to
/// tell "it worked" from "it did nothing": the new count, or the count that was KEPT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The venue handed over a non-empty list; the cache now holds `count` of its instruments.
    ///
    /// `truncated` is the wire's own flag, carried rather than dropped: a `ServerBacked` listing
    /// that hit `vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS` is SHORT, and a picker
    /// silently missing a venue's tail is a bug report nobody can reproduce. A `Direct` fetch is
    /// never truncated — no cap sits between a linked provider and this fold.
    Refreshed { count: usize, previous: usize, truncated: bool },
    /// The venue answered, with nothing in it. **The cache was not touched** — see the module doc.
    Empty { kept: usize },
    /// The fetch failed. **The cache was not touched.**
    Failed { error: String, kept: usize },
    /// **Nothing was fetched, and that is not a failure.** An unarmed lane, a server whose build
    /// carries no provider for the venue, a credentialed venue, a venue with no bulk list, or a
    /// server that does not advertise the capability at all. **The cache was not touched.**
    ///
    /// ⚠ `why` is the SERVER-side type's own sentence
    /// (`vike_datahub_client::catalog::CatalogListing::describe`), never one assembled here — so
    /// the daemon, the CLI and this row cannot describe one refusal three different ways.
    Refused { why: String, kept: usize },
}

impl RefreshOutcome {
    /// Did this outcome change the cache? Only [`Self::Refreshed`] does.
    #[must_use]
    pub fn adopted(&self) -> bool {
        matches!(self, Self::Refreshed { .. })
    }

    /// One line for the row, in the operator's terms.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Refreshed { count, previous, truncated: false } => {
                format!("refreshed — {previous} → {count} instruments")
            }
            Self::Refreshed { count, previous, truncated: true } => format!(
                "refreshed — {previous} → {count} instruments, TRUNCATED at the wire cap: the tail \
                 of this venue's universe is missing"
            ),
            Self::Empty { kept } => {
                format!("the venue answered with nothing — kept the {kept} already cached")
            }
            Self::Failed { error, kept } => {
                format!("failed — kept the {kept} already cached: {error}")
            }
            Self::Refused { why, kept: 0 } => why.clone(),
            Self::Refused { why, kept } => format!("{why} (kept the {kept} already cached)"),
        }
    }
}

/// **Fold one venue's fetch into the whole-catalog cache.** The one place a cached list is
/// replaced, and the one place the [`VenueStamp`] is written.
///
/// ⚠ **The two early returns are the safety property, not tidiness.** A `Err` and an EMPTY `Ok`
/// both return before a single instrument is removed, so a venue that refuses, times out or
/// answers with nothing keeps exactly the list it had. Moving either `return` below the `retain`
/// is what `a_failed_refresh_keeps_the_previous_cache` mutation-proves against.
///
/// Every other venue's rows are untouched in every arm: the `retain` is keyed on THIS venue.
///
/// `truncated` is the wire's flag and reaches the outcome unchanged — it is a fact about the FETCH
/// rather than about the fold, and it is a parameter here rather than a field set afterwards so
/// that no caller can adopt a short list and forget to say so.
pub fn merge_refresh(
    cache: &mut CatalogCache,
    venue: &str,
    fetched: Result<Vec<Instrument>, String>,
    now_ms: i64,
    truncated: bool,
) -> RefreshOutcome {
    let previous = cache.instruments.iter().filter(|i| i.venue == venue).count();
    let list = match fetched {
        Err(error) => return RefreshOutcome::Failed { error, kept: previous },
        Ok(list) if list.is_empty() => return RefreshOutcome::Empty { kept: previous },
        Ok(list) => list,
    };
    let count = list.len();
    cache.instruments.retain(|i| i.venue != venue);
    cache.instruments.extend(list);
    match cache.fetched.iter_mut().find(|s| s.venue == venue) {
        Some(stamp) => {
            stamp.last_refreshed_ms = now_ms;
            stamp.count = count;
        }
        None => cache.fetched.push(VenueStamp {
            venue: venue.to_string(),
            last_refreshed_ms: now_ms,
            count,
        }),
    }
    RefreshOutcome::Refreshed { count, previous, truncated }
}

/// Whether this binary can refresh a venue at all, by which route, and why not when it cannot.
///
/// ⚠ **Six variants, and the last three are the ones 0062 exists for.** Three of these are answers
/// about THIS BUILD (`Direct`, `QueryBacked`, `NotInThisBuild`); `ServerBacked` is an answer about
/// a ROUTE; `Credentialed` and `NoBulkList` are facts about the VENUE that no build and no switch
/// can change. Collapsing the last two into "not in this build" is what told an operator that five
/// venues were a packaging problem when they are not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshAvailability {
    /// Enumerable, with a provider linked into this binary: the button is live and the venue is
    /// called from THIS box.
    Direct,
    /// Publicly enumerable, no provider linked here: the button is live and the venue is called by
    /// the backend's datahub, from ITS box and against ITS budget. See [`SERVER_BACKED_NOTE`].
    ServerBacked,
    /// A LINKED provider whose [`CatalogMode`] is `QueryBacked` — there is no list to fetch. See
    /// [`QUERY_BACKED_NOTE`].
    QueryBacked,
    /// `vike_catalog::CatalogAvailability::Credentialed` — a bulk list exists and reaching it
    /// spends the OPERATOR's venue identity, which 0062's decision 3 says a server will not do on
    /// a client's request.
    ///
    /// ⚠ **This doc said "Nothing arms this" and that is no longer true.**
    /// `docs/decisions/0066`'s decision 9 armed a locally-credentialed refresh: on the box that
    /// holds this venue's keys the OPERATOR lists it themselves, with their own credentials, and
    /// nothing asks a server to spend anything. What is still true — and is the fence the record
    /// puts round that decision — is that no route from THIS binary arms, and no server switch
    /// arms in either direction.
    ///
    /// `own_keys` is whether this box holds credentials for the venue, and it changes only the
    /// SENTENCE: a venue with keys is told which act lists it, a venue without is told what would
    /// arm that act. Neither grows a control here, because this binary links none of those three
    /// bridges — `crates/vike-desktop/Cargo.toml`'s tombstone deleted them on the ground that a
    /// catalog fetch is a venue REST call, and re-linking them is a decision that record
    /// deliberately did not take.
    Credentialed { own_keys: bool },
    /// `vike_catalog::CatalogAvailability::NoBulkList` — the venue publishes no list at any price.
    /// `why` is the table's own operator-facing reason.
    NoBulkList { why: &'static str },
    /// Neither a linked provider nor a classified route — the fail-closed arm, reached by a venue
    /// off `vike_model::VENUES`. See [`NOT_LINKED_NOTE`].
    NotInThisBuild,
}

impl RefreshAvailability {
    /// Whether a press can do anything at all. The ONE predicate [`refresh_block`] gates on, so a
    /// new variant cannot silently become refreshable.
    #[must_use]
    pub fn refreshable(self) -> bool {
        matches!(self, Self::Direct | Self::ServerBacked)
    }

    /// The reason text, or `None` when the refresh is available.
    ///
    /// ⚠ **It takes the venue and returns an owned `String`**, where it used to be a bare
    /// `Option<&'static str>`. Both changes are forced by the same thing: the two refusal arms
    /// render through `vike_datahub_client::catalog::CatalogRefusal::describe`, which names the
    /// venue in its sentence and is the ONE spelling of it in the workspace. A `&'static str`
    /// could not carry the venue and a local `format!` would be a second spelling nothing compares.
    #[must_use]
    pub fn note(self, venue: &str) -> Option<String> {
        match self {
            Self::Direct => None,
            Self::ServerBacked => Some(SERVER_BACKED_NOTE.to_string()),
            Self::QueryBacked => Some(QUERY_BACKED_NOTE.to_string()),
            // ⚠ The TRUNK is the server-side sentence, unchanged and still the ONE spelling —
            // `CatalogRefusal::NeedsCredentials::describe`, which names the ACT (`vike-backend catalog
            // refresh <venue>`) and no screen, so a daemon printing it says something true.
            // What is appended is a fact about THIS BOX, and it is marked as one: whether the keys
            // that act needs are here. A sentence that named Connections inside the shared spelling
            // would put a desktop screen in a daemon's log.
            Self::Credentialed { own_keys } => {
                let shared = crate::catalog_wire::local_refusal_line(
                    venue,
                    &CatalogRefusal::NeedsCredentials,
                );
                Some(format!("{shared} {}", credential_note(venue, own_keys)))
            }
            Self::NoBulkList { why } => Some(crate::catalog_wire::local_refusal_line(
                venue,
                &CatalogRefusal::NoBulkList { why: why.to_string() },
            )),
            Self::NotInThisBuild => Some(NOT_LINKED_NOTE.to_string()),
        }
    }
}

/// Why a press was (or would be) refused. `None` from [`refresh_block`] means "go".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshBlock {
    /// [`RefreshAvailability::refreshable`] is false.
    Unavailable(RefreshAvailability),
    /// A fetch for this venue is already running.
    InFlight,
    /// Inside [`REFRESH_COOLDOWN_MS`] of the last completed attempt.
    Cooldown { remaining_ms: i64 },
    /// The venue routes to a datahub and no datahub address is resolved — no backend is connected
    /// and `config.datahub_addr` is unset. The control stays on the row, disabled: this venue CAN
    /// be refreshed, just not from a desktop with nothing to ask.
    NoBackend,
}

impl RefreshBlock {
    /// The button's hover text.
    #[must_use]
    pub fn line(self, venue: &str) -> String {
        match self {
            Self::Unavailable(a) => a.note(venue).unwrap_or_else(|| "available".to_string()),
            Self::InFlight => "already fetching this venue".to_string(),
            // ⚠ NOT "just refreshed". Every completed attempt starts this cooldown — including a
            // failure and a refusal — so wording that claims a successful fetch is a sentence an
            // operator reads while retrying the one that did not work.
            Self::Cooldown { remaining_ms } => format!(
                "this venue was asked a moment ago — it may be asked again in {}s",
                remaining_ms.div_euclid(1000) + 1
            ),
            // The SAME sentence the Status cell carries — one spelling, in `catalog_wire`.
            Self::NoBackend => crate::catalog_wire::NO_BACKEND_NOTE.to_string(),
        }
    }
}

/// Where a venue's refresh state stands right now.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum VenueRefreshState {
    /// Nothing has been attempted this session.
    #[default]
    Idle,
    /// A fetch is running; `since_ms` is when it started.
    InFlight { since_ms: i64 },
    /// An attempt finished at `at_ms` with this outcome.
    Done { at_ms: i64, outcome: RefreshOutcome },
}

/// One rendered row of the Instruments screen: everything the operator needs for one venue.
#[derive(Clone, Debug)]
pub struct VenueCatalogRow {
    pub venue: String,
    /// `None` ⇒ this binary links no provider for the venue, so its mode is unanswerable here.
    pub mode: Option<CatalogMode>,
    /// **The routing decision** — [`vike_catalog::catalog_source_for`] over this binary's linked
    /// provider set. `Some(Direct)` ⇒ fetch it here; `Some(ServerBacked)` ⇒ ask the backend's
    /// datahub; `None` ⇒ no route exists for anybody, and
    /// [`vike_catalog::catalog_availability`] says which of the two reasons applies.
    ///
    /// ⚠ It was written and read by nobody, and its old `provider.is_some()` derivation called
    /// alpaca/oanda/ctrader/ig/ibkr `ServerBacked` — a route that does not exist. It is now the
    /// field [`VenueCatalogRow::availability`] is computed from.
    pub source: Option<CatalogSource>,
    /// The resolved datahub address a `ServerBacked` press would dial, or `None` when none is
    /// resolved. Only [`CatalogSource::ServerBacked`] rows care; a `Direct` row ignores it.
    pub server: Option<String>,
    /// The cached stamp — when the list was last fetched and how many it holds. `None` ⇒ never.
    pub stamp: Option<VenueStamp>,
    /// **The SHIPPED baseline's row for this venue**, when one exists — a THIRD state beside
    /// "never fetched" and "fetched, measured N", and given its own field rather than folded into
    /// [`Self::stamp`] for the reason `vike_catalog::baseline`'s rule 2 gives: a `VenueStamp` has
    /// no provenance field, so a baseline wearing one renders as an operator's own refresh age.
    ///
    /// ⚠ **[`Self::stamp`] WINS wherever both exist**, and the row says which answered
    /// ([`Self::answer`]). Shipped data never overwrites operator data, even when the operator's
    /// own fetch measured ZERO — zero is a measured claim about a venue and nothing has measured
    /// the shipped one.
    pub baseline: Option<BaselineVenue>,
    /// **The operator's OWN CREDENTIALED list for this venue**, from `vike_catalog::LOCAL_FILE` —
    /// written by `vike-backend catalog refresh` on the box that holds the keys
    /// (`docs/decisions/0066` decision 9).
    ///
    /// Their data, so it outranks [`Self::baseline`] and is outranked by [`Self::stamp`]; the
    /// precedence is `vike_catalog::merge_sources`', spelled once so this row and the picker cannot
    /// disagree about which source is in force.
    pub local: Option<BaselineVenue>,
    /// **Whether THIS BOX holds credentials for this venue.** Changes no control and no route — it
    /// changes the SENTENCE a credentialed venue reads, from "what would arm it" to "it is armed".
    ///
    /// Set from `vike_connections::credentialed_venues` over the same credential map the
    /// Connections grid renders, so the two cannot claim different things about one venue.
    pub own_keys: bool,
    pub state: VenueRefreshState,
}

impl VenueCatalogRow {
    /// **The ROUTE decides, uniformly**, and the local provider only refines a `Direct` answer.
    ///
    /// ⚠ A venue whose bridge this binary links but which the table routes NOWHERE reads its table
    /// reason rather than its provider's mode — deliberately, and
    /// [`vike_catalog::catalog_source_for`]'s own ⚠ argues it: for a credentialed venue the honest
    /// answer depends on credentials no table can see, and a control that silently returns
    /// `Ok(vec![])` is the failure 0062's decision 5 exists to prevent.
    #[must_use]
    pub fn availability(&self) -> RefreshAvailability {
        match self.source {
            Some(CatalogSource::Direct) => match self.mode {
                Some(CatalogMode::QueryBacked) => RefreshAvailability::QueryBacked,
                _ => RefreshAvailability::Direct,
            },
            Some(CatalogSource::ServerBacked) => RefreshAvailability::ServerBacked,
            None => match catalog_availability(&self.venue) {
                CatalogAvailability::Credentialed => {
                    RefreshAvailability::Credentialed { own_keys: self.own_keys }
                }
                CatalogAvailability::NoBulkList { why } => RefreshAvailability::NoBulkList { why },
                // `PublicBulk` with no route is unreachable through `catalog_source_for` (it
                // answers `Some` for every public venue); `UnknownVenue` is the fail-closed arm.
                // Either way: no control, and a note that does not claim a venue property.
                _ => RefreshAvailability::NotInThisBuild,
            },
        }
    }

    /// **WHICH SOURCE answered for this venue** — the fact decision 7's rule 3 requires a row to be
    /// able to state, so two sources can never silently disagree about which one is on screen.
    #[must_use]
    pub fn answer(&self) -> CatalogAnswer {
        catalog_answer(self.stamp.is_some(), self.local.is_some(), self.baseline.is_some())
    }

    /// How many instruments this venue contributes to the picker — from whichever source
    /// [`Self::answer`] says answered.
    ///
    /// ⚠ `0` for [`CatalogAnswer::Nothing`] is NOT a claim about the venue, and the screen must not
    /// render it as one: `crates/vike-app-core/src/tool_views/instruments.rs` renders `—` on
    /// exactly that argument, because zero is a measured claim and nothing has measured one.
    #[must_use]
    pub fn count(&self) -> usize {
        match self.answer() {
            CatalogAnswer::OperatorFetch => self.stamp.as_ref().map_or(0, |s| s.count),
            CatalogAnswer::LocalCredentialed => {
                self.local.as_ref().map_or(0, |b| b.instruments.len())
            }
            CatalogAnswer::ShippedBaseline => {
                self.baseline.as_ref().map_or(0, |b| b.instruments.len())
            }
            CatalogAnswer::Nothing => 0,
        }
    }

    /// The [`BaselineVenue`] whichever LIST source answered, or `None` when the cache answered (or
    /// nothing did). What the screen renders a date and a qualifier from.
    #[must_use]
    pub fn answering_list(&self) -> Option<&BaselineVenue> {
        match self.answer() {
            CatalogAnswer::LocalCredentialed => self.local.as_ref(),
            CatalogAnswer::ShippedBaseline => self.baseline.as_ref(),
            _ => None,
        }
    }
}

/// **May this venue be refreshed right now?** `None` = yes. The ONE decision site: the screen
/// disables the button on it and [`CatalogRefresh::request`] refuses on it, so a second caller
/// cannot route around the budget.
///
/// The order is deliberate. Availability first, because a venue that can never be refreshed has no
/// budget question to ask; then the budget, because an in-flight or just-asked venue is refused
/// whichever route it takes; then [`RefreshBlock::NoBackend`] last, so an idle `ServerBacked` row
/// with no datahub resolved reads the sentence that names the fix.
#[must_use]
pub fn refresh_block(row: &VenueCatalogRow, now_ms: i64) -> Option<RefreshBlock> {
    let availability = row.availability();
    if !availability.refreshable() {
        return Some(RefreshBlock::Unavailable(availability));
    }
    match &row.state {
        VenueRefreshState::InFlight { .. } => return Some(RefreshBlock::InFlight),
        VenueRefreshState::Done { at_ms, .. } => {
            let elapsed = now_ms.saturating_sub(*at_ms);
            if elapsed < REFRESH_COOLDOWN_MS {
                return Some(RefreshBlock::Cooldown {
                    remaining_ms: REFRESH_COOLDOWN_MS - elapsed,
                });
            }
        }
        VenueRefreshState::Idle => {}
    }
    if availability == RefreshAvailability::ServerBacked && row.server.is_none() {
        return Some(RefreshBlock::NoBackend);
    }
    None
}

/// "never" / "just now" / "6 min ago" / "3 h ago" / "2 d ago" — the stamp in the only terms that
/// answer *did my press do anything*. Pure, so it is unit-tested rather than eyeballed.
#[must_use]
pub fn refreshed_label(stamp: Option<&VenueStamp>, now_ms: i64) -> String {
    let Some(stamp) = stamp else { return "never".to_string() };
    let secs = now_ms.saturating_sub(stamp.last_refreshed_ms).div_euclid(1000).max(0);
    match secs {
        0..=9 => "just now".to_string(),
        10..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

// ------------------------------------------------------------------------------------------------
// The handle
// ------------------------------------------------------------------------------------------------

/// The shared mutable half, behind one lock. `tx` lives in here rather than beside it because
/// `std::sync::mpsc::Sender` is `Send` but NOT `Sync`, and [`CatalogRefresh`] is shared by `&`.
struct CatalogState {
    cache: CatalogCache,
    /// **The SHIPPED baseline, held APART from the cache and never written back.**
    ///
    /// ⚠ The separation is decision 7's rule 2 and it rests on a missing field rather than on
    /// taste: [`VenueStamp`] records WHEN and HOW MANY and never FROM WHERE, so a baseline folded
    /// through [`merge_refresh`] would render through [`refreshed_label`] as an AGE —
    /// indistinguishable from the operator's own refresh — and the next `save_cache` would persist
    /// shipped data as if the operator had fetched it. Nothing in this module ever puts a
    /// [`vike_catalog::BaselineVenue`] into `cache`.
    baseline: BaselineCatalog,
    /// **The operator's OWN CREDENTIALED lists** (`vike_catalog::LOCAL_FILE`), held apart from both
    /// of the others. Written by `vike-backend catalog refresh` on the box that holds the keys; read
    /// here and never written, which is what keeps that file's single-writer property true.
    local: BaselineCatalog,
    per_venue: BTreeMap<String, VenueRefreshState>,
    /// Where a newly merged [`Catalog`] is published — the SAME channel the picker already drains.
    tx: Sender<Catalog>,
}

impl CatalogState {
    /// **The picker's universe**: the operator's own lists, plus the shipped baseline for every
    /// venue the operator has not measured. The ONE place the two sources are combined, so a
    /// caller cannot publish one without the other — see [`vike_catalog::merge_baseline`] for why
    /// the key is the cache's STAMP and not its row count.
    fn universe(&self) -> Catalog {
        Catalog::from_instruments(merge_sources(&self.cache, &self.local, &self.baseline))
    }
}

/// What one completed fetch handed back, before the fold sees it — the seam that lets the LOCAL
/// and the WIRE routes share [`settle`] byte for byte, so there is one place the ordering, the
/// publish and the persist live.
enum Fetched {
    /// A list (or the error that replaced it), plus the wire's truncation flag.
    List { instruments: Result<Vec<Instrument>, String>, truncated: bool },
    /// A successful answer that fetched nothing, carrying the server-side sentence.
    Refused { why: String },
}

/// **The catalog's owner in a GUI binary**: the on-disk cache, the per-venue refresh state, the
/// route to the backend's datahub, and the one channel the symbol picker reads from.
///
/// Shared by `&`: every method takes `&self`, the mutable halves sit behind locks, and the
/// worker threads hold `Arc` clones of the lock and of the provider — never of `self` — so the
/// handle has no lifetime entanglement with a spawned fetch.
pub struct CatalogRefresh {
    /// The providers THIS BINARY links. A constructor parameter, never a registry: a crate down
    /// here cannot know what the binary above it linked.
    providers: Vec<Arc<dyn CatalogProvider>>,
    /// `<project>/settings/state/catalog.json`, or `None` for an in-memory-only session (a process
    /// whose boot walk found no project). `None` degrades to today's behaviour exactly: nothing is
    /// loaded at start, so every Enumerable venue is fetched once.
    cache_path: Option<PathBuf>,
    /// `<project>/bin/catalog/venue-baseline.json` — the SHIPPED baseline artifact, or `None` for
    /// a process that resolved no project.
    ///
    /// ⚠ **A different directory from `cache_path`, and the split is the artifact's whole
    /// distribution argument.** The cache lives under `state/` because the program writes it; the
    /// baseline lives under `vike_model::state_path::PROJECT_BIN_DIR` because it ARRIVES — a
    /// release asset `scripts/fetch_release_tools.sh` installs, never a committed file a deployed
    /// binary could find in a source tree it does not have. A missing file DEGRADES and never
    /// refuses (`crates/bridges/dukascopy/src/config.rs`'s `bridge_jar` is the shape): the venues
    /// it would have covered read exactly as they read today.
    baseline_path: Option<PathBuf>,
    /// `<project>/settings/state/venue-catalog-local.json` — the operator's OWN credentialed lists
    /// (`docs/decisions/0066` decision 9), or `None` for a process that resolved no project.
    ///
    /// ⚠ **READ-ONLY from here.** `vike-backend catalog refresh` is the single writer, and that is what
    /// keeps it safe beside `cache_path`: this handle persists `CatalogCache` by writing the WHOLE
    /// file from memory, so a second writer of THAT file would lose rows on the next refresh. A
    /// desktop with this file open therefore shows what the CLI last wrote, and picks up a newer
    /// write at the next start — a restart-required freshness the record accepts, because the
    /// alternative is a per-frame stat of a file nothing asked about.
    local_path: Option<PathBuf>,
    /// **The venues THIS BOX holds credentials for**, pushed by the shell.
    ///
    /// Late-bound, like [`Self::dial`] and for a version of the same reason: credentials are edited
    /// at runtime in Connections, so a set baked in at `App::new` would tell an operator who had
    /// just saved their alpaca keys that no keys were saved. The shell pushes it from the FRESH
    /// credential read the Connections tool already performs — see
    /// [`Self::set_credentialed_venues`].
    credentialed: Mutex<Vec<String>>,
    /// **Where a `ServerBacked` press dials.** Late-bound rather than a constructor parameter, and
    /// that is the sizing hazard of this whole change written down: this handle is built ONCE at
    /// `App::new`, while the resolved datahub address and the observe-key name are properties of
    /// the ACTIVE BACKEND RECORD, which a switch replaces at runtime. A dial baked into the
    /// constructor would go stale SILENTLY, because a stale address still connects — and signing
    /// the new backend's datahub with the previous record's key is `bad mac` with nothing on
    /// screen that could explain it. `crate::md_session::MdSession::set_addr` exists for that
    /// precise bug and this is its twin; see [`Self::dial_is_stale`].
    dial: Mutex<Option<CatalogDial>>,
    state: Arc<Mutex<CatalogState>>,
}

impl CatalogRefresh {
    /// Build the handle. Does no I/O — [`Self::spawn_initial`] performs the disk read off-thread,
    /// because a cache holding a large venue is megabytes of JSON and a GUI start owes no frame to
    /// parsing it. The dial starts empty; the shell pushes one per frame through [`Self::set_dial`].
    #[must_use]
    pub fn new(
        providers: Vec<Arc<dyn CatalogProvider>>,
        cache_path: Option<PathBuf>,
        baseline_path: Option<PathBuf>,
        local_path: Option<PathBuf>,
        tx: Sender<Catalog>,
    ) -> Self {
        Self {
            providers,
            cache_path,
            baseline_path,
            local_path,
            credentialed: Mutex::new(Vec::new()),
            dial: Mutex::new(None),
            state: Arc::new(Mutex::new(CatalogState {
                cache: CatalogCache::default(),
                baseline: BaselineCatalog::default(),
                local: BaselineCatalog::default(),
                per_venue: BTreeMap::new(),
                tx,
            })),
        }
    }

    /// **Tell the handle which venues this box holds credentials for.**
    ///
    /// It changes no route and no control — only the SENTENCE a credentialed venue's row reads,
    /// from "what would arm it" to "it is armed" (`docs/decisions/0066` decision 9). The caller is
    /// a BINARY, because only a binary may read the credential store; the desktop pushes it from
    /// the fresh read its Connections tool already performs, so an operator who has just saved a
    /// venue's keys sees the sentence change without restarting.
    ///
    /// ⚠ The SET is `vike_connections::credentialed_venues`' output — the same producer the
    /// Connections grid renders — so this row and that grid cannot claim different things about one
    /// venue.
    pub fn set_credentialed_venues(&self, venues: Vec<String>) {
        *self.credentialed.lock().unwrap_or_else(|p| p.into_inner()) = venues;
    }

    /// **Does the stored dial still answer for this address and key name?** Wait-free — one
    /// uncontended lock and two compares — so the shell may call it every frame, and RESOLVING a
    /// dial (which opens the credential store) happens only when this says so.
    ///
    /// ⚠ **`None` is NOT a different server — it is "no new answer", and the dial is HELD.** The
    /// verbatim rule and reason of `crate::md_session::MdSession::set_addr`: the second rung of
    /// `crate::datahub_resolve::resolve_datahub_addr` is the active backend's `Welcome`
    /// advertisement, which the observe bridge CLEARS the moment the link drops and restores only
    /// after the next handshake. On the documented default box that advertisement is the whole
    /// address ladder, so every tradehub blip would otherwise arrive here as "the datahub went
    /// away" and turn every live button into a `NoBackend` refusal for a server that did not go
    /// anywhere.
    #[must_use]
    pub fn dial_is_stale(&self, addr: Option<&str>, key_name: &str) -> bool {
        let dial = self.dial.lock().unwrap_or_else(|p| p.into_inner());
        match (&*dial, addr) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(d), Some(a)) => d.addr != a || d.key_name != key_name,
        }
    }

    /// Point the handle at a datahub. `None` CLEARS it — call it only when the caller genuinely
    /// means "there is no datahub", never on a transient advertisement gap (see
    /// [`Self::dial_is_stale`], which is the guard that expresses the difference).
    pub fn set_dial(&self, dial: Option<CatalogDial>) {
        *self.dial.lock().unwrap_or_else(|p| p.into_inner()) = dial;
    }

    /// The dial as a worker thread needs it — a clone, so no lock is held across a socket.
    fn dial_snapshot(&self) -> Option<CatalogDial> {
        self.dial.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The venue slugs this binary links a provider for — [`catalog_source_for`]'s `direct` set.
    fn direct_slugs(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.venue()).collect()
    }

    /// The rows the Instruments screen renders — one per roster venue, in roster order.
    ///
    /// The roster is [`vike_catalog::BRIDGE_VENUES`], which IS `vike_model::VENUES`, so a new
    /// venue appears here the day its bridge crate lands and needs no row written anywhere.
    #[must_use]
    pub fn rows(&self) -> Vec<VenueCatalogRow> {
        // The dial's lock is taken BEFORE the state lock and released immediately, so the two are
        // never held together and there is no ordering for a second caller to get wrong.
        let server = self.dial_snapshot().map(|d| d.addr);
        let credentialed = self.credentialed.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let direct = self.direct_slugs();
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        vike_catalog::BRIDGE_VENUES
            .iter()
            .map(|venue| {
                let provider = self.providers.iter().find(|p| p.venue() == *venue);
                VenueCatalogRow {
                    venue: (*venue).to_string(),
                    mode: provider.map(|p| p.mode()),
                    source: catalog_source_for(venue, &direct),
                    server: server.clone(),
                    stamp: state.cache.fetched.iter().find(|s| s.venue == *venue).cloned(),
                    baseline: state.baseline.venue(venue).cloned(),
                    local: state.local.venue(venue).cloned(),
                    own_keys: credentialed.iter().any(|v| v == *venue),
                    state: state.per_venue.get(*venue).cloned().unwrap_or_default(),
                }
            })
            .collect()
    }

    /// How many instruments the PICKER holds — the number the screen's heading states.
    ///
    /// ⚠ It counts the merged universe, not `cache.instruments`, because the heading and the
    /// picker must be the same number: a heading reading "412 instruments in the picker" while the
    /// picker searched 812 of them would be a lie of exactly the kind decision 7 is about.
    #[must_use]
    pub fn total(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        merge_sources(&state.cache, &state.local, &state.baseline).len()
    }

    /// **The explicit refresh.** Refuses on [`refresh_block`]; otherwise marks the venue in flight
    /// and spawns ONE thread that fetches — locally through the linked provider, or over the wire
    /// through [`crate::catalog_wire::fetch_venue_catalog`] — folds through [`merge_refresh`],
    /// rewrites the cache and publishes the merged [`Catalog`] to the picker. Returns immediately:
    /// the frame thread never waits on a socket.
    ///
    /// `wake` runs on the worker when the attempt has finished and its result is visible in
    /// [`Self::rows`]; the GUI passes `request_repaint`, and a test passes a signal.
    pub fn request(
        &self,
        venue: &str,
        now_ms: i64,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(), RefreshBlock> {
        let row = self
            .rows()
            .into_iter()
            .find(|r| r.venue == venue)
            .ok_or(RefreshBlock::Unavailable(RefreshAvailability::NotInThisBuild))?;
        if let Some(block) = refresh_block(&row, now_ms) {
            return Err(block);
        }
        // `refresh_block` returned `None`, so the availability is one of the two refreshable arms.
        let route = match row.availability() {
            RefreshAvailability::Direct => {
                let Some(provider) = self.providers.iter().find(|p| p.venue() == venue).cloned()
                else {
                    return Err(RefreshBlock::Unavailable(RefreshAvailability::NotInThisBuild));
                };
                Route::Local(provider)
            }
            RefreshAvailability::ServerBacked => Route::Server(self.dial_snapshot()),
            other => return Err(RefreshBlock::Unavailable(other)),
        };
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state
                .per_venue
                .insert(venue.to_string(), VenueRefreshState::InFlight { since_ms: now_ms });
        }
        let (state, cache_path) = (Arc::clone(&self.state), self.cache_path.clone());
        let venue = venue.to_string();
        std::thread::Builder::new()
            .name(format!("vt-catalog-{venue}"))
            .spawn(move || {
                match route {
                    Route::Local(provider) => run_one(&provider, &state, cache_path.as_deref()),
                    Route::Server(dial) => {
                        run_server(&venue, dial.as_ref(), &state, cache_path.as_deref())
                    }
                }
                wake();
            })
            .expect("spawn catalog refresh thread");
        Ok(())
    }

    /// **Startup**: read the cache off-thread, publish it to the picker, and fetch ONCE — and only
    /// once — every LINKED Enumerable venue the cache holds no stamp for.
    ///
    /// ⚠ That last clause is the persist doc's policy read literally: *"Loaded instantly at
    /// startup; rewritten only on an explicit user refresh"*. A venue already in the cache is
    /// NEVER re-fetched by this — the button is the only thing that re-fetches. A venue that has
    /// never been fetched has nothing to load, so its first fetch is the load, and skipping it
    /// would leave a fresh install with an empty picker and no hint that a button exists.
    ///
    /// ⚠ **LOCAL PROVIDERS ONLY, deliberately.** It iterates `self.providers`, so no
    /// `ServerBacked` venue is fetched here however cold it is. Routing them would make a start
    /// open eight concurrent authenticated dials and spend eight of the datahub's tokens with
    /// nobody having asked — the exact poll `crate::catalog_wire`'s doc forbids, wearing a first
    /// run. It also bypasses [`refresh_block`] entirely, which is survivable only because the set
    /// it can reach is this binary's own linked providers.
    pub fn spawn_initial(&self, wake: impl Fn() + Send + 'static) {
        let providers: Vec<Arc<dyn CatalogProvider>> = self.providers.clone();
        let (state, cache_path) = (Arc::clone(&self.state), self.cache_path.clone());
        let (baseline_path, local_path) = (self.baseline_path.clone(), self.local_path.clone());
        std::thread::Builder::new()
            .name("vt-catalog-initial".into())
            .spawn(move || {
                // ⚠ **THE BASELINE IS READ FIRST, and the ordering is load-bearing.** The cache
                // read below publishes a `Catalog` to the picker, and that publish is the merged
                // universe — so a baseline landing AFTER it would leave the picker searching the
                // cache alone until something else republished. Reading it first costs nothing (it
                // is the same thread and the same startup) and makes the first published universe
                // the complete one.
                //
                // A baseline that is ABSENT, unreadable or INVALID is the ordinary state on every
                // box that has not installed the release asset, and it degrades:
                // `BaselineCatalog::default()` is already in place, so the venues it would have
                // covered read exactly as they read today. An INVALID one is `warn!`ed rather than
                // ignored — a file that is present and refused is a real finding, and the parser's
                // own message names which rule refused it.
                //
                // ⚠ The LOCAL document is read the same way and by the same parser — it is the
                // same TYPE, so every stamp and qualifier rule a shipped row obeys a local one
                // obeys too. That is the point of reusing it: the fact that makes a shipped list
                // honest (which environment, which division) makes the operator's own list honest,
                // and a second type would have let the second writer forget it.
                //
                // ⚠ Each document is read under its OWN `Provenance`, which is the one validation
                // rule that differs between them: a SHIPPED ctrader row is refused (a shared list
                // across brokers whose symbol ids differ can resolve a name to a different
                // instrument) while the operator's own is admitted, because theirs is their own
                // account at their own broker. Reading both as `Shipped` would refuse the one
                // ctrader list that can ever be honest.
                for (path, from) in [
                    (&baseline_path, vike_catalog::Provenance::Shipped),
                    (&local_path, vike_catalog::Provenance::Operator),
                ] {
                    let Some(path) = path.as_deref() else { continue };
                    let kind = match from {
                        vike_catalog::Provenance::Shipped => "shipped baseline",
                        vike_catalog::Provenance::Operator => "own credentialed list",
                    };
                    match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| {
                        vike_catalog::BaselineCatalog::parse_as(&b, from).map_err(|e| e.to_string())
                    }) {
                        Ok(doc) => {
                            let venues = doc.venues.len();
                            {
                                let mut g = state.lock().unwrap_or_else(|p| p.into_inner());
                                match from {
                                    vike_catalog::Provenance::Shipped => g.baseline = doc,
                                    vike_catalog::Provenance::Operator => g.local = doc,
                                }
                            }
                            if venues > 0 {
                                tracing::info!(
                                    "catalog: {kind} loaded from {} ({venues} venues)",
                                    path.display()
                                );
                            }
                        }
                        Err(e) if path.exists() => tracing::warn!(
                            "catalog: {kind} at {} is present and unusable ({e}) — the venues it \
                             covers will read as though it were absent",
                            path.display()
                        ),
                        Err(_) => {}
                    }
                }
                if let Some(path) = cache_path.as_deref() {
                    match load_cache(path) {
                        Ok(cache) => {
                            state.lock().unwrap_or_else(|p| p.into_inner()).cache = cache;
                        }
                        // An absent cache is the ordinary first run and says nothing; a cache that
                        // is present and unreadable is a real finding, so it is logged rather than
                        // folded into the same silence.
                        Err(e) if path.exists() => {
                            tracing::warn!("catalog cache at {} unreadable: {e}", path.display());
                        }
                        Err(_) => {}
                    }
                }
                // ⚠ ONE publish for BOTH sources, and it is outside the cache arm on purpose: it
                // used to live inside it, which was correct while the cache was the only source
                // and became a hole the moment a second one existed — a box with a shipped
                // baseline and NO cache file (a fresh install, which is precisely the case the
                // baseline exists for) would have published nothing at all and searched an empty
                // picker until the first press.
                {
                    let guard = state.lock().unwrap_or_else(|p| p.into_inner());
                    let catalog = guard.universe();
                    if !catalog.is_empty() {
                        let _ = guard.tx.send(catalog);
                    }
                }
                let cold: Vec<Arc<dyn CatalogProvider>> = {
                    let guard = state.lock().unwrap_or_else(|p| p.into_inner());
                    providers
                        .into_iter()
                        .filter(|p| p.mode() == CatalogMode::Enumerable)
                        .filter(|p| !guard.cache.fetched.iter().any(|s| s.venue == p.venue()))
                        .collect()
                };
                for provider in cold {
                    run_one(&provider, &state, cache_path.as_deref());
                }
                wake();
            })
            .expect("spawn catalog initial thread");
    }
}

/// Which leg a spawned refresh takes. Computed on the frame thread (where the providers and the
/// dial are readable) and MOVED into the worker, so the worker holds no `&self`.
enum Route {
    Local(Arc<dyn CatalogProvider>),
    Server(Option<CatalogDial>),
}

/// One LINKED venue's fetch → fold → persist → publish, on whatever thread calls it. Free rather
/// than a method so both entry points run byte-identical work and there is one place the ordering
/// lives.
///
/// The lock is held across the fold and the channel send but NOT across the fetch: a blocking
/// venue REST call inside the mutex would stall `rows()` — i.e. the frame — for the whole fetch.
fn run_one(
    provider: &Arc<dyn CatalogProvider>,
    state: &Arc<Mutex<CatalogState>>,
    cache_path: Option<&std::path::Path>,
) {
    let venue = provider.venue().to_string();
    let fetched = provider.list_instruments().map_err(|e| e.to_string());
    // ⚠ `truncated: false` is a FACT rather than a default: no cap sits between a linked provider
    // and this fold. The wire's `CATALOG_MAX_INSTRUMENTS` bounds a `ServerBacked` listing only.
    settle(&venue, state, cache_path, Fetched::List { instruments: fetched, truncated: false });
}

/// One `ServerBacked` venue's fetch → fold → persist → publish. The wire twin of [`run_one`], and
/// it goes through the SAME [`settle`], so a listing that arrived over a socket lands in the same
/// merge with the same failure rule rather than beside it.
///
/// ⚠ Every non-listing answer becomes [`RefreshOutcome::Refused`] and touches no instrument — it
/// is never folded into `Failed` (which would call a venue property an error) nor into `Empty`
/// (which would render *"the venue answered with nothing"*, the lie 0062's decision 5 exists to
/// prevent).
fn run_server(
    venue: &str,
    dial: Option<&CatalogDial>,
    state: &Arc<Mutex<CatalogState>>,
    cache_path: Option<&std::path::Path>,
) {
    let fetched = match fetch_venue_catalog(dial, venue) {
        CatalogFetchReport::Listed { instruments, truncated, .. } => {
            Fetched::List { instruments: Ok(instruments), truncated }
        }
        other => {
            let line = crate::catalog_wire::render_catalog_fetch_status(&other);
            match other {
                // A dead or unreachable server is a FAILURE — retrying it CAN work, which is
                // exactly what distinguishes it from a refusal, whose answer a retry cannot change.
                CatalogFetchReport::ConnectFailed { .. } | CatalogFetchReport::Failed { .. } => {
                    Fetched::List { instruments: Err(line), truncated: false }
                }
                _ => Fetched::Refused { why: line },
            }
        }
    };
    settle(venue, state, cache_path, fetched);
}

/// The shared tail of both routes: fold, stamp, publish, persist, log — in that order, in one
/// place, so the two legs cannot drift about what a completed attempt does.
fn settle(
    venue: &str,
    state: &Arc<Mutex<CatalogState>>,
    cache_path: Option<&std::path::Path>,
    fetched: Fetched,
) {
    let now_ms = vike_model::now_ms();
    let (outcome, saved) = {
        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
        let outcome = match fetched {
            Fetched::List { instruments, truncated } => {
                merge_refresh(&mut guard.cache, venue, instruments, now_ms, truncated)
            }
            // ⚠ No cache access beyond the COUNT — a refusal is not a fold, and the `kept` it
            // reports is the list it deliberately left alone.
            Fetched::Refused { why } => RefreshOutcome::Refused {
                why,
                kept: guard.cache.instruments.iter().filter(|i| i.venue == venue).count(),
            },
        };
        guard.per_venue.insert(
            venue.to_string(),
            VenueRefreshState::Done { at_ms: now_ms, outcome: outcome.clone() },
        );
        let saved = if outcome.adopted() {
            // Publish the WHOLE merged universe, not this venue's slice: the picker holds one
            // `Catalog` and adopting a slice would drop every other venue from it.
            //
            // ⚠ `universe()` rather than the cache alone, and the direction matters at exactly this
            // moment: a venue the operator has just refreshed now has a STAMP, so
            // `merge_baseline` stops contributing shipped rows for it — which is the shipped list
            // being REPLACED by the operator's own, in one step, with nothing written back to the
            // baseline and nothing of the operator's overwritten.
            let _ = guard.tx.send(guard.universe());
            // ⚠ Only the CACHE is persisted. The baseline is a read-only source that arrived as a
            // release asset; writing it into `catalog.json` would make a state wipe the thing that
            // deletes a shipped artifact, and would stamp it with an age it does not have.
            cache_path.map(|path| (path.to_path_buf(), guard.cache.clone()))
        } else {
            None
        };
        (outcome, saved)
    };
    if let Some((path, cache)) = saved
        && let Err(e) = save_cache(&path, &cache)
    {
        // A cache that cannot be written is a real loss (the next start re-fetches) but not a
        // reason to discard a refresh the picker has already adopted.
        tracing::warn!("catalog cache write to {} failed: {e}", path.display());
    }
    match &outcome {
        RefreshOutcome::Refreshed { count, .. } => {
            tracing::info!("catalog refresh: {venue} → {count} instruments")
        }
        // A REFUSAL is `info`, not `warn`: an unarmed lane and a credentialed venue are correct
        // configurations, and logging them as faults is how a default reads as a problem.
        RefreshOutcome::Refused { why, .. } => tracing::info!("catalog refresh: {venue}: {why}"),
        other => tracing::warn!("catalog refresh: {venue}: {}", other.line()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_catalog::AssetClass;

    fn inst(venue: &str, sym: &str) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: AssetClass::CryptoSpot,
            base: "BTC".into(),
            quote: "USDT".into(),
            description: String::new(),
            properties: Default::default(),
            // ⚠ `None` is the ORDINARY state for both — see `vike_catalog::Instrument::contract_type`.
            contract_type: None,
            settle_asset: None,
        }
    }

    fn seeded() -> CatalogCache {
        let mut cache = CatalogCache::default();
        merge_refresh(&mut cache, "binance", Ok(vec![inst("binance", "BTCUSDT")]), 1_000, false);
        merge_refresh(&mut cache, "okx", Ok(vec![inst("okx", "BTC-USDT")]), 1_000, false);
        cache
    }

    fn row(
        venue: &str,
        source: Option<CatalogSource>,
        mode: Option<CatalogMode>,
    ) -> VenueCatalogRow {
        VenueCatalogRow {
            venue: venue.into(),
            mode,
            source,
            server: Some("127.0.0.1:7878".into()),
            stamp: None,
            baseline: None,
            local: None,
            own_keys: false,
            state: VenueRefreshState::Idle,
        }
    }

    #[test]
    fn a_successful_merge_replaces_only_its_own_venue_and_stamps_it() {
        let mut cache = seeded();
        let out = merge_refresh(
            &mut cache,
            "binance",
            Ok(vec![inst("binance", "ETHUSDT"), inst("binance", "SOLUSDT")]),
            9_000,
            false,
        );
        assert_eq!(out, RefreshOutcome::Refreshed { count: 2, previous: 1, truncated: false });
        let binance: Vec<&str> = cache
            .instruments
            .iter()
            .filter(|i| i.venue == "binance")
            .map(|i| i.raw_symbol.as_str())
            .collect();
        assert_eq!(binance, ["ETHUSDT", "SOLUSDT"], "the old row is gone, the new ones are in");
        assert_eq!(
            cache.instruments.iter().filter(|i| i.venue == "okx").count(),
            1,
            "another venue's rows are untouched"
        );
        let stamp = cache.fetched.iter().find(|s| s.venue == "binance").unwrap();
        assert_eq!((stamp.count, stamp.last_refreshed_ms), (2, 9_000));
        assert_eq!(
            cache.fetched.iter().find(|s| s.venue == "okx").unwrap().last_refreshed_ms,
            1_000,
            "another venue's stamp is untouched"
        );
    }

    /// The rule the module exists for, at the pure layer: neither failure path writes.
    #[test]
    fn a_failed_or_empty_merge_writes_nothing_at_all() {
        for fetched in [Err("connection reset".to_string()), Ok(Vec::new())] {
            let mut cache = seeded();
            let before = cache.clone();
            let out = merge_refresh(&mut cache, "binance", fetched, 9_000, false);
            assert!(!out.adopted(), "{out:?} must not be adopted");
            assert_eq!(cache.instruments, before.instruments, "instruments untouched");
            assert_eq!(
                cache.fetched.iter().find(|s| s.venue == "binance").unwrap().last_refreshed_ms,
                1_000,
                "the stamp must not move — a moved stamp claims a refresh that did not happen"
            );
        }
    }

    #[test]
    fn a_first_fetch_that_fails_leaves_the_venue_unstamped() {
        let mut cache = CatalogCache::default();
        let out = merge_refresh(&mut cache, "binance", Err("timeout".into()), 9_000, false);
        assert_eq!(out, RefreshOutcome::Failed { error: "timeout".into(), kept: 0 });
        assert!(cache.fetched.is_empty(), "no stamp for a venue that never answered");
    }

    /// A TRUNCATED listing is adopted — it is real data — and the outcome SAYS it is short.
    #[test]
    fn a_truncated_listing_is_adopted_and_says_so() {
        let mut cache = CatalogCache::default();
        let out =
            merge_refresh(&mut cache, "polymarket", Ok(vec![inst("polymarket", "X")]), 1, true);
        assert_eq!(out, RefreshOutcome::Refreshed { count: 1, previous: 0, truncated: true });
        assert!(out.adopted(), "a short list is still a list");
        assert!(out.line().contains("TRUNCATED"), "{}", out.line());
        assert!(
            !RefreshOutcome::Refreshed { count: 1, previous: 0, truncated: false }
                .line()
                .contains("TRUNCATED"),
            "an untruncated listing says nothing about a cap"
        );
    }

    /// **The routing decision, over the REAL table** — the six classes the screen must tell apart.
    #[test]
    fn availability_follows_the_route_and_not_the_local_provider_alone() {
        // deribit: linked here ⇒ Direct.
        assert_eq!(
            row("deribit", Some(CatalogSource::Direct), Some(CatalogMode::Enumerable))
                .availability(),
            RefreshAvailability::Direct
        );
        // binance: publicly enumerable, unlinked ⇒ the datahub answers.
        assert_eq!(
            row("binance", Some(CatalogSource::ServerBacked), None).availability(),
            RefreshAvailability::ServerBacked
        );
        // A linked QueryBacked provider keeps its own answer.
        assert_eq!(
            row("x", Some(CatalogSource::Direct), Some(CatalogMode::QueryBacked)).availability(),
            RefreshAvailability::QueryBacked
        );
        // ...and the three that route NOWHERE read the TABLE, not the build.
        assert_eq!(
            row("alpaca", None, None).availability(),
            RefreshAvailability::Credentialed { own_keys: false }
        );
        assert!(matches!(
            row("ig", None, None).availability(),
            RefreshAvailability::NoBulkList { .. }
        ));
        assert!(matches!(
            row("ibkr", None, None).availability(),
            RefreshAvailability::NoBulkList { .. }
        ));
        // A venue off the roster is fail-closed rather than reported as a venue property.
        assert_eq!(row("kraken", None, None).availability(), RefreshAvailability::NotInThisBuild);
    }

    /// **A credentialed venue names WHAT WOULD ARM IT, and says whether it is armed here.**
    /// `docs/decisions/0066` decision 9.
    ///
    /// ⚠ Mutation proof: make `RefreshAvailability::Credentialed`'s note return the shared sentence
    /// alone (drop the `credential_note` append) and the last four assertions go red — the row
    /// reads "this server will not" and stops, which is the *"this cannot be refreshed"* sentence
    /// the record replaced. Invert `credential_note`'s branch and the middle two swap.
    #[test]
    fn a_credentialed_venue_names_the_act_and_whether_this_box_can_perform_it() {
        let without = RefreshAvailability::Credentialed { own_keys: false }.note("alpaca").unwrap();
        let with = RefreshAvailability::Credentialed { own_keys: true }.note("alpaca").unwrap();

        // The TRUNK is the server's own sentence, which names the ACT and no screen — it is
        // printed by a daemon too.
        for n in [&without, &with] {
            assert!(n.contains("vike-backend catalog refresh alpaca"), "{n}");
            assert!(n.contains("will not spend those on a client's request"), "{n}");
            // …and never a server switch, in either direction: nothing arms this at a server.
            assert!(!n.contains("VIKE_DATAHUB_VENUE_CATALOG"), "{n}");
            assert!(!n.contains("venue_catalog_off"), "{n}");
            assert!(!n.contains("0 instruments"), "never a count: {n}");
        }
        // …and the LOCAL half, which is a fact about this box and may name a local screen.
        assert!(without.contains("No `ALPACA` credentials are saved on this box"), "{without}");
        assert!(without.contains("Connections"), "it names what would arm it: {without}");
        assert!(with.contains("ARE saved on this box"), "{with}");
        assert!(!with.contains("Connections"), "an armed venue needs no setup step: {with}");
    }

    /// Exactly two arms may render a control, and every other arm carries a sentence naming the
    /// venue — a disabled button with no explanation is the thing this screen exists to avoid.
    #[test]
    fn only_the_two_routed_arms_are_refreshable_and_the_rest_explain_themselves() {
        assert!(RefreshAvailability::Direct.refreshable());
        assert!(RefreshAvailability::ServerBacked.refreshable());
        assert!(RefreshAvailability::Direct.note("deribit").is_none());
        for a in [
            RefreshAvailability::QueryBacked,
            RefreshAvailability::Credentialed { own_keys: false },
            RefreshAvailability::Credentialed { own_keys: true },
            RefreshAvailability::NoBulkList { why: "no bulk market list" },
            RefreshAvailability::NotInThisBuild,
        ] {
            assert!(!a.refreshable(), "{a:?} must render no control");
            let note = a.note("alpaca").expect("a reason");
            assert!(note.len() > 20, "{a:?}: too thin to render: {note}");
            assert!(!note.contains("0 instruments"), "{a:?} must never read as a count: {note}");
        }
        // The two refusal arms render the SERVER-side sentence, so they name the venue.
        assert!(
            RefreshAvailability::Credentialed { own_keys: false }
                .note("oanda")
                .unwrap()
                .contains("oanda")
        );
        assert!(
            RefreshAvailability::NoBulkList { why: "no bulk market list" }
                .note("ig")
                .unwrap()
                .contains("ig")
        );
    }

    #[test]
    fn the_budget_refuses_an_in_flight_and_a_just_refreshed_venue() {
        let mut r = row("deribit", Some(CatalogSource::Direct), Some(CatalogMode::Enumerable));
        assert_eq!(refresh_block(&r, 100_000), None, "idle and routed ⇒ go");

        r.state = VenueRefreshState::InFlight { since_ms: 100_000 };
        assert_eq!(refresh_block(&r, 100_001), Some(RefreshBlock::InFlight));

        r.state =
            VenueRefreshState::Done { at_ms: 100_000, outcome: RefreshOutcome::Empty { kept: 0 } };
        assert_eq!(
            refresh_block(&r, 100_000 + REFRESH_COOLDOWN_MS - 1),
            Some(RefreshBlock::Cooldown { remaining_ms: 1 })
        );
        assert_eq!(
            refresh_block(&r, 100_000 + REFRESH_COOLDOWN_MS),
            None,
            "the cooldown ends, it does not latch"
        );

        let ig = row("ig", None, None);
        assert!(
            matches!(
                refresh_block(&ig, 0),
                Some(RefreshBlock::Unavailable(RefreshAvailability::NoBulkList { .. }))
            ),
            "a venue with no bulk list is refused before any budget question is asked"
        );
    }

    /// **A REFUSED attempt starts the same cooldown as any other**, so a routed venue whose server
    /// said no cannot be re-asked faster than the server's own refill rate.
    #[test]
    fn a_refusal_spends_the_cooldown_like_any_other_completed_attempt() {
        let mut r = row("binance", Some(CatalogSource::ServerBacked), None);
        r.state = VenueRefreshState::Done {
            at_ms: 100_000,
            outcome: RefreshOutcome::Refused { why: "not armed".into(), kept: 0 },
        };
        assert_eq!(
            refresh_block(&r, 100_000 + 1_000),
            Some(RefreshBlock::Cooldown { remaining_ms: REFRESH_COOLDOWN_MS - 1_000 })
        );
        // ...and the hover does not claim a refresh happened.
        let line = RefreshBlock::Cooldown { remaining_ms: 30_000 }.line("binance");
        assert!(!line.contains("just refreshed"), "{line}");
        assert!(line.contains("asked again in 31s"), "{line}");
    }

    /// A `ServerBacked` venue with no datahub resolved keeps its control, disabled, with the
    /// sentence that names the fix — it CAN be refreshed, just not from a desktop with nothing to
    /// ask. A `Direct` venue is unaffected by the dial.
    #[test]
    fn a_routed_venue_with_no_backend_is_blocked_by_name() {
        let mut r = row("binance", Some(CatalogSource::ServerBacked), None);
        assert_eq!(refresh_block(&r, 0), None, "with a dial it is pressable");
        r.server = None;
        assert_eq!(refresh_block(&r, 0), Some(RefreshBlock::NoBackend));
        let line = RefreshBlock::NoBackend.line("binance");
        assert!(line.contains("config.datahub_addr"), "{line}");

        let mut direct = row("deribit", Some(CatalogSource::Direct), Some(CatalogMode::Enumerable));
        direct.server = None;
        assert_eq!(refresh_block(&direct, 0), None, "a local fetch needs no datahub");
    }

    #[test]
    fn the_stamp_label_reads_as_elapsed_time() {
        let stamp = VenueStamp { venue: "v".into(), last_refreshed_ms: 0, count: 3 };
        assert_eq!(refreshed_label(None, 0), "never");
        assert_eq!(refreshed_label(Some(&stamp), 5_000), "just now");
        assert_eq!(refreshed_label(Some(&stamp), 30_000), "30s ago");
        assert_eq!(refreshed_label(Some(&stamp), 600_000), "10 min ago");
        assert_eq!(refreshed_label(Some(&stamp), 7_200_000), "2 h ago");
        assert_eq!(refreshed_label(Some(&stamp), 172_800_000), "2 d ago");
    }
}
