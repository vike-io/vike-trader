//! The mount phases: the market-data plane, the backfill table, the seed/catalog/import lanes and
//! the execution stream.

/// **Phase: the live market-data plane** — `Some(hub)` only when an operator armed it with the
/// `flags.datahub_live` row; off is byte-identical to a build without the plane. `run` calls this once the listener is bound, with
/// the ONE broker both planes take their venue clients from.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_market_data_plane(
    settings: &vike_config::Settings,
    broker: &std::sync::Arc<crate::feeds::FeedBroker>,
) -> Option<std::sync::Arc<crate::market_data::MarketDataHub>> {
    use std::sync::Arc;

    // ⚠ **THE LIVE MARKET-DATA PLANE, ARMED BY AN OPERATOR AND NOT BY A BUILD** — the
    // `flags.datahub_live` row, and nothing else (decision 0111: `VIKE_DATAHUB_LIVE` refuses
    // startup naming that row).
    //
    // ⚠ Off is BYTE-IDENTICAL to a build without the plane: no hub is mounted, `served_features`
    // advertises neither `market_data` nor any `market_data_venue=` entry, and `MarketDataSubscribe` is answered
    // with `crate::server::NO_MARKET_DATA_PLANE` on a connection that stays positional. That is the
    // credential-is-the-gate idiom pointed at a CAPABILITY: the build carries it, the operator arms
    // it, and the two cannot answer differently because the advertisement keys on the MOUNT.
    if settings.flags.datahub_live {
        // ⚠ THE CREDENTIALED VENUES THIS PLANE MAY ATTACH TO, the `config.datahub_md_attach_venues` row
        // and nothing else (decision 0193, verdict 8): no row names none, whatever the build carries.
        let (credentialed, unopenable) = crate::market_data::venues::credentialed_from_row(
            settings.config.datahub_market_data_attach_venues.as_deref(),
        );
        for word in &unopenable {
            tracing::warn!(
                word = %word,
                "vike-datahub market data: ignoring a config.datahub_md_attach_venues entry — it names no \
                 credentialed venue whose one client the holders share"
            );
        }
        let served =
            crate::market_data::venues::supported().into_iter().chain(credentialed.iter().copied());
        let hub = crate::market_data::MarketDataHub::with_gate(
            crate::market_data::venues::market_builder(Arc::clone(broker), credentialed.clone()),
            served.map(str::to_string).collect(),
            crate::market_data::venues::attach_gate(Arc::clone(broker)),
        );
        // Tier R — the RESIDENT set, the `config.datahub_live_resident` row. An unparseable entry
        // is a WARNING that names the entry rather than a startup refusal, because a typo in one
        // resident key must not take the whole data wire down
        // (`docs/decisions/0013-degrade-vs-refuse.md`).
        let resident_raw = settings.config.datahub_live_resident.clone().unwrap_or_default();
        let (resident, bad) = crate::market_data::parse_resident_set(&resident_raw);
        for row in &bad {
            tracing::warn!(
                row = %row,
                "vike-datahub market data: ignoring an unparseable config.datahub_live_resident entry — \
                 the format is venue:symbol:lane with lane one of depth|book|trades|quotes"
            );
        }
        // ⚠ **A ROW THAT PARSES IS NOT A ROW THAT CAN BE SERVED**, and `parse_resident_set` checks
        // only the three-field shape and the lane word — `notavenue:X:depth` and a real venue on a
        // lane its declared caps do not serve both get through it. `add_resident` runs `acquire`'s
        // capability checks and the two key caps, and each refusal is named HERE for the same
        // reason the unparseable rows are: a resident key the daemon silently could not subscribe
        // is an endless 5-second reconcile-failure loop with no line saying which row caused it.
        let mut pinned = 0usize;
        for spec in &resident {
            match hub.add_resident(spec) {
                Ok(()) => {
                    pinned += 1;
                    // The operator named this key, so its hold may open the stream (rule nine of
                    // `crate::feeds`). Before `spawn`, so no window can have joined it first.
                    broker.pin_market_data_key(&spec.venue, &spec.symbol, spec.lane);
                }
                Err(why) => tracing::warn!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    lane = ?spec.lane,
                    %why,
                    "vike-datahub market data: REFUSING a config.datahub_live_resident entry — it is not \
                     pinned and nothing will retry it. Every other entry is unaffected"
                ),
            }
        }
        hub.spawn();
        tracing::info!(
            venues = ?hub.served_venues(),
            resident = pinned,
            "vike-datahub: LIVE MARKET-DATA plane armed (the flags.datahub_live row) — this \
             process is now the single subscriber to each venue it serves, and spends that venue's API budget from \
             this box's IP"
        );
        Some(hub)
    } else {
        None
    }
}

/// **Phase: the STRATEGY LANE** (wave 2 of `vike-gatewayhub`) - the tradehub's price path, armed by the
/// KEY and by nothing else: `Some(hub)` only on a KEYED datahub whose store holds the strategy-lane key
/// (`VIKE_DATAHUB_TRADE_KEY`, minted by `vike-cli backend mint-missing`), which is `HubKeys::has(Scope::Trade)`.
/// Absence of the key is the switch and it fails CLOSED: no key, no `Scope::Trade`, no hub, the verb is
/// refused (`crate::server::NO_STRATEGY_LANE`) and `Welcome` advertises nothing of it. `run` calls this
/// once the broker exists, with the ONE broker every plane takes its venue clients from, so a key the
/// lane holds that market data or the recorder also holds is subscribed once.
///
/// What it serves is this build's `crate::feeds::venues::strategy_venues`, and the tick pump is mounted
/// over the process's one gatewayhub link (`real_ticks_table`). A build that arms no venue mounts a lane
/// that serves none and refuses every `StrategyOpen` BY NAME, the way market data does (a real configuration, not an
/// oversight).
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_strategy_lane(
    keys: Option<&vike_hub_protocol::auth::HubKeys>,
    broker: &std::sync::Arc<crate::feeds::FeedBroker>,
    gateway: &vike_gatewayhub_client::client::GatewayLink,
) -> Option<std::sync::Arc<crate::strat::StrategyLane>> {
    if !keys.is_some_and(|k| k.has(vike_hub_protocol::auth::Scope::Trade)) {
        tracing::info!(
            "vike-datahub: strategy lane NOT mounted (this server holds no strategy-lane key): \
             `StrategyOpen` is refused and `strategy_lane` is not advertised"
        );
        return None;
    }
    let served: Vec<String> =
        crate::feeds::venues::strategy_venues().into_iter().map(str::to_string).collect();
    let hub = crate::strat::StrategyLane::new(
        std::sync::Arc::clone(broker),
        served,
        Some(crate::feeds::venues::real_ticks_table(gateway)),
        crate::strat::StrategyLaneLimits::PRODUCTION,
    );
    tracing::info!(
        venues = ?hub.served_venues(),
        "vike-datahub: STRATEGY LANE mounted (the strategy-lane key is held) - a Trade-scope client \
         may now open a price stream per venue; its keys draw on the lane's own caps and never on the \
         market-data plane's"
    );
    Some(hub)
}

