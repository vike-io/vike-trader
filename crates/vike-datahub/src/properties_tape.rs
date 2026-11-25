//! **The writer of the `kind=properties` tape**, armed by the `flags.record_properties` row, ON by
//! default (`crate::datahub_cli`'s `lanes` phase `start_properties_tape`): the instrument grid a
//! backtest's `engine.snap_to_properties` and `vike-cli data catalog show` read.
//!
//! It is a thread of this daemon, over the SERVED store handle (`docs/decisions/0084-only-the-datahub-touches-the-store.md`)
//! and the catalog lane's own bounds, so its venue calls are the gatewayhub's and the lane's budget is
//! the budget. No client request reaches it: `crate::server`'s `venue_catalog_verb` is still handed no
//! store, so that verb stays the Observe read
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 1 says it
//! is, and nothing here persists a catalog: no list, nothing replaced, one additive row per instrument.
//!
//! What it records, each limit a choice with a reason:
//!
//! - **Only a symbol the store holds prices for**, a catalog row matched to a series by the STORE name its
//!   KEPT row stores and filed under it (decision 0290, `vike_catalog::names_of_row` of the venue's
//!   `vike_catalog::kept_list` row of the same own name: bybit's perpetual `BTCUSDT.P` and its spot `BTCUSDT`
//!   are two series, hyperliquid's `BTC` is `BTCUSDC.P`; a row the grammar cannot name, or no kept row, is no
//!   series). A run can trade nothing else, and a venue's whole list
//!   is thousands of series every `inventory()` would then fold a manifest for. A symbol backfilled
//!   later gets its row on the next pass. A venue the store holds no prices on is not asked at all.
//! - **Only a row that states both a tick and a lot** ([`grid_of`]). A row without them is no grid, and
//!   recording it would silence what a run says about a symbol with none
//!   (`crates/vike-backtest/src/run/data_plan/verdict.rs`'s `missing_grid_lines`). Dukascopy's and FXCM's
//!   catalogs are bundled names. Polymarket's states a tick and no lot, and no `taker_hold_ms` either:
//!   its hold takes two REST calls per token the catalog verb does not make, and a recorded `0` would read
//!   as a market with no hold. So a Polymarket run still has no hold and still says so. None of the three
//!   is asked ([`NO_GRID_VENUES`]).
//! - **Only a row whose sizes count the coin a fill counts** ([`grid_of`]). `OrderBrokerSimulator::apply_fill` rounds a
//!   BASE size on the row, and an okx SWAP or FUTURES row counts CONTRACTS (`ctVal` of the coin each) and an
//!   inverse row US dollars, so the row raw would round 0.013 BTC to 0.01 and drop a 0.004 BTC open where the
//!   venue's real step is 0.0001. An okx LINEAR contract's row is restated in BASE by the one conversion the
//!   bridge's mounted path and symbol facts use (`vike_catalog::okx_grid_in_base`, since 2026-11-16: the tape
//!   skipped every SWAP and FUTURES before, so an okx derivative's run had no grid). Every other row that counts
//!   contracts, and every inverse row, is no grid, and the run says so: `contract_size` is no one meaning a shared
//!   rule could scale by (deribit's is the LOT, `vike_deribit::catalog::contract_value_of`).
//! - **The class from the row's `Contract`**, what the venue's fetch typed, never from the name.
//!
//! The recorder's commit key allows one row per symbol per UTC day, so a day is one store part per
//! series and no compaction pass has anything to merge: unlike `flags.record_chains`, this is not
//! refused on a daemon that runs no maintenance. A row's time is when it was SEEN, so the tape begins
//! the day the flag goes on and a run over an earlier window says that too.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use vike_data::{HistoryStore, PropertiesRecorder};
use vike_model::StoreKindId;
use vike_model::{Instrument, Margining, SymbolProperties};

use crate::catalog::{CATALOG_TTL, CatalogLane};

/// How often a pass runs: the lane's default memo, so four fetches a day per venue, the steady state
/// `crate::catalog`'s budget arithmetic for `vike_datahub_client::catalog::CATALOG_VENUE_REFILL` already prices. A box with a longer memo
/// row re-reads the memo here and asks the venue nothing.
pub const PASS_EVERY: Duration = CATALOG_TTL;

/// The wait after a pass a venue did not answer: a gatewayhub that was restarting must not cost six
/// hours of a day's one row, and an hour costs a dead venue one warning an hour.
pub const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

