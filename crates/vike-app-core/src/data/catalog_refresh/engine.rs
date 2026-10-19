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
    /// baseline lives under `vike_paths::state_path::PROJECT_BIN_DIR` because it ARRIVES — a
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
    /// screen that could explain it. `crate::data::md_session::MdSession::set_addr` exists for that
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
    /// verbatim rule and reason of `crate::data::md_session::MdSession::set_addr`: the second rung of
    /// `crate::data::datahub_resolve::resolve_datahub_addr` is the active backend's `Welcome`
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
    pub fn set_dial(&self, dial: Option<CatalogDial>) {
        *lock(&self.dial) = dial;
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
    pub fn rows(&self) -> Vec<VenueCatalogRow> {
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
        let state = lock(&self.state);
        merge_sources(&state.cache, &state.local, &state.baseline).len()
    }

    /// **The explicit refresh.** Refuses on [`refresh_block`]; otherwise marks the venue in flight
    /// and spawns ONE thread that fetches — locally through the linked provider, or over the wire
    /// through [`crate::data::catalog_wire::fetch_venue_catalog`] — folds through [`merge_refresh`],
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
            let mut state = lock(&self.state);
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
    /// nobody having asked — the exact poll `crate::data::catalog_wire`'s doc forbids, wearing a first
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
                                let mut g = lock(&state);
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
                            lock(&state).cache = cache;
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
        let mut guard = lock(state);
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