/// **Phase: the START HISTORY** of the strategy lane (`crate::strat::history`'s module doc is the authority):
/// attach a holder that SAVES the closed bars of the keys the lane's sessions hold on a saved venue (OANDA so
/// far), compares the venue's own seed with the store (one journal line per subscription) and, only with
/// `flags.start_history_from_store`, preloads a bar subscription's start history from the store before the venue
/// is asked. `run` calls this once, after it knows whether THIS process compacts its store.
///
/// ⚠ **Armed only where the store is compacted.** A bar is one part until a maintenance pass merges them (about
/// fourteen hundred a day per key at the one-minute step), so a serve-only daemon, which runs no pass, would
/// grow the series without bound: it says so and saves nothing. The strategy lane is exactly what it was there.
///
/// ⚠ **The flag is read HERE and nowhere else** (`flags.start_history_from_store` is the needle
/// `vike_config::CONSUMPTION` finds in this file). Off, the holder saves and compares and delivers nothing
/// different to a strategy.
///
/// A build without `backfill-serve` has no kline ingest to write with, so it saves nothing and says so once at
/// start: a silent no-save would be indistinguishable from a venue that is merely quiet.
///
/// The FUNDING RECORDER (`crate::strat::funding_record`) is armed here too, on the same two conditions: it
/// writes one part per settlement through the funding backfill's ingest.
#[cfg(feature = "datafusion-serve")]
pub(super) fn arm_start_history(
    strat: Option<&std::sync::Arc<crate::strat::StrategyLane>>,
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    gateway: &vike_gatewayhub_client::client::GatewayLink,
    flags: &vike_config::Flags,
    compaction_runs: bool,
) -> Option<std::sync::Arc<crate::strat::history::StartHistory>> {
    let hub = strat?;
    let serve = flags.start_history_from_store;
    if !compaction_runs {
        tracing::info!(
            serve,
            "vike-datahub: start history NOT armed - this process runs no store maintenance (a recording \
             profile with maintenance does), and a closed bar is a part until a pass merges them"
        );
        return None;
    }
    #[cfg(feature = "backfill-serve")]
    {
        use std::sync::Arc;

        use crate::strat::funding_record::{FundingRateRecorder, store_writer};
        use crate::strat::history::{StartHistory, StartHistoryGapSourceGateway, ingest_writer};

        // The FUNDING RECORDER rides the same condition (a compacted store, a `backfill-serve` build): each
        // settlement the live funding lane reports for a held key goes into the funding series.
        match FundingRateRecorder::spawn(store_writer(Arc::clone(store))) {
            Ok(recorder) => {
                hub.attach_funding_recorder(recorder);
                tracing::info!(
                    "vike-datahub: FUNDING RECORDER armed - each settlement the live funding lane reports for a \
                     key the strategy lane holds is written into its `interval=funding` series"
                );
            }
            Err(e) => tracing::warn!(
                error = %e,
                "vike-datahub: the funding recorder could not start its worker thread - live funding \
                 settlements are NOT recorded"
            ),
        }
        let reader = Arc::clone(store) as Arc<dyn vike_data::HistoryStore + Send + Sync>;
        let gap = Arc::new(StartHistoryGapSourceGateway::new(gateway.clone()))
            as Arc<dyn crate::strat::history::StartHistoryGapSource>;
        match StartHistory::spawn(reader, ingest_writer(Arc::clone(store)), Some(gap), serve) {
            Ok(history) => {
                hub.attach_start_history(Arc::clone(&history));
                tracing::info!(
                    venues = ?crate::strat::history::SAVED_VENUES,
                    serve,
                    "vike-datahub: START HISTORY armed - the strategy lane saves the closed bars of the keys \
                     it holds on these venues; {}",
                    if serve {
                        "flags.start_history_from_store is ON: a bar subscription's history is read from the \
                         store first and only the missing span is fetched from the venue"
                    } else {
                        "strategies still get exactly what the venue seeds (the shadow only writes one \
                         comparison line per subscription)"
                    }
                );
                Some(history)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "vike-datahub: the start history could not start its worker thread - closed bars are NOT \
                     saved and the strategy lane is what it was"
                );
                None
            }
        }
    }
    #[cfg(not(feature = "backfill-serve"))]
    {
        let _ = (hub, store, gateway);
        tracing::warn!(
            serve,
            "vike-datahub: start history NOT armed - this build has no kline ingest to save closed bars \
             with (rebuild with `--features backfill-serve`); the strategy lane is what it was"
        );
        None
    }
}

/// Why the live-bar recorder records nothing on this process although `flags.record_bars` is on, or `None`
/// when it can record: it needs the strategy lane (the one path a live close takes through this daemon) and a
/// process that COMPACTS its store (each close is one part until a maintenance pass merges them).
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn bar_recorder_refusal(
    lane_mounted: bool,
    compaction_runs: bool,
) -> Option<&'static str> {
    if !lane_mounted {
        return Some(
            "the live-bar recorder (the flags.record_bars row, on unless a row says false) records \
             nothing here: this datahub mounts no strategy lane (it holds no strategy-lane key), and \
             that lane is the one path a live bar takes through it",
        );
    }
    (!compaction_runs).then_some(
        "the live-bar recorder (the flags.record_bars row, on unless a row says false) is NOT \
         started: this process runs no store maintenance, so the part each closed bar is would never \
         be compacted. Run this daemon with a recording profile whose [maintenance].interval_secs is \
         above 0 (`vike-datahub --recorder-profile <name>`) and restart, or switch the flag off \
         (`vike-cli config set flags.record_bars false`)",
    )
}

