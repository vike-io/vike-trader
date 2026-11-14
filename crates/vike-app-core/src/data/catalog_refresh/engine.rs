//! The `CatalogRefresh` handle and the fetch -> fold -> persist -> publish runners behind it.

use super::*;
use crate::lock;

// ------------------------------------------------------------------------------------------------
// The handle
// ------------------------------------------------------------------------------------------------

/// The shared mutable half, behind one lock. `tx` lives in here rather than beside it because
/// `std::sync::mpsc::Sender` is `Send` but NOT `Sync`, and [`CatalogRefresh`] is shared by `&`.
struct CatalogState {
    cache: CatalogCache,
    /// **The operator's OWN CREDENTIALED lists** (the settings database's `operator_instrument_list`
    /// rows), held APART from the cache and never written back: [`CatalogCacheVenueStamp`] records
    /// WHEN and HOW MANY and never FROM WHERE, so one folded through [`merge_refresh`] would render
    /// through [`refreshed_label`] as an AGE. Written by `vike-backend catalog refresh` on the box that
    /// holds the keys, served by the datahub with its kept lists; read here and never written.
    local: InstrumentLists,
    per_venue: BTreeMap<String, CatalogRefreshVenueState>,
    /// Where a newly merged [`Catalog`] is published — the SAME channel the picker already drains.
    tx: Sender<Catalog>,
}