/// The catalog venues a pass never asks: [`grid_of`] drops every row of polymarket's (its list is up to forty
/// pages, four times a day, for nothing), and fxcm's and dukascopy's lists come from a running account's session
/// (2026-11-13: fxcm's spelled `EUR/USD` where its stored series say `EURUSD`; dukascopy's carries a price step and
/// no size, which the tape did not ask for before either), so the tape keeps its old answer for both: nothing; and
/// oanda and ctrader, whose lists the lane serves only as HELD lists since 2026-11-13: the tape never asked them,
/// and a run on their series keeps the answer it had; and deribit, served since DER-2 for its kept list, which is
/// fetched only on the operator's refresh (the owner: no timers) and whose inverse rows [`grid_of`] drops anyway;
/// and ig, served since 2026-11-16, whose list is about 17 reads of the account's own allowance, read only on the
/// operator's command, and states no grid anyway (its facts are read per epic). A row here that stops being true
/// costs that venue its grid, and its runs still say so.
const NO_GRID_VENUES: [&str; 7] =
    ["polymarket", "dukascopy", "fxcm", "oanda", "ctrader", "deribit", "ig"];

/// Start the writer's thread. It runs for the life of the process: a stop mid-write is the store's own
/// crash safety (the same as a recorder flush), and the next start's pass is idempotent.
pub fn start(
    lane: Arc<CatalogLane>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> std::io::Result<()> {
    std::thread::Builder::new().name("vike-properties-tape".to_string()).spawn(move || {
        let recorder = PropertiesRecorder::new(Arc::clone(&store), true);
        loop {
            let clean = record_pass(&lane, store.as_ref(), &recorder, vike_model::now_ms());
            std::thread::sleep(if clean { PASS_EVERY } else { RETRY_AFTER });
        }
    })?;
    Ok(())
}

/// One pass over every venue of the lane's table; `false` when something did not answer. A store error
/// on one row is the recorder's to log and drop.
pub fn record_pass(
    lane: &CatalogLane,
    store: &(dyn HistoryStore + Send + Sync),
    recorder: &PropertiesRecorder,
    now_ms: i64,
) -> bool {
    let held = match store.list_series() {
        Ok(held) => held,
        Err(e) => {
            tracing::warn!(error = %e, "properties tape: the store would not list its series");
            return false;
        }
    };
    let mut clean = true;
    for venue in lane.table().supported() {
        if NO_GRID_VENUES.contains(&venue) {
            continue;
        }
        let wanted: HashSet<&str> = held
            .iter()
            .filter(|id| id.venue == venue && !id.symbol.is_empty() && holds_prices(id.kind))
            .map(|id| id.symbol.as_str())
            .collect();
        if wanted.is_empty() {
            continue;
        }
        match lane.list(venue) {
            // A series is the row's STORE name (decision 0290) as its KEPT row stores it, found by the listing row's
            // own name and never spelled here (the owner's ruling «Б», 2026-11-16); the tape files the grid under it.
            Ok(rows) => {
                let kept = vike_catalog::kept_list(venue).unwrap_or_default();
                let store_of: HashMap<&str, String> = kept
                    .iter()
                    .filter(|row| row.venue == venue)
                    .filter_map(|row| {
                        let user = vike_catalog::user_name_of_row(row).ok()?;
                        Some((
                            row.raw_symbol.as_str(),
                            vike_paths::store_path::store_name_of_user_name(&user),
                        ))
                    })
                    .collect();
                recorder.record_all(
                    venue,
                    rows.into_iter().filter_map(|row| {
                        let store = store_of.get(row.raw_symbol.as_str())?;
                        if !wanted.contains(store.as_str()) {
                            return None;
                        }
                        Some((store.clone(), grid_of(row)?))
                    }),
                    now_ms.saturating_mul(1_000_000),
                );
            }
            Err(why) => {
                tracing::warn!(venue, %why, "properties tape: no list for this venue on this pass");
                clean = false;
            }
        }
    }
    clean
}

/// The kinds a run replays. An `exec_*` series names what was traded, which no run reads back as a price.
fn holds_prices(kind: StoreKindId) -> bool {
    matches!(
        kind,
        StoreKindId::Bar
            | StoreKindId::Quote
            | StoreKindId::Trade
            | StoreKindId::Book
            | StoreKindId::Depth
    )
}

/// What a catalog row says for the tape, or `None` when it states no grid a BASE-sized fill can round on
/// (the module doc's two limits): no tick or no lot, a contract that is not linear, or a row that counts
/// contracts the tape cannot restate (any venue's but okx's, whose linear SWAP or FUTURES row is restated in
/// BASE by the bridge's own conversion, `vike_catalog::okx_grid_in_base`).
fn grid_of(row: Instrument) -> Option<SymbolProperties> {
    let class = row.asset_class();
    let grid = match row.properties.contract_size {
        size if size <= 0.0 => row.properties,
        size if row.venue == "okx" && size.is_finite() => {
            vike_catalog::okx_grid_in_base(&row.properties, size)
        }
        _ => return None,
    };
    let linear = matches!(row.contract.margining(), None | Some(Margining::Linear));
    (linear && grid.tick_size > 0.0 && grid.step_size > 0.0)
        .then_some(SymbolProperties { asset_class: Some(class), ..grid })
}

#[path = "properties_tape_tests.rs"]
#[cfg(test)]
mod properties_tape_tests;