/// **Phase: the LIVE-BAR RECORDER** (`flags.record_bars`, ON by default: the owner's ruling of 2026-11-17).
/// Attaches a `vike_store::RecorderSink` of its own to the strategy lane, which hands it every CLOSED bar of a
/// key a session holds, after the sessions; it flushes them into the recorder's lane,
/// `kind=bar/venue=…/source=recorder/symbol=…/interval=…` (`crates/vike-store/src/live_rec.rs`'s module doc),
/// a side copy no strategy reads back. `flush` is the tape recorder's flush policy at the bars' own age
/// (`crate::recorder::bar_flush_policy`: `config.recorder_flush_max_rows` and
/// `config.recorder_bar_flush_max_age_secs`, an hour unset), so the closes are batched by the same code, one
/// commit key per flush. A `false` row attaches nothing, and the lane is what it was.
/// Returns the recorder's handle, whose drop at the orderly stop flushes the open buffers. `run` calls this
/// once, after it knows whether THIS process compacts its store.
#[cfg(feature = "datafusion-serve")]
pub(super) fn arm_bar_recorder(
    strat: Option<&std::sync::Arc<crate::strat::StrategyLane>>,
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    flags: &vike_config::Flags,
    compaction_runs: bool,
    flush: vike_store::RecorderConfig,
) -> Option<vike_store::RecorderHandle> {
    if !flags.record_bars {
        return None;
    }
    if let Some(why) = bar_recorder_refusal(strat.is_some(), compaction_runs) {
        // No lane is no live bar at all, so it is a statement and not a warning.
        if strat.is_some() {
            tracing::warn!("vike-datahub: {why}");
        } else {
            tracing::info!("vike-datahub: {why}");
        }
        return None;
    }
    let hub = strat?;
    match vike_store::RecorderSink::spawn(
        std::sync::Arc::clone(store),
        vike_store::RecorderConfig { record_bars: true, ..flush },
    ) {
        Ok((sink, handle)) => {
            hub.attach_bar_recorder(sink);
            tracing::info!(
                "vike-datahub: LIVE-BAR RECORDER armed (the flags.record_bars row, on unless a row says \
                 false) - each closed bar the strategy lane hands a strategy is also written into the \
                 store's `source=recorder` lane, which no strategy reads"
            );
            Some(handle)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "vike-datahub: the live-bar recorder could not start its writer thread - closed live \
                 bars are NOT recorded and the strategy lane is what it was"
            );
            None
        }
    }
}

/// **Phase: the backfill table** — the REAL collector table over the SAME store handle the server
/// serves, fetching through the gatewayhub (`gateway`: the one link every remote lane of this
/// process dials from). `run` calls this only in a `backfill-serve` build; any other build binds
/// `backfill` to `None` itself.
///
/// ⚠ **The OANDA practice token is NOT read here, or anywhere in this process** (decision 0164:
/// the gatewayhub makes every venue call; 0097's one read is the gatewayhub's now): it holds the
/// read, per connection, and logs the "oanda history lane: ARMED / not armed / UNKNOWN" startup
/// line itself. A box that stores no key starts and serves every verb exactly as before; only a
/// `Backfill` for oanda is refused, with the gatewayhub's own words. The history-channels read's
/// presence probe for that row is a question to the gatewayhub, a word per request, never a value
/// (decision 0102); `real_backfill_table` attaches it.
#[cfg(feature = "backfill-serve")]
pub(super) fn start_backfill(
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    gateway: &vike_gatewayhub_client::client::GatewayLink,
) -> Option<crate::backfill::BackfillTable> {
    // Backfill-on-demand (split-plane REQ-9): a `backfill-serve` build mounts the REAL collector
    // table over the SAME store handle the server serves, which is what makes `serve` advertise
    // and answer `Request::Backfill`; any other build mounts none and the verb is a clean refusal.
    Some(crate::backfill::real_backfill_table(std::sync::Arc::clone(store), gateway.clone()))
}

/// **Phase: the chart-gap seed lane** — armed by an operator with the `flags.datahub_chart_seed`
/// row, and only on a build that mounted a collector table (`backfill` says whether it did). `run` calls this once, right after the
/// backfill table is bound.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_seed_lane(
    flags: &vike_config::Flags,
    backfill: Option<&crate::backfill::BackfillTable>,
) -> Option<std::sync::Arc<crate::seed::SeedLane>> {
    // ⚠ **THE CHART-GAP SEED LANE, ARMED BY AN OPERATOR AND NOT BY A BUILD** — the
    // `flags.datahub_chart_seed` row, the same idiom as the market-data arm above (decision 0111:
    // `VIKE_DATAHUB_CHART_SEED` refuses startup naming that row).
    //
    // ⚠ The lane's mere EXISTENCE is the arming: `crate::server`'s `served_features` keys
    // `FEATURE_SEED_SERIES` on `Option::is_some`, so there is no second switch for the
    // advertisement to disagree with. And an UNARMED daemon still answers the verb — successfully,
    // having written nothing — which is the leg that makes a WRITE verb's `VerbScope::Read`
    // classification honest. `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` puts
    // removing this switch, or defaulting it ON, in its reopen list.
    if flags.datahub_chart_seed {
        if backfill.is_none() {
            // Armed on a build with no collectors: the lane would advertise a capability it can
            // only refuse. Say so ONCE at startup rather than once per request, and arm nothing —
            // `docs/decisions/0013-degrade-vs-refuse.md`: a capability degrades, it does not refuse
            // to start.
            tracing::warn!(
                "vike-datahub: the chart-seed lane is switched on (the \
                 flags.datahub_chart_seed row) but this build carries no \
                 collector table — the chart-seed lane is NOT armed and `seed_series` \
                 is not advertised. Rebuild with `--features backfill-serve`"
            );
            None
        } else {
            tracing::info!(
                "vike-datahub: CHART-GAP SEED lane armed (the \
                 flags.datahub_chart_seed row) — an \
                 OBSERVE-scope client may now have this process fetch one bounded kline \
                 window per series into its store, spending this box's venue-API budget. \
                 Every bound is this server's; see crates/vike-datahub/src/seed.rs"
            );
            Some(std::sync::Arc::new(crate::seed::SeedLane::new()))
        }
    } else {
        None
    }
}

