//! The mount phases: the market-data plane, the backfill table, the seed/catalog/import lanes and
//! the journal materializer.

#[cfg(feature = "backfill-serve")]
use super::oanda_history::{
    oanda_history_lane_line, oanda_history_presence_probe, oanda_history_token_reader,
    read_oanda_history_token,
};

/// **Phase: the live market-data plane** — `Some(hub)` only when an operator armed it with the
/// `flags.datahub_live` row; off is byte-identical to a build without the plane. `run` calls this once the listener is bound, with
/// the ONE broker both planes take their venue clients from.
#[cfg(feature = "datafusion-serve")]
pub(super) fn mount_market_data_plane(
    settings: &vike_config::Settings,
    broker: &std::sync::Arc<crate::feeds::FeedBroker>,
) -> Option<std::sync::Arc<crate::md::MdHub>> {
    use std::sync::Arc;

    // ⚠ **THE LIVE MARKET-DATA PLANE, ARMED BY AN OPERATOR AND NOT BY A BUILD** — the
    // `flags.datahub_live` row, and nothing else (decision 0111: `VIKE_DATAHUB_LIVE` refuses
    // startup naming that row).
    //
    // ⚠ Off is BYTE-IDENTICAL to a build without the plane: no hub is mounted, `served_features`
    // advertises neither `market_data` nor any `md_venue=` entry, and `MdSubscribe` is answered
    // with `crate::server::NO_MARKET_DATA_PLANE` on a connection that stays positional. That is the
    // credential-is-the-gate idiom pointed at a CAPABILITY: the build carries it, the operator arms
    // it, and the two cannot answer differently because the advertisement keys on the MOUNT.
    if settings.flags.datahub_live {
        // ⚠ NO `#[cfg]` HERE, and that is the whole point of the feature-free hub. The venue TABLE
        // is feature-free code with cfg'd ARMS, so a build with no venue feature mounts a hub that
        // refuses every venue BY NAME — naming the feature to rebuild with — rather than serving
        // nothing quietly. That is a real configuration, exactly as `record` alone is, and it is
        // `crate::recording::build_recording_feed`'s rule one level up.
        let hub = crate::md::MdHub::new(
            crate::md::venues::market_builder(Arc::clone(broker)),
            crate::md::venues::supported().into_iter().map(str::to_string).collect(),
        );
        // Tier R — the RESIDENT set, the `config.datahub_live_resident` row. An unparseable entry
        // is a WARNING that names the entry rather than a startup refusal, because a typo in one
        // resident key must not take the whole data wire down
        // (`docs/decisions/0013-degrade-vs-refuse.md`).
        let resident_raw = settings.config.datahub_live_resident.clone().unwrap_or_default();
        let (resident, bad) = crate::md::parse_resident_set(&resident_raw);
        for row in &bad {
            tracing::warn!(
                row = %row,
                "vike-datahub md: ignoring an unparseable config.datahub_live_resident entry — \
                 the format is venue:symbol:lane with lane one of depth|book|trades"
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
                Ok(()) => pinned += 1,
                Err(why) => tracing::warn!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    lane = ?spec.lane,
                    %why,
                    "vike-datahub md: REFUSING a config.datahub_live_resident entry — it is not \
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

/// **Phase: the backfill table** — the REAL collector table over the SAME store handle the server
/// serves, plus the startup line saying whether the OANDA history lane is armed. `run` calls this
/// only in a `backfill-serve` build; any other build binds `backfill` to `None` itself.
#[cfg(feature = "backfill-serve")]
pub(super) fn mount_backfill(
    booted: &vike_boot::Booted,
    store: &std::sync::Arc<vike_store::DataFusionHist>,
) -> Option<crate::backfill::BackfillTable> {
    use std::sync::Arc;

    // Backfill-on-demand (split-plane REQ-9): a `backfill-serve` build mounts the REAL collector
    // table over the SAME store handle the server serves, which is what makes `serve` advertise
    // and answer `Request::Backfill`; any other build mounts none and the verb is a clean refusal.
    //
    // ⚠ Its CREDENTIALED row (OANDA's, decision 0097) takes a token PROVIDER built here,
    // off this boot's credential store handle (`booted.store`, opened over the one settings
    // directory the Polymarket egress read above uses) — and the provider holds that handle and
    // nothing else. The ONE read this block makes itself is the startup line's: it is the read that
    // logs the store's permission finding (the provider's own per-request reads are silent), and
    // its token is dropped inside the `map` before anything is logged — the line reports PRESENCE,
    // never a value.
    let presence = read_oanda_history_token(&booted.store, true).map(drop);
    let oanda_history_token = oanda_history_token_reader(booted.store.clone());
    let line = oanda_history_lane_line(booted.settings_dir.as_deref(), presence);
    match presence {
        Err(vike_oanda::HistoryTokenError::StoreUnreadable) => {
            tracing::warn!("vike-datahub: {line}")
        }
        Ok(()) | Err(vike_oanda::HistoryTokenError::NotConfigured) => {
            tracing::info!("vike-datahub: {line}")
        }
    }
    // ...and the history-channels read's PRESENCE probe for that row, over the same store:
    // a word per request, never a value (decision 0102). Keyed by the venue the OANDA bridge
    // names itself, the string the credentialed row was built under.
    Some(
        crate::backfill::real_backfill_table(Arc::clone(store), oanda_history_token)
            .with_credential_probe(
                vike_oanda::recon_client::VENUE,
                oanda_history_presence_probe(booted.store.clone()),
            ),
    )
}

/// **Phase: the chart-gap seed lane** — armed by an operator with the `flags.datahub_chart_seed`
/// row, and only on a build that mounted a collector table (`backfill` says whether it did). `run` calls this once, right after the
/// backfill table is bound.
#[cfg(feature = "datafusion-serve")]
pub(super) fn mount_seed_lane(
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
/// calls this once, after the seed lane. The `booted.settings.flags.venue_catalog_off,` argument is
/// the needle `vike_config::CONSUMPTION`'s row for that key reads in this file.
#[cfg(feature = "datafusion-serve")]
pub(super) fn mount_catalog_lane(
    booted: &vike_boot::Booted,
) -> Option<std::sync::Arc<crate::catalog::CatalogLane>> {
    // ⚠ **THE VENUE-CATALOG LANE, SWITCHED BY A SETTINGS ROW AND NOT BY A BUILD** — on by default,
    // and the `flags.venue_catalog_off` row is its one switch, the same idiom as the two arms above.
    //
    // ⚠ **A build feature could NOT be the arming**, and that is argued rather than assumed:
    // `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md` means a
    // `catalog-serve`-gated default would be ON wherever the image is the install, which is exactly
    // where it matters most. The switch has to be runtime.
    //
    // ⚠ Unlike the seed lane above, an armed lane on a table-less build is NOT refused here. It is
    // armed with an EMPTY table, and every venue then answers a `NotServed` naming an empty
    // supported set. The difference is deliberate and is the honest direction: the seed lane's
    // unarmed answer is indistinguishable from a working lane that found no rows, so arming it
    // without collectors would mislead — while THIS verb's `NotServed` says outright that the build
    // carries no providers, which is precisely what an operator who set the variable needs to read.
    // `docs/decisions/0013-degrade-vs-refuse.md`: a capability degrades, it does not refuse to
    // start.
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
    // `crates/vike-config/tests/flag_registry.rs`'s `every_flag_defaults_off` makes a default-ON
    // positive flag unrepresentable in that type. The verdict is
    // `vike_catalog::venue_catalog_gate`, which lives one crate below every consumer so no second
    // root can word it differently — the `reconcile_gate` shape, copied deliberately.
    //
    // ⚠ The PROVIDER-LESS build still SERVES, exactly as an armed table-less build did before, and
    // the argument is unchanged: this verb's `NotServed` says outright that the build carries no
    // providers, which is what an operator needs to read. `docs/decisions/0013-degrade-vs-refuse.md`
    // — a capability degrades, it does not refuse to start. What is NEW is that the same shape is
    // now reachable without anybody having asked for the lane, which is why the gate gives it its
    // own verdict arm rather than folding it into a warning beside an `Armed` one.
    // `flags.hyperliquid_hip3` — the same resolved row the trading daemon's hyperliquid mount folds
    // (decision 0095), so this catalog's HIP-3 universe cannot disagree with the mount's symbology.
    #[cfg(feature = "catalog-serve")]
    let catalog_table = crate::catalog::real_catalog_table(booted.settings.flags.hyperliquid_hip3);
    #[cfg(not(feature = "catalog-serve"))]
    let catalog_table = crate::catalog::CatalogTable::new(Vec::new());
    // ⚠ This is the site `crates/vike-config/src/consumed.rs`'s row for `flags.venue_catalog_off`
    // names, needle and all, and it must stay an ARGUMENT to the gate rather than a local bound
    // first: a `Consumption` row proves a key is READ, and a value assigned and never passed is
    // exactly the declared-but-unconsumed shape that table exists to catch.
    let catalog_gate = vike_catalog::venue_catalog_gate(
        booted.settings.flags.venue_catalog_off,
        catalog_table.supported().len(),
    );
    {
        // Both answers are news — the gate's module doc, property 3. The REFUSAL is `warn!`
        // because the operator of a box whose symbol picker will be empty has to be able to find
        // out why from the log alone; the served verdict is `info!` because it is the ordinary
        // state.
        let line = vike_catalog::venue_catalog_gate_line(
            catalog_gate,
            &catalog_table.supported().join(", "),
        );
        match catalog_gate {
            vike_catalog::VenueCatalogGate::Armed => tracing::info!("vike-datahub: {line}"),
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

/// **Phase: the archive import lane** — mounted by the PROJECT, never by an operator switch.
/// `run` calls this once, after the catalog lane, with the store handle the registry's one format
/// folds in. The `store` parameter is read only by a `backfill-serve` build.
#[cfg(feature = "datafusion-serve")]
#[cfg_attr(not(feature = "backfill-serve"), allow(unused_variables, clippy::allow_attributes))]
pub(super) fn mount_import_lane(
    booted: &vike_boot::Booted,
    store: &std::sync::Arc<vike_store::DataFusionHist>,
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
    let import_formats = crate::import::formats::real_import_registry(Arc::clone(store));
    #[cfg(not(feature = "backfill-serve"))]
    let import_formats: Vec<Box<dyn crate::import::ArchiveFormat>> = Vec::new();
    let (import_lane, import_line) =
        crate::import::mount(booted.settings_dir.as_deref(), import_formats);
    tracing::info!("vike-datahub: {import_line}");
    import_lane.map(Arc::new)
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
/// (`crate::options::deribit`'s module doc).
///
/// ⚠ **On, with no compaction, it is REFUSED** ([`chain_recorder_arming`]): every snapshot is one
/// store part, and only a recording profile's maintenance scheduler compacts them.
///
/// ⚠ **Two more rows tune the lane, and this is the one place they are read**: the chain recorder's
/// snapshot cadence (`config.datahub_chain_cadence_secs`) and the REST poll interval
/// (`config.datahub_options_poll_secs`). `crate::options::deribit` reads no settings; each value
/// reaches it as a parameter, and an unset row is the compiled-in value, so a box without them is
/// byte-identical to the lane before they existed.
#[cfg(feature = "datafusion-serve")]
pub(super) fn mount_options_lane(
    booted: &vike_boot::Booted,
    store: &std::sync::Arc<vike_store::DataFusionHist>,
    compaction_runs: bool,
) -> Option<std::sync::Arc<crate::options::OptionsLane>> {
    options_lane(
        booted.settings.flags.record_chains,
        compaction_runs,
        store,
        booted.settings.config.datahub_chain_cadence_secs,
        booted.settings.config.datahub_options_poll_secs,
    )
}

/// The chain recorder's arming verdict — a pure function of the row and of whether this process
/// compacts its store, so the decision is tested without a lane (`options_chain_recorder`, an
/// `options-serve` item, acts on it). The `allow` is [`materializer_arming`]'s, widened to every
/// build in which no `options-serve` serving `run` calls it.
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
    store: std::sync::Arc<dyn vike_data::HistStore + Send + Sync>,
    cadence_ms: i64,
) -> Option<std::sync::Arc<vike_data::ChainRecorder>> {
    let arming = chain_recorder_arming(record_chains, compaction_runs);
    if arming == ChainRecorderArming::RefusedNoCompaction {
        tracing::warn!("vike-datahub: {}", chain_recorder_refusal_line());
    }
    crate::options::deribit::chain_recorder(arming == ChainRecorderArming::Armed, store, cadence_ms)
}

/// [`mount_options_lane`] on an `options-serve` build: the real lane, with the recorder when armed.
/// `chain_cadence_secs` and `poll_secs` are the two tuning rows, still unresolved: `None` is the
/// compiled-in value (`crate::options::deribit::chain_cadence_ms` and `poll_interval`).
#[cfg(all(feature = "datafusion-serve", feature = "options-serve"))]
fn options_lane(
    record_chains: bool,
    compaction_runs: bool,
    store: &std::sync::Arc<vike_store::DataFusionHist>,
    chain_cadence_secs: Option<u32>,
    poll_secs: Option<u32>,
) -> Option<std::sync::Arc<crate::options::OptionsLane>> {
    use crate::options::deribit::{chain_cadence_ms, poll_interval};
    use std::sync::Arc;

    let cadence_ms = chain_cadence_ms(chain_cadence_secs);
    let poll = poll_interval(poll_secs);
    let chain_rec =
        options_chain_recorder(record_chains, compaction_runs, Arc::clone(store) as _, cadence_ms);
    // A tuning row is disclosed when it is in force, and a row that nothing reads in the box's
    // current state is said so rather than left to look configured (`config show` cannot know
    // whether the recorder armed).
    if poll_secs.is_some() {
        tracing::info!(
            poll_secs = poll.as_secs(),
            "vike-datahub options: the REST poll interval is the config.datahub_options_poll_secs \
             row, not the default — every poll spends Deribit's public budget from this box's IP"
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
        "vike-datahub: OPTIONS lane mounted (Deribit; it polls nothing until the first \
             OptionsChain request{}) — clients ask this process for option chains and open no \
             venue socket of their own",
        if chain_rec.is_some() {
            " — EXCEPT that the flags.record_chains row is on, so it starts now and records \
                 every polled chain into kind=chain"
        } else {
            ""
        }
    );
    Some(Arc::new(crate::options::deribit::real_options_lane(chain_rec, poll)))
}

/// [`mount_options_lane`] on a build without the lane: nothing to mount, said once.
#[cfg(all(feature = "datafusion-serve", not(feature = "options-serve")))]
fn options_lane(
    record_chains: bool,
    _compaction_runs: bool,
    _store: &std::sync::Arc<vike_store::DataFusionHist>,
    chain_cadence_secs: Option<u32>,
    poll_secs: Option<u32>,
) -> Option<std::sync::Arc<crate::options::OptionsLane>> {
    if record_chains {
        tracing::warn!(
            "vike-datahub: the flags.record_chains row is on, but this build carries no \
                 options lane (`--features options-serve`), so NO option chain is polled or \
                 recorded"
        );
    }
    if chain_cadence_secs.is_some() || poll_secs.is_some() {
        tracing::warn!(
            "vike-datahub: a config.datahub_chain_cadence_secs or config.datahub_options_poll_secs \
             row is set, but this build carries no options lane (`--features options-serve`), so \
             the row is read by nothing"
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
pub(super) fn mount_tool_feeds(
    booted: &vike_boot::Booted,
    cadences: crate::tool_feeds::Cadences,
) -> Option<std::sync::Arc<crate::tool_feeds::ToolFeedsLane>> {
    tool_feeds_lane(&booted.store, cadences)
}

/// **Phase: the tool-feed cadences** — the box's `config.datahub_news_poll_secs`,
/// `config.datahub_calendar_poll_secs` and `config.datahub_equity_poll_secs` rows as the lane's
/// [`Cadences`](crate::tool_feeds::Cadences); an absent row is that feed's default, so a box without
/// the rows polls exactly as before. `Err` carries the exit status (2: a refused CONFIGURATION).
/// `run` calls this BEFORE the bind guard, beside the other row parses, so a refusal is never a
/// crash-loop behind a bound port.
///
/// The rows were already refused outside 60 to 86400 seconds at the write and at the load
/// (`vike_config::Config::apply`); `Cadences::from_rows` refuses them again, which is the lane's own
/// guard against a zero cadence and fires on no row a loader accepted.
#[cfg(feature = "datafusion-serve")]
pub(super) fn load_feed_cadences(
    booted: &vike_boot::Booted,
) -> Result<crate::tool_feeds::Cadences, std::process::ExitCode> {
    match crate::tool_feeds::Cadences::from_rows(
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
            Err(std::process::ExitCode::from(2))
        }
    }
}

/// [`mount_tool_feeds`] on a `tool-feeds-serve` build: the real lane.
#[cfg(all(feature = "datafusion-serve", feature = "tool-feeds-serve"))]
fn tool_feeds_lane(
    store: &vike_secrets::CredentialStore,
    cadences: crate::tool_feeds::Cadences,
) -> Option<std::sync::Arc<crate::tool_feeds::ToolFeedsLane>> {
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

/// [`mount_tool_feeds`] on a build without the lane: nothing to mount, said once. A cadence row on
/// such a build changes nothing, and says so rather than looking armed.
#[cfg(all(feature = "datafusion-serve", not(feature = "tool-feeds-serve")))]
fn tool_feeds_lane(
    _store: &vike_secrets::CredentialStore,
    cadences: crate::tool_feeds::Cadences,
) -> Option<std::sync::Arc<crate::tool_feeds::ToolFeedsLane>> {
    tracing::info!(
        "vike-datahub: no TOOL FEEDS lane in this build — NewsFeed, EconomicCalendar and \
         EquityCalendar answer NotArmed. Rebuild with `--features tool-feeds-serve` to serve them"
    );
    if cadences != crate::tool_feeds::Cadences::default() {
        tracing::warn!(
            "vike-datahub: a config.datahub_*_poll_secs row is set, but this build carries no \
             tool-feeds lane, so no feed is polled and the row changes nothing"
        );
    }
    None
}

/// The journal materializer's arming verdict — a pure function of its two rows, so the decision is
/// tested without a thread ([`mount_journal_materializer`] acts on it).
///
/// ⚠ The `allow`s on this and the three functions below are `startup_mode_line`'s shape
/// (`crates/vike-datahub/src/datahub_cli/recording.rs`): only the serving `run` calls the phase, and
/// the tests exercise it in every build.
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MaterializerArming {
    /// `flags.journal_materializer` is off — the default. Nothing spawns.
    Off,
    /// On, and `config.journal_dir` names the directory to follow.
    Spawn(std::path::PathBuf),
    /// On, and no `config.journal_dir` row: refused with a warning, nothing spawns.
    /// `docs/decisions/0013-degrade-vs-refuse.md` — an off-path writer degrades, it does not
    /// refuse the data wire.
    RefusedNoDir,
}

/// **The arming decision.** `enabled` is the `flags.journal_materializer` row and `journal_dir` the
/// `config.journal_dir` row, both as this process's boot resolved them.
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn materializer_arming(
    enabled: bool,
    journal_dir: Option<&std::path::Path>,
) -> MaterializerArming {
    match (enabled, journal_dir) {
        (false, _) => MaterializerArming::Off,
        (true, Some(dir)) => MaterializerArming::Spawn(dir.to_path_buf()),
        (true, None) => MaterializerArming::RefusedNoDir,
    }
}

/// The startup line the refusal owes an operator — named so a test can hold it to naming BOTH rows.
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn materializer_refusal_line() -> &'static str {
    "the JOURNAL MATERIALIZER is switched on (the flags.journal_materializer row) but there is no \
     config.journal_dir row naming the journal it should follow — it is NOT started. Write the \
     directory the trade daemon journals to (`vike-cli config set config.journal_dir <dir>`) and \
     restart, or switch the flag off"
}

/// **Phase: the journal materializer** — `Some(handle)` only when an operator armed it with the
/// `flags.journal_materializer` row AND a `config.journal_dir` row names the journal; `run` calls
/// this once the store is open, with the SAME store handle the server serves, and holds the handle
/// for the process lifetime (drop = one final drain, then join).
///
/// ⚠ The directory is the TRADE daemon's (it writes the journal; this process only reads it and
/// writes `materializer.ckpt` beside it). It is the same `config.journal_dir` row that daemon
/// journals to and its ONLY directory (decision 0111, amended 2026-10-11: a run profile only tunes
/// the journal), so the two follow one directory by construction. The datahub needs read AND
/// write access to it — an ops step, `crate::materialize`'s module doc — and this phase checks it
/// once (`crate::materialize`'s `probe_checkpoint_write`), refusing to spawn, with one warning,
/// when the directory is missing or that write is refused. With the flag OFF and a checkpoint left
/// in the directory, the journal's prune keeps every segment past it, so that case is warned about
/// here.
///
/// Feature-free, so the arming tests run in every build; only the serving `run` calls it.
#[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
pub(super) fn mount_journal_materializer(
    settings: &vike_config::Settings,
    store: std::sync::Arc<dyn vike_data::HistStore + Send + Sync>,
) -> Option<crate::materialize::MaterializerHandle> {
    // ⚠ This argument is the needle `vike_config::CONSUMPTION`'s `flags.journal_materializer` row
    // reads in this file: keep it an ARGUMENT to the decision, never a local bound first.
    let arming = materializer_arming(
        settings.flags.journal_materializer,
        settings.config.journal_dir.as_deref(),
    );
    match arming {
        MaterializerArming::Off => {
            // The one OFF case worth a line: a checkpoint left behind pins the journal's segments.
            // A stat of one file, and an unreadable directory simply answers "absent".
            if let Some(dir) = settings.config.journal_dir.as_deref()
                && vike_journal::MaterializeCheckpoint::exists(dir)
            {
                tracing::warn!(
                    dir = %dir.display(),
                    "vike-datahub: the journal materializer is OFF but its checkpoint \
                     (materializer.ckpt) is still in the journal directory — the trade daemon's \
                     prune keeps every journal segment past it, so the directory grows. Switch \
                     the materializer on (the flags.journal_materializer row) or delete that file"
                );
            }
            None
        }
        MaterializerArming::RefusedNoDir => {
            tracing::warn!("vike-datahub: {}", materializer_refusal_line());
            None
        }
        MaterializerArming::Spawn(dir) => {
            // The checkpoint write, asked once BEFORE anything is appended: a materializer that
            // appends and cannot checkpoint grows the journal directory and re-appends after a
            // restart (`crate::materialize`'s module doc). The probe changes nothing it finds.
            if let Err(error) = crate::materialize::probe_checkpoint_write(&dir) {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %error,
                    "vike-datahub: the JOURNAL MATERIALIZER is switched on (the \
                     flags.journal_materializer row) but this process cannot write the journal \
                     directory config.journal_dir names, where its materializer.ckpt lives — it is \
                     NOT started. Grant this unit write access to that one directory (its \
                     ReadWritePaths= line, plus a shared group or an ACL) and restart, or switch \
                     the flag off"
                );
                return None;
            }
            tracing::info!(
                dir = %dir.display(),
                "vike-datahub: JOURNAL MATERIALIZER armed (the flags.journal_materializer row) — \
                 folding the command journal in this directory into the store's exec_fill / \
                 exec_order series, resuming from its materializer.ckpt"
            );
            crate::materialize::maybe_spawn(
                Some(dir),
                store,
                crate::materialize::MaterializerConfig::default(),
            )
        }
    }
}
