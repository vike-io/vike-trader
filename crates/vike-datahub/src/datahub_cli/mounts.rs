//! The mount phases: the market-data plane, the backfill table and the seed/catalog/import lanes.

#[cfg(feature = "backfill-serve")]
use super::oanda_history::{
    oanda_history_lane_line, oanda_history_presence_probe, oanda_history_token_reader,
    read_oanda_history_token,
};

/// **Phase: the live market-data plane** — `Some(hub)` only when an operator armed it with the
/// `flags.datahub_live` row; off is byte-identical to a build without the plane. `run` calls this once the listener is bound, with
/// the ONE broker both planes take their venue clients from.
#[cfg(feature = "serve-datafusion")]
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
    store: &std::sync::Arc<vike_data::DataFusionHist>,
) -> Option<crate::backfill::BackfillTable> {
    use std::sync::Arc;

    // Backfill-on-demand (split-plane REQ-9): a `backfill-serve` build mounts the REAL collector
    // table over the SAME store handle the server serves, which is what makes `serve` advertise
    // and answer `Request::Backfill`; any other build mounts none and the verb is a clean refusal.
    //
    // ⚠ Its CREDENTIALED row (OANDA's, decision 0097) takes a token PROVIDER built here,
    // off this boot's one settings directory — the same directory the Polymarket egress read above
    // uses — and the provider holds that directory and nothing else. The ONE read this block makes
    // itself is the startup line's: it is the read that logs the store's permission finding (the
    // provider's own per-request reads are silent), and its token is dropped inside the `map` before
    // anything is logged — the line reports PRESENCE, never a value.
    let presence = read_oanda_history_token(booted.settings_dir.as_deref(), true).map(drop);
    let oanda_history_token = oanda_history_token_reader(booted.settings_dir.clone());
    let line = oanda_history_lane_line(booted.settings_dir.as_deref(), presence);
    match presence {
        Err(vike_oanda::HistoryTokenError::StoreUnreadable) => {
            tracing::warn!("vike-datahub: {line}")
        }
        Ok(()) | Err(vike_oanda::HistoryTokenError::NotConfigured) => {
            tracing::info!("vike-datahub: {line}")
        }
    }
    // ...and the history-channels read's PRESENCE probe for that row, over the same directory:
    // a word per request, never a value (decision 0102). Keyed by the venue the OANDA bridge
    // names itself, the string the credentialed row was built under.
    Some(
        crate::backfill::real_backfill_table(Arc::clone(store), oanda_history_token)
            .with_credential_probe(
                vike_oanda::recon_client::VENUE,
                oanda_history_presence_probe(booted.settings_dir.clone()),
            ),
    )
}

/// **Phase: the chart-gap seed lane** — armed by an operator with the `flags.datahub_chart_seed`
/// row, and only on a build that mounted a collector table (`backfill` says whether it did). `run` calls this once, right after the
/// backfill table is bound.
#[cfg(feature = "serve-datafusion")]
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
#[cfg(feature = "serve-datafusion")]
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
    catalog_gate
        .serves()
        .then(|| std::sync::Arc::new(crate::catalog::CatalogLane::new(catalog_table)))
}

/// **Phase: the archive import lane** — mounted by the PROJECT, never by an operator switch.
/// `run` calls this once, after the catalog lane, with the store handle the registry's one format
/// folds in. The `store` parameter is read only by a `backfill-serve` build.
#[cfg(feature = "serve-datafusion")]
#[cfg_attr(not(feature = "backfill-serve"), allow(unused_variables, clippy::allow_attributes))]
pub(super) fn mount_import_lane(
    booted: &vike_boot::Booted,
    store: &std::sync::Arc<vike_data::DataFusionHist>,
) -> Option<std::sync::Arc<crate::import::ImportLane>> {
    use std::sync::Arc;

    // ⚠ **THE ARCHIVE IMPORT LANE — mounted by the PROJECT, not by an operator switch**
    // (`docs/decisions/0100`, verdicts 1 and 2). Its root is `<project>/market_data/imports`,
    // resolved ONCE here from this boot's settings directory (`imports_dir_beside`, inside
    // `crate::import::mount`) — no variable and no settings key, so `vike_ops::settings::SETTINGS`
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