/// **Phase: the venue-catalog lane** — on by default, and the REFUSAL (`flags.venue_catalog_off`) is
/// the only switch; the verdict is `vike_catalog::venue_catalog_gate`, logged either way. `run`
/// calls this once, after the seed lane. The `venue_catalog_gate(booted.settings.flags.venue_catalog_off)` call is
/// the needle `vike_config::CONSUMPTION`'s row for that key reads in this file. `gateway` is the one
/// link every remote lane dials from (`super::gateway`): the table's providers are the gatewayhub's.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_catalog_lane(
    booted: &vike_boot::Booted,
    gateway: &vike_gatewayhub_client::client::GatewayLink,
) -> Option<std::sync::Arc<crate::catalog::CatalogLane>> {
    // ⚠ **THE VENUE-CATALOG LANE, SWITCHED BY A SETTINGS ROW AND NOT BY A BUILD** — on by default,
    // and the `flags.venue_catalog_off` row is its one switch, the same idiom as the two arms above.
    //
    // ⚠ **A build feature could NOT be the arming**: the image ships every feature
    // (`docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md`), so a feature
    // would be on wherever the image is the install, which is exactly where it matters most. The switch has
    // to be runtime.
    //
    // ⚠ **THE DEFAULT FLIPPED on 2026-09-16 and this block used to read the opposite way.** It was
    // `vars.get("VIKE_DATAHUB_VENUE_CATALOG") == Some("1")`, default OFF, and
    // `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`
    // turned it round: without an instrument list you cannot pick a symbol, so a switch that is off
    // by default means the product does not work out of the box — and on the primary install the
    // operator never sees the variable's name at all, because every surface that would have taught
    // it is a surface the switch turns off.
    //
    // The REFUSAL is what survives, as a settings key rather than a variable
    // (`vike_config::Flags::venue_catalog_off`), because
    // `crates/vike-config/tests/flag_registry.rs`'s `every_flag_defaults_off` refuses a new default-ON
    // positive flag in that type (it names one exception, `record_properties`). The verdict is
    // `vike_catalog::venue_catalog_gate`, which lives one crate below every consumer so no second
    // root can word it differently — the `reconcile_gate` shape, copied deliberately.
    //
    // `flags.hyperliquid_hip3` — the same resolved row the trading daemon's hyperliquid mount folds
    // (decision 0095). ⚠ It is handed to the table but does NOT reach the gatewayhub, whose catalog
    // verbs carry no such field: the daemon lists hyperliquid under its own resolved flag, and
    // `crate::catalog::real_catalog_table` says so in the log when this one is on.
    let catalog_table =
        crate::catalog::real_catalog_table(gateway, booted.settings.flags.hyperliquid_hip3);
    // ⚠ This is the site `crates/vike-config/src/consumed.rs`'s row for `flags.venue_catalog_off`
    // names, needle and all, and it must stay an ARGUMENT to the gate rather than a local bound
    // first: a `Consumption` row proves a key is READ, and a value assigned and never passed is
    // exactly the declared-but-unconsumed shape that table exists to catch.
    let catalog_gate = vike_catalog::venue_catalog_gate(booted.settings.flags.venue_catalog_off);
    {
        // Both answers are news — the gate's module doc, property 2. The REFUSAL is `warn!`
        // because the operator of a box whose symbol picker will be empty has to be able to find
        // out why from the log alone; the served verdict is `info!` because it is the ordinary
        // state.
        let line = vike_catalog::venue_catalog_gate_line(
            catalog_gate,
            &catalog_table.supported().join(", "),
        );
        match catalog_gate {
            vike_catalog::CatalogVenueGate::Armed => tracing::info!("vike-datahub: {line}"),
            _ => tracing::warn!("vike-datahub: {line}"),
        }
    }
    // ⚠ **The memo's TTL is the one cost term of this lane an operator may move**, and it arrives
    // here as a PARAMETER of the lane (`crate::catalog` reads no settings): the
    // `config.datahub_catalog_ttl_secs` row, which `vike-config` refuses outside its range at the
    // write and at the load, or `CATALOG_TTL` for a box that wrote none — byte-identical to the lane
    // before the row existed. The per-venue bucket is NOT this row: how fast a venue can be asked at
    // all stays `CATALOG_VENUE_BURST` and `CATALOG_VENUE_REFILL`.
    let ttl = crate::catalog::catalog_ttl(booted.settings.config.datahub_catalog_ttl_secs);
    if booted.settings.config.datahub_catalog_ttl_secs.is_some() {
        tracing::info!(
            ttl_secs = ttl.as_secs(),
            "vike-datahub: the venue-catalog memo TTL is the config.datahub_catalog_ttl_secs row, \
             not the six-hour default"
        );
    }
    catalog_gate
        .serves()
        .then(|| std::sync::Arc::new(crate::catalog::CatalogLane::new(catalog_table, ttl)))
}

/// **Phase: the `kind=properties` writer** (`crate::properties_tape`'s module doc) — the
/// `flags.record_properties` row, ON by default (the owner's ruling of 2026-11-09); a `false` row
/// starts nothing. It writes through the SAME store
/// handle the server serves and lists through the catalog lane, so `run` calls this after that lane
/// exists; with the catalog refused there is nothing to list, and it says so. The read below is the
/// needle `vike_config::CONSUMPTION`'s row for that key reads in this file.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_properties_tape(
    booted: &vike_boot::Booted,
    catalog: Option<&std::sync::Arc<crate::catalog::CatalogLane>>,
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
) {
    if !booted.settings.flags.record_properties {
        return;
    }
    let Some(lane) = catalog else {
        tracing::warn!(
            "vike-datahub: flags.record_properties is on (the default) and the \
             flags.venue_catalog_off row refuses the venue catalog it reads, so NO instrument grid \
             is recorded; a false flags.record_properties row silences this"
        );
        return;
    };
    match crate::properties_tape::start(
        std::sync::Arc::clone(lane),
        std::sync::Arc::clone(store) as _,
    ) {
        Ok(()) => tracing::info!(
            "vike-datahub: recording instrument grids (flags.record_properties, on by default): one \
             row a day per symbol this store holds prices for, off the venue catalog through the \
             gatewayhub"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "vike-datahub: the instrument-grid recorder could not start its thread, so NO grid is recorded"
        ),
    }
}

