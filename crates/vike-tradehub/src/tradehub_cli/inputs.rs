//! **A mount's INPUTS, read from the DATAHUB** (decision 0271's live half; the owner's ruling of
//! 2026-10-20: prices go gateway, datahub, db, and strategies read from the datahub): the core's
//! [`vike_core::CoreConfig::input_opener`] and what keeps each opened input fresh. BOTH arms hand it in
//! ([`datahub_inputs`]): the live arm (`live_runtime_with`) and the paper arm (`flags.tradehub_live` off,
//! `start_paper`), so a paper mount reads its inputs as a live one does.
//!
//! - **At mount** the core asks [`input_opener`] for each name its strategy declares. The name goes
//!   through the profile's `[inputs]` remap (the backtest profile's table, `ProfileDaemon::inputs`) and is
//!   parsed by the backtest's grammar (`vike_data::store::series::parse_input_name_for`: a one-part name,
//!   `"1d"`, is the mount's own symbol at that bar length). Its history is read as deep as a backtest reads
//!   it (`vike_backtest`'s `load_inputs`): an input whose bars are LONGER than the mount's own reads its
//!   whole stored history, any other its last [`INPUT_HISTORY_BARS`] bars (more than a backtest's one bar
//!   before its window). The read goes through the datahub's strategy lane
//!   (`vike_datahub_client::strat::input_bars`, the `StrategyInputBars` read, on this daemon's lane key), under
//!   the store's bar ceiling: a history longer than one reply can carry is the datahub's refusal by name,
//!   never a shorter answer. It runs on the core's fold thread, so it is BOUNDED ([`within`],
//!   [`INPUT_READ_BOUND`]). A name that does not parse, a read that fails or outlives its bound, or a series
//!   the store holds no bar of refuses the mount by name in the core.
//! - **A venue's own candles** (`VENUE:SYMBOL:INTERVAL`, a higher timeframe of the traded symbol among
//!   them) are then held open over the strategy lane like a mount's own bar series, by the instrument's USER name
//!   (decision 0290, [`input_key`]: the series is read by its store name, the lane asked by the user name its bridge
//!   translates): [`InputKeep::venue_series`]
//!   joins the bar keeper's list (`TradeLeaseRegistry::keep_bars`), so their closes reach the core through the arm's
//!   sink, no second feed. The paper arm dials no feed at all (its mounts hear no bar of their own either),
//!   so there nothing keeps a venue series fresh and such an input turns stale.
//! - **A lane** (`LANE:VENUE:SYMBOL:INTERVAL`, a user's CSV import), which nothing streams, is read again
//!   after its last bar every [`LANE_REFRESH_EVERY`] ([`spawn_lane_refresher`], a thread of its own, never
//!   the core's), and what is new goes to the core as a bar seed under the lane's key ([`input_key`]), on
//!   both arms.
//!
//! Nothing here is persisted: a restart opens every input again at its mount.

use std::collections::BTreeMap;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use vike_data::SeriesId;
use vike_data::store::series::{SeriesOwn, parse_input_name_for};
use vike_execution::SeriesKey;
use vike_marketdata::Bar;

/// How far back an input whose bars are NOT longer than the mount's own is read at mount, in its own
/// bars: a thousand hours of an hourly input on an hourly mount. A longer one reads its whole history.
pub(super) const INPUT_HISTORY_BARS: i64 = 1_000;

/// How long ONE input's read at mount may take before the mount is refused by name. The core asks its
/// opener on the fold thread, at both mount doors, and the datahub client's own bounds (a 10 s connect,
/// a 10 s handshake, a 300 s reply) would hold the fold that long on a datahub that hangs. Not measured:
/// how long the datahub takes to answer an input's history, a whole one under the bar ceiling included.
pub(super) const INPUT_READ_BOUND: Duration = Duration::from_secs(5);

/// How often a lane input is read again from the store.
pub(super) const LANE_REFRESH_EVERY: Duration = Duration::from_secs(60);

/// One stored-series read, `(series, first, last)` with both epoch-ms bounds inclusive: the bars, or why
/// they could not be read. Production is [`super::live_runtime`]'s datahub read.
pub(super) type InputReader =
    Arc<dyn Fn(&SeriesId, i64, i64) -> Result<Vec<Bar>, String> + Send + Sync>;

/// One lane input the refresher reads again.
struct Lane {
    id: SeriesId,
    key: SeriesKey,
    /// The newest bar the core was sent.
    last_timestamp: i64,
    /// A read failed and was said; quiet until one succeeds.
    failing: bool,
}

/// What this node keeps fresh for the inputs the core opened: shared by the opener (which adds), the bar
/// keeper (which reads the venue series) and the lane refresher. Never shrinks: a series stays open to the
/// teardown, as a mount's own bar series does.
#[derive(Default)]
pub(super) struct InputKeep {
    venue_series: Mutex<Vec<SeriesKey>>,
    lanes: Mutex<Vec<Lane>>,
}

