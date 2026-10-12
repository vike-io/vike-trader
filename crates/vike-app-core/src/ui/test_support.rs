use super::*;

/// One closed-only `BarSeries`.
pub(crate) fn series(bars: &[vike_model::Bar]) -> vike_exec::BarSeries {
    vike_exec::BarSeries { closed: std::sync::Arc::new(bars.to_vec()), forming: None }
}

/// One aggressive-buy `TradeTick` on `symbol`.
pub(crate) fn trade(symbol: &str, ts: i64, price: f64, size: f64) -> vike_model::TradeTick {
    vike_model::TradeTick {
        ts,
        local_ts: 0,
        price,
        size,
        is_buyer_maker: false,
        symbol: symbol.to_string(),
    }
}

/// A `ChartState` whose `bars` already carry the given open times — the `bar_ots` grid an
/// `OrderflowAgg` buckets trades into.
pub(crate) fn chart_with_ots(ots: &[i64]) -> model::ChartState {
    let mut cs = model::ChartState::default();
    cs.bars.extend(ots.iter().enumerate().map(|(i, &ot)| model::Bar {
        t: i as f64,
        ot,
        o: 1.0,
        h: 1.0,
        l: 1.0,
        c: 1.0,
        v: 0.0,
    }));
    cs
}

/// Total buy+sell volume an `OrderflowAgg` has booked across every bar/bucket — the single
/// number that says "did this aggregator receive the trades".
pub(crate) fn of_total(agg: &OrderflowAgg) -> f64 {
    agg.footprints().iter().flat_map(|b| b.cells.iter()).map(|c| c.buy_vol + c.sell_vol).sum()
}

pub(crate) fn keys(v: &[&str]) -> HashSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The whole mutable half, owned, so a test can call [`sync`] repeatedly against it.
#[derive(Default)]
pub(crate) struct State {
    pub charts: HashMap<String, model::ChartState>,
    pub aggs: HashMap<String, (String, String, TickVolAgg)>,
    pub of_aggs: HashMap<String, (String, String, OrderflowAgg)>,
    /// What the folded snapshot PUBLISHES — `CoreSyncState::published`'s owned half.
    pub published: Vec<crate::ui::series_follow::PublishedSeries>,
    pub last_seq: u64,
    pub last_direct_gen: u64,
    pub status: String,
    /// The market-data producer's tape-gap epochs — empty in every test that is not about a
    /// gap, which is what a caller with no market-data plane passes too.
    pub tape_gaps: HashMap<(String, String), u64>,
}

/// Run the real fold. `spawned`/`hidden` and the feed-status line are the only knobs a test
/// usually needs beyond `State` itself. The direct-bar store is unmounted (`None`) — the fat
/// local / thin observe shape; [`sync_direct`] is the third-mode twin.
pub(crate) fn sync(
    st: &mut State,
    snap: &vike_exec::CoreSnapshot,
    spawned: &HashSet<String>,
    hidden: &HashSet<String>,
    trades: &TradeStore,
    feed_status: &Mutex<String>,
) {
    sync_with(
        st,
        snap,
        spawned,
        hidden,
        trades,
        feed_status,
        None,
        crate::backend::split_plane::BarPlane::None,
        None,
    );
}

/// [`sync`] with the tape-gap epoch source SUPPLIED — for a test about the fold's ORDERING
/// rather than about a gap's effect. `State::tape_gaps` is a plain map, so a test using it can
/// only ever set the epoch BEFORE the fold runs, which is the well-ordered case and therefore
/// cannot see a reader that is consulted too early.
pub(crate) fn sync_gaps(
    st: &mut State,
    snap: &vike_exec::CoreSnapshot,
    spawned: &HashSet<String>,
    hidden: &HashSet<String>,
    trades: &TradeStore,
    feed_status: &Mutex<String>,
    tape_gaps: TapeGapEpochs<'_>,
) {
    sync_with(
        st,
        snap,
        spawned,
        hidden,
        trades,
        feed_status,
        None,
        crate::backend::split_plane::BarPlane::None,
        Some(tape_gaps),
    );
}

/// [`sync`] with the bar store MOUNTED under the **VENUE-FEEDS** plane — the third-mode shape,
/// where a `DIRECT_BAR_VENUES` venue's klines are venue truth.
pub(crate) fn sync_direct(
    st: &mut State,
    snap: &vike_exec::CoreSnapshot,
    spawned: &HashSet<String>,
    hidden: &HashSet<String>,
    trades: &TradeStore,
    feed_status: &Mutex<String>,
    direct_bars: &DirectBarStore,
) {
    sync_with(
        st,
        snap,
        spawned,
        hidden,
        trades,
        feed_status,
        Some(direct_bars),
        crate::backend::split_plane::BarPlane::VenueFeeds,
        None,
    );
}

/// [`sync`] with the store mounted under the **BACKEND-STORE** plane — the SHIPPED desktop's
/// shape (`split_plane::bar_plane(AppMode::ObserveOnly)`): the store holds bars read out of
/// the backend's own hist store, and claims exactly the series the live snapshot does not
/// publish.
pub(crate) fn sync_backend_store(
    st: &mut State,
    snap: &vike_exec::CoreSnapshot,
    spawned: &HashSet<String>,
    hidden: &HashSet<String>,
    trades: &TradeStore,
    feed_status: &Mutex<String>,
    direct_bars: &DirectBarStore,
) {
    sync_with(
        st,
        snap,
        spawned,
        hidden,
        trades,
        feed_status,
        Some(direct_bars),
        crate::backend::split_plane::BarPlane::BackendStore,
        None,
    );
}

fn sync_with(
    st: &mut State,
    snap: &vike_exec::CoreSnapshot,
    spawned: &HashSet<String>,
    hidden: &HashSet<String>,
    trades: &TradeStore,
    feed_status: &Mutex<String>,
    direct_bars: Option<&DirectBarStore>,
    plane: crate::backend::split_plane::BarPlane,
    tape_gaps: Option<TapeGapEpochs<'_>>,
) {
    // The default source is a CLONE of `State::tape_gaps`, taken here rather than borrowed,
    // so the rest of `st` stays freely `&mut`-borrowable below. A test that needs the epoch to
    // move DURING the fold passes its own reader through [`sync_gaps`].
    let snapshot = st.tape_gaps.clone();
    let from_state =
        move |v: &str, s: &str| snapshot.get(&(v.to_string(), s.to_string())).copied().unwrap_or(0);
    let fallback: TapeGapEpochs<'_> = &from_state;
    let tape_gaps: TapeGapEpochs<'_> = tape_gaps.unwrap_or(fallback);
    sync_from_core(
        CoreSyncInputs {
            snap,
            spawned,
            hidden,
            display_tz: DisplayTz::Utc,
            trades,
            feed_status,
            direct_bars,
            bar_plane: plane,
            tape_gaps,
        },
        CoreSyncState {
            charts: &mut st.charts,
            aggs: &mut st.aggs,
            of_aggs: &mut st.of_aggs,
            published: &mut st.published,
            last_seq: &mut st.last_seq,
            last_direct_gen: &mut st.last_direct_gen,
            status: &mut st.status,
        },
    );
}