/// **Phase: the catalog warm-up** — ONCE, in the background, as soon as the daemon listens: the
/// store's inventory and coverage report, so their memo (`vike-store`'s `CoverageMemo`) holds every
/// series before the first client asks. Without it the first Data Manager, Tearsheet or picker read
/// after a start walked every manifest itself, 3 to 5 s on the CI box before its first reply byte. Not a
/// timer: every later answer re-reads only what a write moved. A request that lands mid-warm-up
/// waits on the memo and then answers from it.
#[cfg(feature = "datafusion-serve")]
pub(super) fn warm_catalog(store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>) {
    let store = std::sync::Arc::clone(store);
    let spawned = std::thread::Builder::new().name("catalog-warm".into()).spawn(move || {
        let started = std::time::Instant::now();
        match store.inventory().and_then(|series| Ok((series.len(), store.coverage_report()?.len())))
        {
            Ok((series, instruments)) => tracing::info!(
                series,
                instruments,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "vike-datahub: catalog warmed (the inventory and the coverage report)"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "vike-datahub: the catalog warm-up failed; the first inventory walks the store itself"
            ),
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "vike-datahub: no thread for the catalog warm-up");
    }
}

/// **Phase: the archive import lane** — mounted by the PROJECT, never by an operator switch.
/// `run` calls this once, after the catalog lane, with the store handle the registry's one format
/// folds in. The `store` parameter is read only by a `backfill-serve` build.
#[cfg(feature = "datafusion-serve")]
#[cfg_attr(not(feature = "backfill-serve"), allow(unused_variables, clippy::allow_attributes))]
pub(super) fn start_import_lane(
    booted: &vike_boot::Booted,
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
) -> Option<std::sync::Arc<crate::import::ImportLane>> {
    use std::sync::Arc;

    // ⚠ **THE ARCHIVE IMPORT LANE — mounted by the PROJECT, not by an operator switch**
    // (`docs/decisions/0100`, verdicts 1 and 2). Its root is `<project>/market_data/imports`,
    // resolved ONCE here from this boot's settings directory (`imports_dir_beside`, inside
    // `crate::import::mount`) — no variable and no settings key, so `vike_config::settings::SETTINGS`
    // and `vike_config::CONSUMPTION` gain no row. A daemon with no project above it mounts none, and
    // a build without `backfill-serve` carries no format and mounts none; either way the verb is
    // refused by name and `archive_import` is not advertised.
    //
    // ⚠ It is ARMED by default where it can be, unlike the market-data and chart-seed lanes above,
    // and that follows from what it spends: nothing on the network and nothing of an operator's —
    // it reads only files the operator put under this root, it is Control-scoped on a keyed server,
    // and every input whose meaning is uncertain is refused before a key is spent (0100's verdict
    // 1). An empty root costs nothing at all.
    #[cfg(feature = "backfill-serve")]
    let import_formats = crate::import::formats::real_import_registry(
        Arc::clone(store),
        vike_paths::state_path::instruments_dir_beside(booted.settings_dir.as_deref()),
    );
    #[cfg(not(feature = "backfill-serve"))]
    let import_formats: Vec<Box<dyn crate::import::ArchiveFormat>> = Vec::new();
    let (import_lane, import_line) =
        crate::import::mount(booted.settings_dir.as_deref(), import_formats);
    tracing::info!("vike-datahub: {import_line}");
    // ⚠ **THE CSV UPLOAD, ARMED BY AN OPERATOR AND NOT BY THE PROJECT** — the
    // `flags.datahub_csv_upload` row, off by default (decision 0272). Unlike the import above it is an
    // OBSERVE-scope write, which decision 0058's third leg makes honest only while an operator arms it:
    // unarmed, `csv_upload` is not advertised and the verb is refused having read nothing.
    let armed = booted.settings.flags.datahub_csv_upload;
    let import_lane = import_lane.map(|lane| lane.with_upload_armed(armed));
    match &import_lane {
        Some(lane) if armed && lane.upload_format().is_some() => tracing::info!(
            "vike-datahub: CSV UPLOAD armed (the flags.datahub_csv_upload row): an OBSERVE-scope \
             client may send a user's CSV into a NEW user-<label> series of this store; never a \
             venue's series, never one holding rows. See crates/vike-datahub/src/import/upload.rs"
        ),
        _ if armed => tracing::warn!(
            "vike-datahub: the CSV upload is switched on (the flags.datahub_csv_upload row) but \
             this daemon mounts no import lane carrying csv-bars — the upload is NOT armed and \
             `csv_upload` is not advertised"
        ),
        _ => {}
    }
    #[cfg(feature = "backfill-serve")]
    let import_lane = import_lane.map(|lane| with_archive_download(booted, lane));
    #[cfg(feature = "backfill-serve")]
    let import_lane = import_lane.map(|lane| with_sec_downloads(booted, lane));
    import_lane.map(Arc::new)
}

/// **The SEC EDGAR downloads on the import lane** (decision 0100's amendment of 2026-11-16): the filings
/// and the XBRL values of one company per request, at most ten requests a second, each declaring the
/// box's `config.datahub_sec_user_agent` row. Mounted on every lane; a box without the row refuses each
/// request naming it (`crate::import::sec_download`'s module doc), and says so here once.
#[cfg(feature = "backfill-serve")]
fn with_sec_downloads(
    booted: &vike_boot::Booted,
    mut lane: crate::import::ImportLane,
) -> crate::import::ImportLane {
    use crate::import::sec_download::{SEC_DATA, SecHttpUreq, sec_downloads};
    let user_agent = booted.settings.config.datahub_sec_user_agent.clone();
    if user_agent.is_some() {
        tracing::info!(
            "vike-datahub: SEC EDGAR DOWNLOAD mounted on the import lane — FetchArchive sec-filings / \
             sec-companyfacts fetch from SEC under this box's config.datahub_sec_user_agent, at most \
             ten requests a second"
        );
    } else {
        tracing::info!(
            "vike-datahub: SEC EDGAR download mounted WITHOUT a declared User-Agent — every SEC \
             FetchArchive is refused until config.datahub_sec_user_agent names you and a contact \
             email (SEC's fair-access rule; no value ships)"
        );
    }
    let http = std::sync::Arc::new(SecHttpUreq::default());
    let www = vike_backfill::sec_edgar::texts::EDGAR_WWW;
    for download in sec_downloads(user_agent, http, www, SEC_DATA) {
        lane = lane.with_download(Box::new(download));
    }
    lane
}

/// **The archive DOWNLOAD on the import lane** (the owner's ruling of 2026-11-13): the bucket from this
/// box's `venue_url.dukascopy.live.history.rest.archive` row, read once here, and the operator's AWS
/// key pair read from this box's credential store on EVERY request through the one-name scoped
/// reader, so this process holds no key between requests (`crate::import::download`'s module doc). A
/// box whose row is missing or unreadable mounts the lane without it and says why, once.
#[cfg(feature = "backfill-serve")]
fn with_archive_download(
    booted: &vike_boot::Booted,
    lane: crate::import::ImportLane,
) -> crate::import::ImportLane {
    use crate::import::download::{ACCESS_KEY_ID_NAME, SECRET_ACCESS_KEY_NAME, mount};
    let Some(settings_dir) = booted.settings_dir.as_deref() else { return lane };
    let keys =
        std::sync::Arc::new(crate::tool_feeds::ToolFeedKeyReaderStore::new(booted.store.clone()));
    match mount(settings_dir, keys) {
        Ok(download) => {
            tracing::info!(
                "vike-datahub: ARCHIVE DOWNLOAD mounted on the import lane — FetchArchive (Control \
                 scope) lists and downloads Dukascopy's Requester-Pays bucket into the imports root \
                 with the AWS key stored on THIS box (`vike-cli secrets set {ACCESS_KEY_ID_NAME}` \
                 and `{SECRET_ACCESS_KEY_NAME}`), read per request and billed to its account"
            );
            lane.with_download(Box::new(download))
        }
        Err(why) => {
            tracing::warn!(
                "vike-datahub: Dukascopy's archive download is NOT mounted ({why}); a FetchArchive \
                 of dukascopy-bi5 is refused by name"
            );
            lane
        }
    }
}

/// **Phase: the options lane** — mounted by the BUILD (`options-serve`), with no runtime switch, and
/// carrying the operator's `kind=chain` recorder when the `flags.record_chains` row is on AND this
/// process compacts the store. `run` calls this once, after the import lane, with the store handle
/// the recorder writes through and whether a maintenance scheduler will run (`compaction_runs`).
/// The `booted.settings.flags.record_chains` read below is the needle `vike_config::CONSUMPTION`'s
/// row for that key reads in this file.
///
/// ⚠ **The recorder moved HERE from the desktop** (`docs/decisions/0084`: only the datahub touches
/// the store). The desktop's Options tool still carried the `ChainRecorder` seam after its store
/// left it, so the row armed nothing anywhere; this daemon holds the store AND, now, the chain poll.
/// It writes through the SAME store handle the server serves (`ChainRecorder::new` over it), never
/// a second store opened at a root this daemon already owns. OFF by default (the row defaults off),
/// and when armed it STARTS the lane at mount rather than on the first request
/// (`crate::options::gateway`'s module doc).
///
/// ⚠ **On, with no compaction, it is REFUSED** ([`chain_recorder_arming`]): every snapshot is one
/// store part, and only a recording profile's maintenance scheduler compacts them.
///
/// ⚠ **Four more rows tune the lane, and this is the one place they are read**: the chain recorder's
/// snapshot cadence (`config.datahub_chain_cadence_secs`), the REST poll interval
/// (`config.datahub_options_poll_secs`), and the ticker feed's focus
/// (`config.datahub_options_focus_underlyings`, `config.datahub_options_focus_strikes`, the strikes'
/// default `vike_config::config`'s). `crate::options::gateway` reads no settings; each value
/// reaches it as a parameter, and an unset row is the compiled-in value, so a box without them is
/// byte-identical to the lane before they existed — EXCEPT the focus underlyings, which have no
/// compiled value (owner, 2026-11-13): unset, the ticker feed streams nothing and the mount says so.
///
/// `gateway` is the dial to the gatewayhub (`super::gateway`'s `gateway_link`): the lane fetches its
/// chains, its option index and its two feeds through it and opens no venue connection of its own.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_options_lane(
    booted: &vike_boot::Booted,
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    compaction_runs: bool,
    gateway: &vike_gatewayhub_client::client::GatewayLink,
) -> Option<std::sync::Arc<crate::options::OptionsLane>> {
    options_lane(
        booted.settings.flags.record_chains,
        compaction_runs,
        store,
        booted.settings.config.datahub_chain_cadence_secs,
        booted.settings.config.datahub_options_poll_secs,
        booted.settings.config.datahub_options_focus_underlyings.as_deref(),
        booted.settings.config.datahub_options_focus_strikes,
        gateway,
    )
}