impl InputKeep {
    /// The venue series the bar keeper holds open, `(venue, symbol, interval)`.
    pub(super) fn venue_series(&self) -> Vec<SeriesKey> {
        self.venue_series.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Keep `id` fresh from now on, its newest bar at `last_timestamp`.
    fn keep(&self, id: &SeriesId, key: &SeriesKey, last_timestamp_value: i64) {
        if id.source.is_none() {
            let mut held = self.venue_series.lock().unwrap_or_else(PoisonError::into_inner);
            if !held.contains(key) {
                held.push(key.clone());
            }
            return;
        }
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        if !lanes.iter().any(|l| &l.key == key) {
            lanes.push(Lane {
                id: id.clone(),
                key: key.clone(),
                last_timestamp: last_timestamp_value,
                failing: false,
            });
        }
    }

    /// **Read every lane again after its newest bar, up to `now`**, and hand what is new to `send` under the
    /// lane's key. A failed read is said once at `warn` and asked again at the next pass. `send` answers
    /// `false` when the core is gone; the pass then stops and says so with `false`. No lock is held across a
    /// read.
    pub(super) fn refresh_lanes(
        &self,
        read: &InputReader,
        send: &dyn Fn(SeriesKey, Vec<Bar>) -> bool,
        now: i64,
    ) -> bool {
        let due: Vec<(SeriesId, SeriesKey, i64)> = self
            .lanes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|l| (l.id.clone(), l.key.clone(), l.last_timestamp))
            .collect();
        for (id, key, last_timestamp) in due {
            let read = read(&id, last_timestamp.saturating_add(1), now);
            let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(lane) = lanes.iter_mut().find(|l| l.key == key) else { continue };
            match read {
                Ok(bars) => {
                    lane.failing = false;
                    let Some(newest) = bars.iter().map(|b| b.timestamp).max() else { continue };
                    lane.last_timestamp = newest;
                    drop(lanes);
                    if !send(key, bars) {
                        return false;
                    }
                }
                Err(why) => {
                    if !std::mem::replace(&mut lane.failing, true) {
                        tracing::warn!(
                            lane = %key.0, symbol = %key.1, interval = %key.2,
                            "an input lane could not be read again from the datahub ({why}): the mount keeps \
                             the bars it holds, and an input older than two of its bars holds its opening \
                             orders. Asked again every minute, said once"
                        );
                    }
                }
            }
        }
        true
    }
}

/// A venue's KEPT rows as this process reads them: `vike_catalog::kept_list` in production.
pub(super) type KeptRows = fn(&str) -> Option<Arc<Vec<vike_model::Instrument>>>;

/// **The key the core files an input's bars under**: a venue's own candles under the strategy lane's own
/// `(venue, symbol, interval)`, so the feed's bars of it land there, the symbol the instrument's USER name
/// (decision 0290: the name the price lane is asked by and its bridge translates, audit flag DUK-4), found from
/// the stored series' STORE name by its row among `kept`'s; a lane's under the venue led by its lane
/// (`user-mine:binance`), a key no venue feed names.
///
/// # Errors
/// The venue keeps no list, or it holds no row (or two) of the stored name: the translator's refusal in words.
pub(super) fn input_key(id: &SeriesId, kept: KeptRows) -> Result<SeriesKey, String> {
    let interval = id.interval.clone().unwrap_or_default();
    Ok(match id.source.as_deref() {
        Some(lane) => (format!("{lane}:{}", id.venue), id.symbol.clone(), interval),
        None => {
            let rows = kept(&id.venue).unwrap_or_default();
            let user = vike_catalog::names_of_store_name(&rows, &id.venue, &id.symbol)?.user;
            (id.venue.clone(), user, interval)
        }
    })
}