impl CatalogState {
    /// **The picker's universe**: the kept lists, plus the operator's own list for every venue
    /// with no kept list. The ONE place the two sources are combined, so a caller cannot publish
    /// one without the other — see [`vike_catalog::merge_sources`] for why the key is the cache's
    /// STAMP and not its row count.
    fn universe(&self) -> Catalog {
        Catalog::from_instruments(merge_sources(&self.cache, &self.local))
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

/// **The catalog's owner in a GUI binary**: the in-memory cache of the datahub's kept lists, the
/// per-venue refresh state, the route to the backend's datahub, and the one channel the symbol
/// picker reads from.
///
/// ⚠ **It keeps NO file** (the owner's ruling of 2026-11-12): the venues' lists are the DATAHUB's,
/// kept in `<project>/instruments` on the server and refreshed there weekly. This handle reads them
/// (`Request::InstrumentLists`) when a dial appears and asks for a refresh on a press
/// (`Request::RefreshInstrumentLists`). The desktop's old `settings/state/catalog.json` is read and
/// written by nothing; a leftover copy is ignored.
///
/// Shared by `&`: every method takes `&self`, the mutable halves sit behind locks, and the
/// worker threads hold `Arc` clones of the lock and of the provider — never of `self` — so the
/// handle has no lifetime entanglement with a spawned fetch.
pub struct CatalogRefresh {
    /// The providers THIS BINARY links. A constructor parameter, never a registry: a crate down
    /// here cannot know what the binary above it linked.
    providers: Vec<Arc<dyn CatalogProvider>>,
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
    /// screen that could explain it. `crate::data::market_data_session::MarketDataSession::set_address` exists for that
    /// precise bug and this is its twin; see [`Self::dial_is_stale`].
    dial: Mutex<Option<CatalogDial>>,
    state: Arc<Mutex<CatalogState>>,
}

impl CatalogRefresh {
    /// Build the handle. Does no I/O: the datahub's kept lists are read when [`Self::set_dial`]
    /// first points at it. The dial starts empty; the shell pushes one per frame through
    /// [`Self::set_dial`].
    #[must_use]
    pub fn new(providers: Vec<Arc<dyn CatalogProvider>>, tx: Sender<Catalog>) -> Self {
        Self {
            providers,
            credentialed: Mutex::new(Vec::new()),
            dial: Mutex::new(None),
            state: Arc::new(Mutex::new(CatalogState {
                cache: CatalogCache::default(),
                local: InstrumentLists::default(),
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
    /// ⚠ The SET is `vike_gateway_panel::credentialed_venues`' output — the same producer the
    /// Connections grid renders — so this row and that grid cannot claim different things about one
    /// venue.
    pub fn set_credentialed_venues(&self, venues: Vec<String>) {
        *lock(&self.credentialed) = venues;
    }

    /// **Does the stored dial still answer for this address and key name?** Wait-free — one
    /// uncontended lock and two compares — so the shell may call it every frame, and RESOLVING a
    /// dial (which opens the credential store) happens only when this says so.
    ///
    /// ⚠ **`None` is NOT a different server — it is "no new answer", and the dial is HELD.** The
    /// verbatim rule and reason of `crate::data::market_data_session::MarketDataSession::set_address`: the second rung of
    /// `crate::data::datahub_resolve::resolve_datahub_address` is the active backend's `Welcome`
    /// advertisement, which the observe bridge CLEARS the moment the link drops and restores only
    /// after the next handshake. On the documented default box that advertisement is the whole
    /// address ladder, so every tradehub blip would otherwise arrive here as "the datahub went
    /// away" and turn every live button into a `NoBackend` refusal for a server that did not go
    /// anywhere.
    #[must_use]
    pub fn dial_is_stale(&self, addr: Option<&str>, key_name: &str) -> bool {
        let dial = lock(&self.dial);
        match (&*dial, addr) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(d), Some(a)) => d.addr != a || d.key_name != key_name,
        }
    }

    /// Point the handle at a datahub. `None` CLEARS it — call it only when the caller genuinely
    /// means "there is no datahub", never on a transient advertisement gap (see
    /// [`Self::dial_is_stale`], which is the guard that expresses the difference).
    ///
    /// A NEW datahub is asked, once and off-thread, for every list it keeps and for the operator's
    /// own lists (`Request::InstrumentLists`): that is this handle's whole load of the universe.
    pub fn set_dial(&self, dial: Option<CatalogDial>) {
        *lock(&self.dial) = dial.clone();
        if let Some(dial) = dial {
            let state = Arc::clone(&self.state);
            let spawned = std::thread::Builder::new()
                .name("app-kept-lists".into())
                .spawn(move || load_kept_lists(&dial, &state));
            if let Err(e) = spawned {
                tracing::warn!("catalog: the kept lists could not be asked for: {e}");
            }
        }
    }

    /// **Adopt kept lists as though the datahub had answered them**: each replaces its venue in the cache,
    /// stamped with its own fetch time, and the universe is published. The fold [`Self::set_dial`]'s read runs, and
    /// the live smoke's door for staging a venue the datahub keeps no list for.
    pub fn adopt(&self, lists: Vec<VenueInstrumentList>) {
        adopt_into(&self.state, lists, None);
    }

    /// The dial as a worker thread needs it — a clone, so no lock is held across a socket. Also the
    /// tool poll threads' dial (`crate::tools::spawn_tool_fetchers`' `dial`), so the datahub
    /// clients follow one backend switch together.
    #[must_use]
    pub fn dial_snapshot(&self) -> Option<CatalogDial> {
        lock(&self.dial).clone()
    }

    /// The venue slugs this binary links a provider for — [`catalog_source_for`]'s `direct` set.
    fn direct_slugs(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.venue()).collect()
    }

    /// The rows the Instruments screen renders — one per roster venue, in roster order.
    ///
    /// The roster is [`vike_model::VENUES`], which IS `vike_model::VENUES`, so a new
    /// venue appears here the day its bridge crate lands and needs no row written anywhere.
    #[must_use]
    pub fn rows(&self) -> Vec<CatalogRefreshVenueRow> {
        // The dial's lock is taken BEFORE the state lock and released immediately, so the two are
        // never held together and there is no ordering for a second caller to get wrong.
        let server = self.dial_snapshot().map(|d| d.addr);
        let credentialed = lock(&self.credentialed).clone();
        let direct = self.direct_slugs();
        let state = lock(&self.state);
        vike_model::VENUES
            .iter()
            .map(|venue| {
                let provider = self.providers.iter().find(|p| p.venue() == *venue);
                CatalogRefreshVenueRow {
                    venue: (*venue).to_string(),
                    mode: provider.map(|p| p.mode()),
                    source: catalog_source_for(venue, &direct),
                    server: server.clone(),
                    stamp: state.cache.fetched.iter().find(|s| s.venue == *venue).cloned(),
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
        let state = lock(&self.state);
        merge_sources(&state.cache, &state.local).len()
    }

    /// **The explicit refresh.** Refuses on [`refresh_block`]; otherwise marks the venue in flight
    /// and spawns ONE thread that fetches — locally through the linked provider, or over the wire
    /// through [`crate::data::catalog_wire::refresh_kept_list`] — folds through [`merge_refresh`],
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
    ) -> Result<(), CatalogRefreshBlock> {
        let row =
            self.rows().into_iter().find(|r| r.venue == venue).ok_or(
                CatalogRefreshBlock::Unavailable(CatalogRefreshAvailability::NotInThisBuild),
            )?;
        if let Some(block) = refresh_block(&row, now_ms) {
            return Err(block);
        }
        // `refresh_block` returned `None`, so the availability is one of the two refreshable arms.
        let route = match row.availability() {
            CatalogRefreshAvailability::Direct => {
                let Some(provider) = self.providers.iter().find(|p| p.venue() == venue).cloned()
                else {
                    return Err(CatalogRefreshBlock::Unavailable(
                        CatalogRefreshAvailability::NotInThisBuild,
                    ));
                };
                Route::Local(provider)
            }
            CatalogRefreshAvailability::ServerBacked => Route::Server(self.dial_snapshot()),
            other => return Err(CatalogRefreshBlock::Unavailable(other)),
        };
        {
            let mut state = lock(&self.state);
            state
                .per_venue
                .insert(venue.to_string(), CatalogRefreshVenueState::InFlight { since_ms: now_ms });
        }
        let state = Arc::clone(&self.state);
        let venue = venue.to_string();
        std::thread::Builder::new()
            // The venue alone: `vike_model::venues::VENUES`' longest id (`hyperliquid`) fills the 15 bytes.
            .name(format!("app-{venue}"))
            .spawn(move || {
                match route {
                    Route::Local(provider) => run_one(&provider, &state),
                    Route::Server(dial) => run_server(&venue, dial.as_ref(), &state),
                }
                wake();
            })
            .expect("spawn catalog refresh thread");
        Ok(())
    }

    /// **Startup**: off-thread, publish the universe to the picker and fetch ONCE every LINKED
    /// Enumerable venue that holds no list yet. The datahub's kept lists arrive separately, when
    /// [`Self::set_dial`] first points at it.
    ///
    /// ⚠ **LOCAL PROVIDERS ONLY, deliberately.** It iterates `self.providers`, so no
    /// `ServerBacked` venue is fetched here however cold it is: the datahub refreshes those itself,
    /// weekly, and this handle only reads them. It also bypasses [`refresh_block`] entirely, which
    /// is survivable only because the set it can reach is this binary's own linked providers.
    pub fn spawn_initial(&self, wake: impl Fn() + Send + 'static) {
        let providers: Vec<Arc<dyn CatalogProvider>> = self.providers.clone();
        let state = Arc::clone(&self.state);
        std::thread::Builder::new()
            .name("app-catalog".into())
            .spawn(move || {
                // ONE publish of what is already held, before the linked providers' first fetch.
                {
                    let guard = lock(&state);
                    let catalog = guard.universe();
                    if !catalog.is_empty() {
                        let _ = guard.tx.send(catalog);
                    }
                }
                let cold: Vec<Arc<dyn CatalogProvider>> = {
                    let guard = lock(&state);
                    providers
                        .into_iter()
                        .filter(|p| p.mode() == CatalogMode::Enumerable)
                        .filter(|p| !guard.cache.fetched.iter().any(|s| s.venue == p.venue()))
                        .collect()
                };
                for provider in cold {
                    run_one(&provider, &state);
                }
                wake();
            })
            .expect("spawn catalog initial thread");
    }
}

/// **Adopt what the datahub keeps** — `Request::InstrumentLists`, on the thread [`CatalogRefresh::set_dial`]
/// spawned. Each venue the datahub has fetched replaces that venue in the cache, stamped with the
/// SERVER's fetch time; the operator's own lists replace `local` after their check
/// ([`vike_catalog::InstrumentLists::check`]). Then ONE publish. A datahub that cannot answer
/// changes nothing and says why in the log.
fn load_kept_lists(dial: &CatalogDial, state: &Arc<Mutex<CatalogState>>) {
    let reply = match crate::data::catalog_wire::fetch_kept_lists(dial) {
        Ok(reply) => reply,
        Err(e) => {
            tracing::warn!("catalog: the datahub's kept lists could not be read: {e}");
            return;
        }
    };
    let own = InstrumentLists { venues: reply.operator };
    let own = match own.check() {
        Ok(()) => Some(own),
        Err(e) => {
            tracing::warn!(
                "catalog: the operator's own lists are unusable ({e}) — the venues they cover will \
                 read as though they were absent"
            );
            None
        }
    };
    adopt_into(state, reply.venues, own);
}

/// Fold kept lists (and, when given, the operator's own) into the state, then ONE publish when anything changed:
/// a datahub that keeps nothing republishes nothing.
fn adopt_into(
    state: &Arc<Mutex<CatalogState>>,
    venues: Vec<VenueInstrumentList>,
    own: Option<InstrumentLists>,
) {
    let mut guard = lock(state);
    let mut changed = false;
    for list in venues {
        let Some(at) = list.refreshed_ms else { continue };
        adopt_kept(&mut guard.cache, list.venue, list.instruments, at);
        changed = true;
    }
    if let Some(own) = own
        && own != guard.local
    {
        guard.local = own;
        changed = true;
    }
    if changed {
        let _ = guard.tx.send(guard.universe());
    }
}

/// Replace one venue's rows in `cache` with the list the datahub keeps, stamped `at_ms`.
fn adopt_kept(cache: &mut CatalogCache, venue: String, instruments: Vec<Instrument>, at_ms: i64) {
    cache.instruments.retain(|i| i.venue != venue);
    cache.fetched.retain(|s| s.venue != venue);
    cache.fetched.push(CatalogCacheVenueStamp {
        venue,
        last_refreshed_ms: at_ms,
        count: instruments.len(),
    });
    cache.instruments.extend(instruments);
}

/// Which leg a spawned refresh takes. Computed on the frame thread (where the providers and the
/// dial are readable) and MOVED into the worker, so the worker holds no `&self`.
enum Route {
    Local(Arc<dyn CatalogProvider>),
    Server(Option<CatalogDial>),
}

/// One LINKED venue's fetch → fold → publish, on whatever thread calls it. Free rather than a
/// method so both entry points run byte-identical work and there is one place the ordering lives.
///
/// The lock is held across the fold and the channel send but NOT across the fetch: a blocking
/// venue REST call inside the mutex would stall `rows()` — i.e. the frame — for the whole fetch.
fn run_one(provider: &Arc<dyn CatalogProvider>, state: &Arc<Mutex<CatalogState>>) {
    let venue = provider.venue().to_string();
    let fetched = provider.list_instruments().map_err(|e| e.to_string());
    // ⚠ `truncated: false` is a FACT rather than a default: no cap sits between a linked provider
    // and this fold. The wire's `CATALOG_MAX_INSTRUMENTS` bounds a `ServerBacked` listing only.
    settle(&venue, state, Fetched::List { instruments: fetched, truncated: false });
}

/// One `ServerBacked` venue's refresh-now → fold → publish: the datahub fetches AND keeps the list
/// (`Request::RefreshInstrumentLists`, [`crate::data::catalog_wire::refresh_kept_list`]), and this
/// handle folds what it answered through the SAME [`settle`] a linked provider's list goes through.
///
/// ⚠ Every non-listing answer becomes [`CatalogRefreshOutcome::Refused`] and touches no instrument — it
/// is never folded into `Failed` (which would call a venue property an error) nor into `Empty`
/// (which would render *"the venue answered with nothing"*, the lie 0062's decision 5 exists to
/// prevent).
fn run_server(venue: &str, dial: Option<&CatalogDial>, state: &Arc<Mutex<CatalogState>>) {
    let fetched = match refresh_kept_list(dial, venue) {
        CatalogFetchReport::Listed { instruments, truncated, .. } => {
            Fetched::List { instruments: Ok(instruments), truncated }
        }
        other => {
            let line = crate::data::catalog_wire::render_catalog_fetch_status(&other);
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
    settle(venue, state, fetched);
}

/// The shared tail of both routes: fold, stamp, publish, log — in that order, in one place, so the
/// two legs cannot drift about what a completed attempt does. Nothing is written to disk: the
/// datahub keeps the lists.
fn settle(venue: &str, state: &Arc<Mutex<CatalogState>>, fetched: Fetched) {
    let now_ms = vike_model::now_ms();
    let outcome = {
        let mut guard = lock(state);
        let outcome = match fetched {
            Fetched::List { instruments, truncated } => {
                merge_refresh(&mut guard.cache, venue, instruments, now_ms, truncated)
            }
            // ⚠ No cache access beyond the COUNT — a refusal is not a fold, and the `kept` it
            // reports is the list it deliberately left alone.
            Fetched::Refused { why } => CatalogRefreshOutcome::Refused {
                why,
                kept: guard.cache.instruments.iter().filter(|i| i.venue == venue).count(),
            },
        };
        guard.per_venue.insert(
            venue.to_string(),
            CatalogRefreshVenueState::Done { at_ms: now_ms, outcome: outcome.clone() },
        );
        if outcome.adopted() {
            // Publish the WHOLE merged universe, not this venue's slice: the picker holds one
            // `Catalog` and adopting a slice would drop every other venue from it.
            //
            // ⚠ `universe()` rather than the cache alone, and the direction matters at exactly this
            // moment: a venue just refreshed now has a STAMP, so `merge_sources` stops contributing
            // the operator's rows for it — that list REPLACED by a fetched one, in one step.
            let _ = guard.tx.send(guard.universe());
        }
        outcome
    };
    match &outcome {
        CatalogRefreshOutcome::Refreshed { count, .. } => {
            tracing::info!("catalog refresh: {venue} → {count} instruments")
        }
        // A REFUSAL is `info`, not `warn`: an unarmed lane and a credentialed venue are correct
        // configurations, and logging them as faults is how a default reads as a problem.
        CatalogRefreshOutcome::Refused { why, .. } => {
            tracing::info!("catalog refresh: {venue}: {why}")
        }
        other => tracing::warn!("catalog refresh: {venue}: {}", other.line()),
    }
}