/// The chain recorder's arming verdict — a pure function of the row and of whether this process
/// compacts its store, so the decision is tested without a lane (`options_chain_recorder`, an
/// `options-serve` item, acts on it). The `allow` is widened to every build in which no `options-serve`
/// serving `run` calls it.
#[cfg_attr(
    not(all(feature = "datafusion-serve", feature = "options-serve")),
    allow(dead_code, clippy::allow_attributes)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChainRecorderArming {
    /// `flags.record_chains` is off — the default. No recorder; the lane is unchanged.
    Off,
    /// On, and a maintenance scheduler compacts the store: the recorder is built.
    Armed,
    /// On, and NOTHING compacts the store (no recording profile, or one whose
    /// `[maintenance].interval_secs` is 0): refused with a warning, no recorder, and the lane
    /// still serves chains. `docs/decisions/0013-degrade-vs-refuse.md`: an off-path writer
    /// degrades, it does not refuse the data wire — and recording without compaction is not a
    /// recording an operator can keep, at up to 24 uncompacted parts a minute.
    RefusedNoCompaction,
}

/// **The arming decision.** `record_chains` is the `flags.record_chains` row as this boot resolved
/// it; `compaction_runs` is whether this process starts a maintenance scheduler (`run`'s answer).
#[cfg_attr(
    not(all(feature = "datafusion-serve", feature = "options-serve")),
    allow(dead_code, clippy::allow_attributes)
)]
pub(super) fn chain_recorder_arming(
    record_chains: bool,
    compaction_runs: bool,
) -> ChainRecorderArming {
    match (record_chains, compaction_runs) {
        (false, _) => ChainRecorderArming::Off,
        (true, true) => ChainRecorderArming::Armed,
        (true, false) => ChainRecorderArming::RefusedNoCompaction,
    }
}

/// The startup line the refusal owes an operator — named so a test can hold it to naming the row,
/// the missing prerequisite and both fixes.
#[cfg_attr(
    not(all(feature = "datafusion-serve", feature = "options-serve")),
    allow(dead_code, clippy::allow_attributes)
)]
pub(super) fn chain_recorder_refusal_line() -> &'static str {
    "the option-chain RECORDER is switched on (the flags.record_chains row) but this process runs \
     no store maintenance, so the kind=chain parts it would write (one per expiry per minute) \
     would never be compacted — it is NOT started; the options lane still serves chains. Run this \
     daemon with a recording profile whose [maintenance].interval_secs is above 0 \
     (`vike-datahub --recorder-profile <name>`; its maintenance compacts the whole store) and \
     restart, or switch the flag off (`vike-cli config set flags.record_chains false`)"
}

/// The recorder `options_lane` hands the lane: built only when [`chain_recorder_arming`] says
/// [`ChainRecorderArming::Armed`], the refusal logged once otherwise.
#[cfg(feature = "options-serve")]
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn options_chain_recorder(
    record_chains: bool,
    compaction_runs: bool,
    store: std::sync::Arc<dyn vike_data::HistoryStore + Send + Sync>,
    cadence_ms: i64,
) -> Option<std::sync::Arc<vike_data::ChainRecorder>> {
    let arming = chain_recorder_arming(record_chains, compaction_runs);
    if arming == ChainRecorderArming::RefusedNoCompaction {
        tracing::warn!("vike-datahub: {}", chain_recorder_refusal_line());
    }
    crate::options::gateway::chain_recorder(arming == ChainRecorderArming::Armed, store, cadence_ms)
}