/// **The core's input opener** (the module doc): `name` through `remap` (the profile's `[inputs]`), parsed
/// against the mount's own series, its history read up to `now()` through `read` (the whole of it for an
/// input whose bars are longer than the mount's `interval`, else its last [`INPUT_HISTORY_BARS`]), filed under
/// [`input_key`] over `kept`, and the series kept fresh when the store holds any of it.
pub(super) fn input_opener(
    keep: Arc<InputKeep>,
    read: InputReader,
    now: fn() -> i64,
    kept: KeptRows,
    remap: BTreeMap<String, String>,
) -> vike_core::InputOpener {
    Box::new(move |name: &str, venue: &str, symbol: &str, interval: &str| {
        let read_as = remap.get(name).map_or(name, String::as_str);
        let own = Ok(SeriesOwn { venue, symbol, lane: None });
        let (id, interval_ms) = parse_input_name_for(read_as, own).map_err(|e| {
            if read_as == name { e } else { format!("[inputs] maps it to {read_as:?}: {e}") }
        })?;
        let end = now();
        // The backtest's depth (`vike_backtest`'s `load_inputs`): a higher timeframe reads its whole history,
        // and a mount whose interval has no length (as the backtest's tick lane) reads every input so.
        let own_ms = vike_marketdata::interval::interval_ms(interval).unwrap_or(0);
        let start = if interval_ms > own_ms {
            0
        } else {
            end.saturating_sub(interval_ms.saturating_mul(INPUT_HISTORY_BARS))
        };
        let bars = read(&id, start, end)?;
        let key = input_key(&id, kept)?;
        if let Some(newest) = bars.iter().map(|b| b.timestamp).max() {
            keep.keep(&id, &key, newest);
        }
        // The calendar its market keeps, by the backtest's coverage rule and the class its KEPT row states: with
        // one, only the market's open time counts toward staleness and closed time never does
        // (`vike_core::InputRead::calendar`); a symbol the kept list does not carry has none.
        let row = super::settings::session_row(&id.venue, &id.symbol);
        let calendar = vike_catalog::trading_calendar(&id.venue, row.as_ref());
        Ok(vike_core::InputRead { key, interval_ms, bars, calendar })
    })
}

/// Start the thread that reads every lane input again each `every` ([`InputKeep::refresh_lanes`]) and sends
/// what is new through `send`, until the keep is dropped or the core is gone.
pub(super) fn spawn_lane_refresher(
    keep: Weak<InputKeep>,
    read: InputReader,
    send: impl Fn(SeriesKey, Vec<Bar>) -> bool + Send + 'static,
    every: Duration,
) {
    let spawned = std::thread::Builder::new().name("input-lanes".into()).spawn(move || {
        loop {
            std::thread::sleep(every);
            let Some(keep) = keep.upgrade() else { return };
            if !keep.refresh_lanes(&read, &send, vike_model::now_ms()) {
                return;
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(
            "could not start the thread that reads input lanes again ({e}): a mount reading a user's lane \
             sees the bars it held at mount, and the input turns stale"
        );
    }
}

/// **`read`, BOUNDED** (the fold-thread read at mount, [`INPUT_READ_BOUND`]): each call runs on a thread of
/// its own and is answered within `bound`, or refused with the bound named. A read past its bound runs on
/// to its own end on that thread and its answer is dropped; it holds no lock of the core's.
pub(super) fn within(read: InputReader, bound: Duration) -> InputReader {
    Arc::new(move |id: &SeriesId, start: i64, end: i64| {
        let (tx, rx) = sync_channel(1);
        let (read, owned) = (Arc::clone(&read), id.clone());
        std::thread::Builder::new()
            .name("input-read".into())
            .spawn(move || {
                let _ = tx.send(read(&owned, start, end));
            })
            .map_err(|e| format!("could not start the thread that reads it ({e})"))?;
        rx.recv_timeout(bound).unwrap_or_else(|e| {
            Err(match e {
                RecvTimeoutError::Timeout => format!(
                    "the datahub did not answer within {bound:?}, and a mount does not wait longer on \
                     the core's thread"
                ),
                RecvTimeoutError::Disconnected => "its read ended with no answer".to_string(),
            })
        })
    })
}

/// **What both arms hand their core for its inputs**: the opener over the datahub's read
/// ([`super::live_runtime::datahub_input_reader`]), bounded by [`INPUT_READ_BOUND`], through the profile's
/// `[inputs]` `remap`, and the keep it fills. Once the core is up the caller starts
/// [`spawn_datahub_lane_refresher`] (and, on the live arm, hands [`InputKeep::venue_series`] to the bar
/// keeper).
pub(super) fn datahub_inputs(
    remap: BTreeMap<String, String>,
) -> (Arc<InputKeep>, vike_core::InputOpener) {
    let keep = Arc::new(InputKeep::default());
    let read = within(super::live_runtime::datahub_input_reader(), INPUT_READ_BOUND);
    let opener =
        input_opener(Arc::clone(&keep), read, vike_model::now_ms, vike_catalog::kept_list, remap);
    (keep, opener)
}

/// The lane refresher of [`datahub_inputs`]' keep ([`spawn_lane_refresher`] every [`LANE_REFRESH_EVERY`]),
/// reading the datahub on its own thread, never the core's, and handing what is new to `handle`'s bar lane as
/// a seed. It ends with the core (a refused seed) or the keep; with no lane input it
/// reads nothing.
pub(super) fn spawn_datahub_lane_refresher(keep: &Arc<InputKeep>, handle: &vike_core::CoreHandle) {
    let bars = handle.bar_sender();
    spawn_lane_refresher(
        Arc::downgrade(keep),
        super::live_runtime::datahub_input_reader(),
        move |(venue, symbol, interval), seed| {
            bars.seed(vike_execution::BarSeed { venue, symbol, interval, bars: seed }).is_ok()
        },
        LANE_REFRESH_EVERY,
    );
}