/// [`start_options_lane`] on an `options-serve` build: the real lane, with the recorder when armed.
/// `chain_cadence_secs`, `poll_secs`, `focus_underlyings` and `focus_strikes` are the four tuning
/// rows, still unresolved: `None` is the compiled-in value (`crate::options::gateway::chain_cadence_ms`,
/// `poll_interval` and `OptionsFocus::from_rows`), and for `focus_underlyings` no underlying.
/// `gateway` is the lane's dial to the gatewayhub.
#[cfg(all(feature = "datafusion-serve", feature = "options-serve"))]
fn options_lane(
    record_chains: bool,
    compaction_runs: bool,
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    chain_cadence_secs: Option<u32>,
    poll_secs: Option<u32>,
    focus_underlyings: Option<&str>,
    focus_strikes: Option<u32>,
    gateway: &vike_gatewayhub_client::client::GatewayLink,
) -> Option<std::sync::Arc<crate::options::OptionsLane>> {
    use crate::options::gateway::{OptionsFocus, chain_cadence_ms, poll_interval};
    use std::sync::Arc;

    let cadence_ms = chain_cadence_ms(chain_cadence_secs);
    let poll = poll_interval(poll_secs);
    // The loader refused a row this cannot read, so the error arm is a store changed under the boot.
    let focus = OptionsFocus::from_rows(focus_underlyings, focus_strikes).unwrap_or_else(|why| {
        tracing::error!(
            "vike-datahub options: the config.datahub_options_focus_* rows do not read ({why}); \
             the ticker feed streams nothing"
        );
        OptionsFocus::default()
    });
    // The list has no compiled default (owner, 2026-11-13: no hardcoded symbols), so an unset row
    // is said once rather than left to look like a quiet feed.
    if focus.underlyings.is_empty() {
        tracing::info!(
            "vike-datahub options: the config.datahub_options_focus_underlyings row names no \
             underlying, so the ticker feed streams NO live bid/ask (the chains are still polled). \
             Name them with `vike-cli config set config.datahub_options_focus_underlyings \
             <comma list>` and restart"
        );
    } else {
        tracing::info!(
            underlyings = %focus.underlyings.join(","),
            strikes = focus.strikes,
            "vike-datahub options: the ticker feed's focus is the config.datahub_options_focus_* \
             rows"
        );
    }
    let chain_rec =
        options_chain_recorder(record_chains, compaction_runs, Arc::clone(store) as _, cadence_ms);
    // A tuning row is disclosed when it is in force, and a row that nothing reads in the box's
    // current state is said so rather than left to look configured (`config show` cannot know
    // whether the recorder armed).
    if poll_secs.is_some() {
        tracing::info!(
            poll_secs = poll.as_secs(),
            "vike-datahub options: the REST poll interval is the config.datahub_options_poll_secs \
             row, not the default — every poll spends the venue's public budget from this box's IP"
        );
    }
    if chain_cadence_secs.is_some() {
        if chain_rec.is_none() {
            tracing::warn!(
                "vike-datahub options: config.datahub_chain_cadence_secs is set but the chain \
                 recorder is not running (the flags.record_chains row is off, or refused), so the \
                 row is read by nothing"
            );
        } else if u128::from(cadence_ms.unsigned_abs()) < poll.as_millis() {
            tracing::warn!(
                cadence_secs = cadence_ms / 1_000,
                poll_secs = poll.as_secs(),
                "vike-datahub options: the chain cadence is shorter than the poll interval, so \
                 every poll lands a snapshot — the effective interval is the poll's"
            );
        }
    }
    tracing::info!(
        "vike-datahub: OPTIONS lane mounted (the venue, its underlyings and their indexes are the \
             gatewayhub's listing; it polls nothing until the first OptionsChain request{}) — \
             clients ask this process for option chains and open no venue socket of their own",
        if chain_rec.is_some() {
            " — EXCEPT that the flags.record_chains row is on, so it starts now and records \
                 every polled chain into kind=chain"
        } else {
            ""
        }
    );
    Some(Arc::new(crate::options::gateway::real_options_lane(
        chain_rec,
        poll,
        focus,
        gateway.clone(),
    )))
}

/// [`start_options_lane`] on a build without the lane: nothing to mount, said once.
#[cfg(all(feature = "datafusion-serve", not(feature = "options-serve")))]
fn options_lane(
    record_chains: bool,
    _compaction_runs: bool,
    _store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    chain_cadence_secs: Option<u32>,
    poll_secs: Option<u32>,
    focus_underlyings: Option<&str>,
    focus_strikes: Option<u32>,
    _gateway: &vike_gatewayhub_client::client::GatewayLink,
) -> Option<std::sync::Arc<crate::options::OptionsLane>> {
    if record_chains {
        tracing::warn!(
            "vike-datahub: the flags.record_chains row is on, but this build carries no \
                 options lane (`--features options-serve`), so NO option chain is polled or \
                 recorded"
        );
    }
    if chain_cadence_secs.is_some()
        || poll_secs.is_some()
        || focus_underlyings.is_some()
        || focus_strikes.is_some()
    {
        tracing::warn!(
            "vike-datahub: a config.datahub_chain_cadence_secs, config.datahub_options_poll_secs \
             or config.datahub_options_focus_* row is set, but this build carries no options lane \
             (`--features options-serve`), so the row is read by nothing"
        );
    }
    tracing::info!(
        "vike-datahub: no OPTIONS lane in this build — OptionsChain answers NotArmed. Rebuild \
             with `--features options-serve` to serve option chains"
    );
    None
}

/// **Phase: the tool-feeds lane** — mounted by the BUILD (`tool-feeds-serve`), with no runtime
/// switch, exactly as the options lane is. `run` calls this once, after the options lane. The lane
/// polls nothing until the first `NewsFeed` / `EconomicCalendar` / `EquityCalendar` request, and its
/// two keyed feeds read `FINNHUB_API_KEY` / `FMP_API_KEY` from THIS box's credential store (the
/// boot's handle, over the settings directory this boot resolved), one scoped read per poll cycle —
/// so a key stored after start is picked up within the minute, and removing it disarms the feed.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_tool_feeds(
    booted: &vike_boot::Booted,
    cadences: crate::tool_feeds::ToolFeedCadences,
) -> Option<std::sync::Arc<crate::tool_feeds::ToolFeedLane>> {
    tool_feeds_lane(&booted.store, cadences)
}

/// **Phase: the tool-feed cadences** — the box's `config.datahub_news_poll_secs`,
/// `config.datahub_calendar_poll_secs` and `config.datahub_equity_poll_secs` rows as the lane's
/// [`Cadences`](crate::tool_feeds::ToolFeedCadences); an absent row is that feed's default, so a box without
/// the rows polls exactly as before. `Err` carries the exit status (`vike_utility::cli::OPERATOR_NEEDED_EXIT`:
/// a refused CONFIGURATION, parked rather than restarted).
/// `run` calls this BEFORE the bind guard, beside the other row parses, so a refusal is never a
/// crash-loop behind a bound port.
///
/// The rows were already refused outside 60 to 86400 seconds at the write and at the load
/// (`vike_config::Config::apply`); `Cadences::from_rows` refuses them again, which is the lane's own
/// guard against a zero cadence and fires on no row a loader accepted.
#[cfg(feature = "datafusion-serve")]
pub(super) fn load_feed_cadences(
    booted: &vike_boot::Booted,
) -> Result<crate::tool_feeds::ToolFeedCadences, std::process::ExitCode> {
    match crate::tool_feeds::ToolFeedCadences::from_rows(
        booted.settings.config.datahub_news_poll_secs,
        booted.settings.config.datahub_calendar_poll_secs,
        booted.settings.config.datahub_equity_poll_secs,
    ) {
        Ok(cadences) => Ok(cadences),
        Err(e) => {
            tracing::error!(
                error = %e,
                "vike-datahub: refusing to start — a tool-feed cadence row was refused before \
                 anything was opened or bound, so nothing on this box changed"
            );
            eprintln!("vike-datahub: {e}");
            Err(std::process::ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT))
        }
    }
}

/// **Phase: the text recorder** (`crate::tool_feeds::record`'s module doc) — the `flags.record_texts`
/// row, OFF by default: on, it stores every new news item and every new or revised calendar event into
/// `kind=text`, through the SAME store handle the server serves, asking the tool-feeds lane like a client
/// (so the news and the calendar are polled from now on, never lazily). `run` calls this once, after the
/// tool-feeds lane exists and `compaction_runs` is known. The read below is the needle
/// `vike_config::CONSUMPTION`'s row for that key reads in this file.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_text_recorder(
    booted: &vike_boot::Booted,
    lane: Option<&std::sync::Arc<crate::tool_feeds::ToolFeedLane>>,
    store: &std::sync::Arc<vike_store::HistoryStoreDataFusion>,
    compaction_runs: bool,
    write_floor: std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync>,
) {
    let arming =
        text_recorder_arming(booted.settings.flags.record_texts, compaction_runs, lane.is_some());
    let lane = match (arming, lane) {
        (Ok(false), _) => return,
        (Err(line), _) => {
            tracing::warn!("vike-datahub: {line}");
            return;
        }
        (Ok(true), Some(lane)) => std::sync::Arc::clone(lane),
        (Ok(true), None) => return, // unreachable: the arming needs the lane
    };
    match crate::tool_feeds::record::start(lane, std::sync::Arc::clone(store) as _, write_floor) {
        Ok(()) => tracing::info!(
            "vike-datahub: RECORDING the news and the economic calendar into kind=text (the \
             flags.record_texts row): every new item and every revised event, with the time this box \
             received it; the two feeds are polled from now on whether or not a client asks"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "vike-datahub: the text recorder could not start its thread, so NOTHING is recorded"
        ),
    }
}

/// **The text recorder's arming verdict**, a pure function of the row, of whether this process compacts its
/// store and of whether this build mounted the tool-feeds lane: `Ok(false)` off (the default), `Ok(true)`
/// armed, `Err` the startup line a refusal owes the operator (it names the row and both fixes). Without
/// compaction it is refused for the chain recorder's reason (`chain_recorder_arming`): every new snapshot is
/// one store part, a few hundred a day, and only a recording profile's maintenance merges them.
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn text_recorder_arming(
    record_texts: bool,
    compaction_runs: bool,
    lane_mounted: bool,
) -> Result<bool, &'static str> {
    match (record_texts, compaction_runs, lane_mounted) {
        (false, _, _) => Ok(false),
        (true, false, _) => Err("the news and calendar RECORDER is switched on (the \
             flags.record_texts row) but this process runs no store maintenance, so the kind=text \
             parts it would write (one per new snapshot of a feed) would never be compacted — it is \
             NOT started. Run this daemon with a recording profile whose \
             [maintenance].interval_secs is above 0 (`vike-datahub --recorder-profile <name>`) and \
             restart, or switch the flag off (`vike-cli config set flags.record_texts false`)"),
        (true, true, false) => Err("the news and calendar RECORDER is switched on (the \
             flags.record_texts row) but this build carries no tool-feeds lane (`--features \
             tool-feeds-serve`), so NOTHING is recorded; switch the flag off (`vike-cli config set \
             flags.record_texts false`) or run a build with the lane"),
        (true, true, true) => Ok(true),
    }
}

/// [`start_tool_feeds`] on a `tool-feeds-serve` build: the real lane.
#[cfg(all(feature = "datafusion-serve", feature = "tool-feeds-serve"))]
fn tool_feeds_lane(
    store: &vike_secrets::CredentialStore,
    cadences: crate::tool_feeds::ToolFeedCadences,
) -> Option<std::sync::Arc<crate::tool_feeds::ToolFeedLane>> {
    tracing::info!(
        news_secs = cadences.news.as_secs(),
        calendar_secs = cadences.calendar.as_secs(),
        equity_secs = cadences.equity.as_secs(),
        "vike-datahub: TOOL FEEDS lane mounted (news RSS, ForexFactory, Finnhub, FMP, Nasdaq; it \
         polls nothing until the first request) — clients ask this process for the news and the \
         calendars and fetch from no provider of their own. The Finnhub and FMP keys are read from \
         this box's credential store each poll cycle: `vike-cli secrets set FINNHUB_API_KEY` / \
         `FMP_API_KEY` on THIS box arms them"
    );
    Some(std::sync::Arc::new(crate::tool_feeds::http::real_tool_feeds_lane(
        store.clone(),
        cadences,
    )))
}

/// [`start_tool_feeds`] on a build without the lane: nothing to mount, said once. A cadence row on
/// such a build changes nothing, and says so rather than looking armed.
#[cfg(all(feature = "datafusion-serve", not(feature = "tool-feeds-serve")))]
fn tool_feeds_lane(
    _store: &vike_secrets::CredentialStore,
    cadences: crate::tool_feeds::ToolFeedCadences,
) -> Option<std::sync::Arc<crate::tool_feeds::ToolFeedLane>> {
    tracing::info!(
        "vike-datahub: no TOOL FEEDS lane in this build — NewsFeed, EconomicCalendar and \
         EquityCalendar answer NotArmed. Rebuild with `--features tool-feeds-serve` to serve them"
    );
    if cadences != crate::tool_feeds::ToolFeedCadences::default() {
        tracing::warn!(
            "vike-datahub: a config.datahub_*_poll_secs row is set, but this build carries no \
             tool-feeds lane, so no feed is polled and the row changes nothing"
        );
    }
    None
}

/// **Phase: the execution stream** — `Some(lane)` when this daemon keeps a settings database, where each tradehub
/// journal's cursor lives (`vike_secrets::execution_streams`); `run` calls this once the store is open and hands the
/// lane to the server, which writes a tradehub's streamed executions through the SAME store handle it serves.
/// No switch arms it and no journal directory is read: a tradehub that streams is recorded, and one that does not
/// sends nothing. `forecast_floor` is the optional writers' free-space floor, asked before forecast rows are written.
#[cfg(feature = "datafusion-serve")]
pub(super) fn start_execution_stream(
    booted: &vike_boot::Booted,
    forecast_floor: crate::execution_stream::ExecutionStreamForecastFloor,
) -> Option<std::sync::Arc<crate::execution_stream::ExecutionStreamLane>> {
    let Some(location) = booted.store.location() else {
        tracing::warn!("vike-datahub: {}", crate::execution_stream::NO_EXECUTION_STREAM);
        return None;
    };
    tracing::info!(
        settings = %location.settings_dir().display(),
        "vike-datahub: EXECUTION STREAM mounted: a tradehub's live executions are stored into the exec_fill / \
         exec_order / exec_forecast series, each journal's cursor in this settings database"
    );
    Some(std::sync::Arc::new(crate::execution_stream::ExecutionStreamLane::new(
        location.settings_dir().to_path_buf(),
        Some(forecast_floor),
    )))
}
