//! One CHART window per frame: draw, indicator fold, harvested outputs, picker, title-bar actions.

use super::*;
use vike_chart::render::candles::CandleInstance;
use vike_chart::{chart, indicators, model};

/// Window phase 3a — one CHART window: gather its inputs (compare overlays, orderflow, the
/// indicator fold), take its persisted state out of `w`, draw the title bar and `chart::draw`
/// inside `show_window`, then write the state back and apply what the frame harvested
/// ([`apply_chart_outputs`], [`drive_indicator_picker`], [`apply_title_actions`]). Returns the
/// title bar's actions for the window controls the loop applies to every kind.
pub(super) fn draw_chart_window(
    app: &mut App,
    ctx: &egui::Context,
    w: &mut workspace::WinState,
    wi: usize,
    fr: &WindowFrame,
    snap: &FrameSnapshot,
    out: &mut WindowOut,
) -> TitleActions {
    let wid = w.id;
    let wid64 = wid.value(); // chart sync seam (task B8): u64 identity for the group registry
    let is_max = w.maximized;
    let mut act = TitleActions::default();
    // The GPU candle layer's shape builder: `gpu_build` captures neither `app` nor anything else
    // (its two params supply everything it needs), so building it per chart window is free.
    let gpu_build = |instances: Vec<CandleInstance>, rect: egui::Rect| -> egui::Shape {
        eframe::egui_wgpu::Callback::new_paint_callback(
            rect,
            chart_gpu::CandleCallback { instances, rect },
        )
        .into()
    };
    let charts = &app.charts;
    // What the connected node publishes — a disjoint field from `app.sync_next` below, bound here
    // beside `charts` for the same split-borrow reason. Read-only in this phase; it decides
    // what the title-bar badge is entitled to claim and which rows the symbol picker offers.
    let published = &app.published_series;
    let of_aggs = &app.of_aggs; // SP2 orderflow (Task 7): read-only in this phase
    // Indicator favourites (part b): a MUTABLE handle to the global favourites set,
    // bound here (a distinct field from `app.charts` above, so the split borrow is
    // sound) so the per-window ƒx picker can star/unstar into it while `w` is `&mut`.
    let indicator_favs = &mut app.indicator_favs;

    let key = w.key();
    let chart = charts.get(&key);
    let bars_slice: &[model::Bar] = chart.map(|c| c.bars.as_slice()).unwrap_or(&[]);
    let last = chart.and_then(|c| c.bars.last().copied());
    let prev = w.hover;
    let style = w.style;
    let sync_group = w.sync_group;
    // Chart sync seam (task B8): this window's ghost crosshair/injected range for
    // THIS frame, resolved from last frame's harvest (`app.sync_prev`); `None` for
    // an ungrouped window (or a group nobody has broadcast into yet).
    let sync_in = sync_feed(&app.sync_prev, sync_group, wid64);
    let symbol = w.symbol.clone();
    let venue = w.venue.clone(); // cross-venue: title-bar prefix + selection reset
    let interval = w.interval.clone();
    // What the `● LIVE` badge is ENTITLED to say. It was an unconditional label, so an
    // empty grid over a node publishing nothing read as a live candle stream — see
    // `series_follow::ChartFeed`.
    let feed = series_follow::chart_feed(!bars_slice.is_empty(), published, &key);
    // C2a Task 4: gather this window's Compare overlays for the price pane. Clone
    // the symbol list (like `symbol`/`interval` above) so each `SeriesInput.symbol`
    // borrows THIS local rather than `w` — `w` is passed `&mut` into `show_window`
    // below, and the built `overlays` Vec must outlive that closure (it's read at
    // the `overlays: &overlays` feed site inside it). Each overlay's `ChartState` is
    // looked up in `app.charts` exactly like the primary `chart` above (same
    // immutable `charts` borrow, disjoint from the `&mut w`); a compare symbol not
    // yet synced is silently skipped (`filter_map`) until the ensure below populates
    // it. Empty `compare` ⇒ empty Vec ⇒ `overlays: &[]` — byte-identical to the
    // pre-Task-4 single-series render.
    //
    // ENSURE-SYNC: push each compare symbol onto `to_ensure` (idempotent, applied
    // after the loop by the existing `ensure_feed_on` fan-out) so its `ChartState`
    // gets created in `app.charts` AND live-synced by `sync_from_core` every frame
    // — mirrors the `of_wanted`/`to_ensure` collect-now/apply-after shape. Without
    // this the lookup above stays `None` and nothing overlays.
    let compare_syms: Vec<String> = w.compare.clone();
    for sym in &compare_syms {
        // Compare overlays are Binance-namespaced (`{sym}@{interval}` lookup below)
        // and always spot.
        out.to_ensure.push((
            workspace::DEFAULT_VENUE.to_string(),
            sym.clone(),
            interval.clone(),
            None,
        ));
    }
    let overlays: Vec<chart::SeriesInput> = compare_syms
        .iter()
        .enumerate()
        .filter_map(|(i, sym)| {
            // C2a Task 4 review Minor: never overlay the window's OWN
            // symbol. `add_compare` blocks ADDING self, but if the user
            // later switches the primary TO an already-compared symbol it
            // would otherwise overlay itself — filter it here (case-
            // insensitive, matching `add_compare`). Filtered INSIDE the
            // enumerate so the surviving overlays keep their add-order
            // color index (aligned with the title-bar chips).
            if sym.eq_ignore_ascii_case(&symbol) {
                return None;
            }
            let ckey = format!("{sym}@{interval}");
            charts.get(&ckey).map(|st| chart::SeriesInput {
                symbol: sym.as_str(),
                state: st,
                color: COMPARE_COLORS[i % COMPARE_COLORS.len()],
            })
        })
        .collect();
    // SP2 orderflow (Task 7): this window's current toggle state, read before the
    // `show_window` closure below (same up-front extraction as `style`/`sync_group`
    // above — `w` is reborrowed by `show_window`, so the closure can't reach it
    // directly). `of` is this chart's aggregator, if orderflow has ever been
    // requested for it (`None` ⇒ nothing to render yet); `fps` is its per-bar
    // footprint snapshot for THIS frame. Queue the (idempotent) subscribe+register
    // request for after the loop when the toggle/style combo wants orderflow —
    // `app.wins` is mutably borrowed for the whole loop, so `&mut App` work
    // (`ensure_trade_feed_on`, `of_aggs.entry`) can't happen here (mirrors
    // `to_ensure`'s collect-then-apply shape).
    let cvd_on = w.cvd_on;
    let profile_on = w.profile_on;
    let of_tick_size_w = w.of_tick_size;
    let of = of_aggs.get(&key);
    let fps = of.map(|(_, _, a)| a.footprints());
    if w.orderflow_on() {
        out.of_wanted.push((venue.clone(), symbol.clone(), key.clone(), of_tick_size_w));
    }
    // chart single-max default: reconcile Volume/CVD into the authored
    // sub-pane order (they are reorderable peers of study panes now),
    // then snapshot the unified order HERE — before `w.options` is taken
    // below, since `present_sub_panes()` reads `show_volume` from it. The
    // result is an owned Vec, so it survives the `show_window` borrow of `w`.
    w.sync_sub_panes();
    let sub_panes = w.present_sub_panes();
    let nav_in = w.nav.take();
    let mut inds = std::mem::take(&mut w.indicators);
    // Two-tier fold: the persistent instance advances over CLOSED bars
    // only (no-op / one on_bar / refold on structural change); the live
    // forming bar is previewed on a throwaway clone each frame, so a
    // tick never triggers a full-history refold.
    let n_closed = chart.map(|c| c.closed_len.min(c.bars.len())).unwrap_or(0);
    let (closed, _) = bars_slice.split_at(n_closed);
    fold_indicators(&mut inds, charts, bars_slice, n_closed, &interval, &mut out.to_ensure);
    // This frame's `ChartActions` — the hover, nav, scale/invert/options changes, the
    // sync outputs (harvested into the group registry AFTER `show_window` returns, see
    // `sync_harvest` in `apply_chart_outputs`), the CVD / volume ✕ clicks, the study
    // relocation (C1 Task 4) and the pane ↑/↓ reorder (Feature #1) — all applied to
    // `w` after `show_window` returns. Default (every field empty) when there is no
    // chart yet, which is what the per-field `None`/`false` locals this replaced were.
    let mut chart_out = chart::ChartActions::default();
    let mut follow = std::mem::take(&mut w.follow);
    let options = std::mem::take(&mut w.options);
    let mut settings = std::mem::take(&mut w.settings);
    let mut indicator_dialog = std::mem::take(&mut w.indicator_dialog);
    let requested_scale = w.scale;
    let requested_invert = w.invert;
    // T9: same `mem::take`-then-write-back dance as follow/options/
    // settings/indicator_dialog above — `w` itself is passed into
    // `show_window` below, so the closure can't hold `&mut w.panes`
    // directly (it would conflict with that borrow of the whole `w`).
    let mut panes = std::mem::take(&mut w.panes);
    // C1 Task 4: thread the authored per-study assignment map into
    // `chart::draw`. Same `mem::take`/write-back dance — `w` is
    // borrowed by `show_window`, so the closure can't hold
    // `&w.study_pane`. The unified sub-pane order (`sub_panes`) was
    // snapshotted above; `study_pane` itself is taken out and written
    // back after `show_window` — restored before `move_study` is
    // applied below.
    let study_pane = std::mem::take(&mut w.study_pane);
    // C2b Task 6: the authored compare-series pane read-model, threaded
    // into `chart::draw` with the SAME `mem::take`/write-back dance as
    // `study_pane` above (`w` is borrowed by `show_window`, so the closure
    // can't hold `&w.series_pane`). `present_series_panes()` is an OWNED
    // snapshot computed HERE while `series_pane` is still populated;
    // `series_pane` itself is taken out and written back UNCONDITIONALLY
    // after `show_window` (all paths). Empty until Task 9's menu moves a
    // compare symbol into its own pane ⇒ `series_panes` is `&[]` and the
    // render is byte-identical.
    let series_panes = w.present_series_panes();
    let series_pane = std::mem::take(&mut w.series_pane);
    // C2b Task 7: per-symbol secondary-axis assignment, taken out with
    // the same `mem::take`/write-back dance (the closure can't borrow
    // `w`). EMPTY until Task 9's "Pin to scale" menu populates it ⇒
    // `chart::draw` routes every overlay to the shared % axis ⇒
    // byte-identical.
    let series_scale = std::mem::take(&mut w.series_scale);
    // `_bounds`: a chart is a `fills` kind, so its body sizes itself to the available
    // rect and must NEVER be wrapped — this call site uses no wrapper at all, which
    // is what keeps the chart byte-identical. The other two `fills` kinds (DOM,
    // Polymarket) go through the TOOL call site instead, which DOES call
    // `BodyBounds::show`; that method's `fills` arm is what keeps them unwrapped
    // too. See `WinState::user_sized`.
    if workspace::show_window(ctx, w, fr.desktop, |ui, _bounds| {
        let (acts, drag) = chart_window::title_bar(
            ui,
            wid,
            &symbol,
            &venue,
            &interval,
            style,
            sync_group,
            is_max,
            cvd_on,
            profile_on,
            of_tick_size_w,
            &compare_syms, // C2a Task 4: current overlays → the Compare popup chips
            &series_pane,  // C2b Task 9: own-pane assignment → the per-chip menu
            &series_scale, // C2b Task 9: secondary-axis pins → the per-chip menu
            &snap.symbols_catalog, // symbol-search: live instrument universe → picker
            published,     // the node's own series → the picker's backend section
            fr.directory.as_deref(), // venue names (`venue.title`) → the picker's chips
            feed,          // what the badge may claim
        );
        act = acts;
        // The EMPTY-CHART SENTENCE. A chart with no bars used to be an empty grid with
        // an autoscaled axis and a `LIVE` badge; `ChartFeed::hint` says which of the
        // three ways it is empty. A `Live` chart paints nothing (the call returns).
        // ...plus the CHART-SEED sentence when there is one: which switch on which
        // server is why this pane is still blank. See `paint_empty_hint`'s `note`.
        series_follow::paint_empty_hint(ui, feed, fr.chart_seed_note.as_deref());
        // OHLC is now an in-plot overlay (chart::draw) so the window can shrink.
        let _ = (prev, last);
        if let Some(c) = chart {
            chart_out = chart::draw(
                ui,
                chart::ChartInputs {
                    state: c,
                    style,
                    nav: nav_in,
                    indicators: &inds,
                    // Tick-driven microstructure studies (vike-chart
                    // `studies.rs`). Empty until the app grows its own
                    // study menu + tick fan-in — an empty slice renders
                    // byte-identically to the pre-studies chart.
                    studies: &[],
                    follow: &mut follow,
                    options: &options,
                    settings: &mut settings,
                    indicator_dialog: &mut indicator_dialog,
                    scale: requested_scale, // chart-UX bundle T3: persisted per-window
                    invert: requested_invert, // TradingView "Invert scale": persisted per-window
                    panes: &mut panes,      // chart-UX bundle T9: persisted per-window
                    sync: sync_in,          // chart sync seam (task B7 shape, task B8 wiring)
                    // SP2 orderflow (Task 7): live-wired. `footprint` is `None` until
                    // this chart's aggregator exists AND has ingested at least one
                    // trade batch (a one-two-frame startup lag after the toggle
                    // flips) — `chart::draw` treats `None` as "nothing to render yet"
                    // exactly like the pre-Task-7 always-off wiring did.
                    // SP3 Task B #1: `footprints()` now hands out `Arc<Vec<..>>`
                    // (cached — see `bar_agg::OrderflowAgg::cache`'s doc), so
                    // `.as_deref()` alone only reaches `&Vec<FootprintBar>` (one
                    // level, `Arc`→`Vec`); `.as_slice()` takes the second step to
                    // the `&[FootprintBar]` `ChartInputs::footprint` wants.
                    footprint: fps.as_deref().map(|v| v.as_slice()),
                    // SP3 TB-fix (post-Task-B review finding, MEDIUM): the
                    // monotonic generation backing `footprint`'s content, so the
                    // chart-side CVD cache can key on it instead of the footprint
                    // slice's own (ABA-able) address — see vike-chart's
                    // `model.rs::CvdCacheKey` doc. `0` when there's no aggregator
                    // yet, mirroring `footprint: None`'s own default-off shape.
                    footprint_gen: of.map(|(_, _, a)| a.generation()).unwrap_or(0),
                    cvd_on,
                    profile_on,
                    // The AGG's actual bucketing width (whichever of user-pinned or
                    // its own default it landed on) — footprint/profile rendering
                    // must match the granularity the data was bucketed at, so this
                    // is never the chart-side "derive" 0.0 once an agg exists (see
                    // `bar_agg::OrderflowAgg::new`'s `<= 0.0 → 1.0` default).
                    of_tick_size: of_tick_size_w
                        .or_else(|| of.map(|(_, _, a)| a.tick_size()))
                        .unwrap_or(0.0),
                    // Chart single-max default: the authored UNIFIED
                    // sub-pane read-model (Volume/CVD/Study peers, in
                    // user order). `sub_panes` is `present_sub_panes()`'s
                    // owned snapshot; `study_pane_of` is the taken-out
                    // per-study assignment map. `chart::draw` gates each
                    // entry to visible panes internally.
                    sub_panes: &sub_panes,
                    study_pane_of: &study_pane,
                    // C2a Task 4: the Compare overlays gathered above (each a
                    // %-normalized line in the price pane). Empty ⇒ same `&[]`
                    // no-op the pre-Task-4 wiring fed.
                    overlays: &overlays,
                    // C2b Task 6: the authored own-pane compare-series model.
                    // `series_panes` is `present_series_panes()`'s owned
                    // snapshot; `series_pane_of` is the taken-out symbol→pane
                    // map. `chart::draw` reverse-looks-up each pane's symbol,
                    // finds its `SeriesInput` in `overlays`, and renders it as
                    // an absolute-price line in its own sub-pane; own-paned
                    // symbols are skipped by the price-overlay loop. Empty ⇒
                    // byte-identical.
                    series_panes: &series_panes,
                    series_pane_of: &series_pane,
                    // C2b Task 7: per-symbol secondary-axis ("Pin to
                    // scale ▸ Right") assignment. Empty ⇒ every overlay
                    // stays a %-line ⇒ byte-identical. Task 9's menu
                    // writes the pins back onto `w.series_scale`.
                    series_scale: &series_scale,
                    // GPU candle layer (GPU Phase 2, Task 3): wired only when the
                    // user has the toggle on AND the pipeline is actually
                    // available (`gpu_ok`) — off by default, so `None` here keeps
                    // the candle render byte-identical to the pre-Phase-2 egui/LOD
                    // path until both are true.
                    gpu_candles: (fr.gpu_render && fr.gpu_ok)
                        .then_some(&gpu_build as &dyn Fn(_, _) -> _),
                },
            );
        } else {
            vike_ui_theme::components::state::view(
                ui,
                vike_ui_theme::components::state::Load::Loading("Loading chart"),
            );
        }
        drag
    }) {
        out.user_moved_any = true;
    }
    w.follow = follow;
    w.options = options;
    w.settings = settings;
    w.indicator_dialog = indicator_dialog;
    w.panes = panes; // chart-UX bundle T9
    w.study_pane = study_pane; // C1 Task 4: restore before `move_study` apply below
    w.series_pane = series_pane; // C2b Task 6: restore (unconditional, all paths)
    w.series_scale = series_scale; // C2b Task 7: restore (unconditional, all paths)
    w.indicators = inds;
    apply_chart_outputs(
        w,
        chart_out,
        closed,
        &mut app.sync_next,
        &mut app.range_leader,
        sync_group,
        wid64,
    );
    drive_indicator_picker(
        ctx,
        w,
        act.open_picker,
        indicator_favs,
        &snap.symbols_catalog,
        bars_slice,
    );
    // A dashed venue chip in the symbol picker was clicked: ask for that venue's catalog list, by
    // the same entry the Data Manager's Refresh button uses, so the cooldown and every refusal
    // apply. The picker fetches nothing itself; this runs on its own thread and the merged catalog
    // arrives over `catalog_rx`. A refusal is logged, not drawn: the Data Manager row says why.
    if let Some(venue) = act.load_venue.take() {
        let (catalog, wake) = (std::sync::Arc::clone(&app.catalog), ctx.clone());
        let asked = catalog.request(&venue, vike_model::now_ms(), move || wake.request_repaint());
        if let Err(block) = asked {
            tracing::warn!("catalog: the picker's load of {venue} was refused: {block:?}");
        }
    }
    apply_title_actions(w, &mut act, wi, out);
    act
}

/// Chart phase — the two-tier indicator fold: each active indicator advances over the CLOSED bars
/// only (no-op / one on_bar / refold on structural change), and the live forming bar is previewed
/// on a throwaway clone each frame, so a tick never triggers a full-history refold. A foreign-source
/// study folds over ANOTHER symbol's bars and queues that feed on `to_ensure`.
fn fold_indicators(
    inds: &mut [indicators::Active],
    charts: &HashMap<String, model::ChartState>,
    bars_slice: &[model::Bar],
    n_closed: usize,
    interval: &str,
    to_ensure: &mut Vec<(String, String, String, Option<vike_model::AssetClass>)>,
) {
    let (closed, forming) = bars_slice.split_at(n_closed);
    for a in inds {
        // Foreign-source study (TradingView "symbol" input): fold over
        // ANOTHER symbol's bars re-indexed onto THIS chart's timeline
        // (same interval), so the series stays index-aligned to the
        // primary x-axis. `None` ⇒ the primary bars, byte-identical to
        // before this feature. The `.clone()` releases the immutable
        // `source_symbol` borrow before the `&mut a.update` below.
        match a.source_symbol.clone() {
            None => a.update(closed, forming.first()),
            Some(src) => {
                // Ensure-subscribe the foreign feed (idempotent, applied
                // after the loop like the Compare gather) so its ChartState
                // exists in `charts` and is live-synced every frame.
                // Foreign-source study symbols carry no asset class — always spot.
                to_ensure.push((src.venue.clone(), src.symbol.clone(), interval.to_string(), None));
                let fkey = workspace::series_key(&src.venue, &src.symbol, interval);
                if let Some(fcs) = charts.get(&fkey) {
                    // Re-index the foreign bars onto the primary timeline,
                    // then split at the PRIMARY's closed count so the
                    // forming preview lines up (foreign closed bars are
                    // immutable, so the two-tier fast path still holds).
                    let aligned = indicators::align_source_bars(bars_slice, &fcs.bars);
                    let split = n_closed.min(aligned.len());
                    let (fc, ff) = aligned.split_at(split);
                    a.update(fc, ff.first());
                } else {
                    a.update(&[], None); // not synced yet → show nothing
                }
            }
        }
    }
}

/// Chart phase — apply what `chart::draw` harvested onto `w`, now that its persisted state is back
/// in place: the hover and nav, the CVD / volume ✕ clicks, the study relocation and pane reorder,
/// scale / invert / options changes, the sync-group harvest, the live indicator edit and the
/// indicator removal. `closed` is the chart's closed-bar prefix the indicator refold runs over.
fn apply_chart_outputs(
    w: &mut workspace::WinState,
    chart_out: chart::ChartActions,
    closed: &[model::Bar],
    sync_next: &mut HashMap<u8, GroupFrame>,
    range_leader: &mut HashMap<u8, u64>,
    sync_group: Option<u8>,
    wid64: u64,
) {
    w.hover = chart_out.hovered;
    w.nav = chart_out.nav_out;
    // SP2 orderflow (Task 7): the CVD pane's own ✕ was clicked this frame — write
    // the persisted toggle back to off (mirrors `remove_uid`'s "the caller owns
    // the source of truth" shape; see `ChartActions::cvd_toggle`'s doc).
    if chart_out.cvd_toggle {
        w.cvd_on = false;
    }
    // Volume-as-indicator: the volume pane ✕ turns off show_volume
    // (re-add via the ƒx picker's "Volume" entry). `w.options` was
    // restored above, so this direct write persists like cvd.
    if chart_out.volume_remove {
        w.options.show_volume = false;
    }
    // C1 Task 4: apply the ••• "Move to" relocation onto the authored
    // pane model (`w.study_pane`/`w.pane_order` are both intact here —
    // `study_pane` was written back above, `pane_order` was never
    // taken). `move_study` re-points the study's pane and drops any
    // now-empty pane; like the sibling toggles this is a direct field
    // write (no dirty flag exists — `persist::save` reads live state
    // whenever the user explicitly saves).
    if let Some((uid, target)) = chart_out.move_study {
        w.move_study(uid, target);
    }
    // Feature #1: apply the pane ↑/↓ reorder onto `w.pane_order`
    // (intact here — never taken). Direct write, same shape as
    // `move_study` above. Volume/CVD/Study are peers now.
    if let Some((pane, up)) = chart_out.reorder_pane {
        w.reorder_pane(pane, up);
    }
    if let Some(sm) = chart_out.scale_change {
        w.scale = sm;
    }
    if let Some(iv) = chart_out.invert_change {
        w.invert = iv; // TradingView "Invert scale" toggle from the price-axis menu
    }
    if let Some(o) = chart_out.options_change {
        w.options = o; // T6: OK in the settings dialog committed a new palette/flags
    }
    // Chart sync seam (task B8): harvest this frame's sync outputs into the group
    // registry (no-op when `sync_group` is `None` — see `sync_harvest`'s doc).
    sync_harvest(
        sync_next,
        range_leader,
        sync_group,
        wid64,
        chart_out.interacted,
        chart_out.visible_ts,
        chart_out.hover_ts,
    );
    // T8: apply a live indicator edit onto the real `Active` (the
    // chart's `indicators` slice was immutable). set_params refolds
    // only when the params actually moved (a debounced, O(history)
    // refold, keyed off pointer-release); colours+widths always
    // re-apply (cheap). Restores from the snapshot on Cancel.
    if let Some((
        uid,
        vike_chart::IndicatorEdit {
            params,
            source,
            lines,
            show_bands,
            bands,
            show_ob_os_fill,
            ob_fill,
            os_fill,
            visible,
        },
    )) = chart_out.indicator_edit
        && let Some(a) = w.indicators.iter_mut().find(|a| a.uid == uid)
    {
        // Source selector (chart source selector): a changed Source
        // needs a refold, exactly like a param change. Set it FIRST
        // so whichever refold runs below folds off the new series.
        let source_changed = a.source != source;
        a.source = source;
        if a.params != params {
            a.set_params(params, closed); // rebuilds + refolds (new source applied)
        } else if source_changed {
            a.recompute_full(closed); // params unchanged → just refold off the new source
        }
        for (line, (col, wid, vis, ls)) in a.outputs.iter_mut().zip(&lines) {
            line.color = *col;
            line.width = *wid;
            line.visible = *vis;
            line.line_style = *ls;
        }
        a.show_bands = show_bands;
        // Per-level band colour/show (RSI "Style" parity): `bands`
        // is index-aligned to `a.bands` (both seeded from
        // `spec.bands`); update value untouched (static level).
        for (band, (_v, col, show)) in a.bands.iter_mut().zip(&bands) {
            band.color = *col;
            band.show = *show;
        }
        a.show_ob_os_fill = show_ob_os_fill;
        a.ob_fill = ob_fill;
        a.os_fill = os_fill;
        a.visible = visible;
    }
    if let Some(uid) = chart_out.remove_uid {
        w.remove_indicator(uid);
    }
}

/// Chart phase — the ƒx indicator picker: toggle it when the title bar asked, draw its popup, and
/// add the indicator it returns (routed to the picker-chosen pane).
fn drive_indicator_picker(
    ctx: &egui::Context,
    w: &mut workspace::WinState,
    open_picker: bool,
    indicator_favs: &mut Vec<String>,
    symbols_catalog: &vike_catalog::Catalog,
    bars_slice: &[model::Bar],
) {
    if open_picker {
        w.picker_open = !w.picker_open;
    }
    if let Some(name) = tool_views::fx_picker_popup(ctx, w, indicator_favs, symbols_catalog) {
        // Part (a): route the add to the picker-chosen target pane
        // (reusing `move_study`); `PaneTarget::Auto` == today's behavior.
        let target = w.picker_target;
        w.add_indicator_to(name, bars_slice, target);
    }
}

/// Chart phase — apply the title bar's actions to `w`: style, the Compare popup (add / remove /
/// own-pane / overlay / scale pin), the orderflow popup, a new symbol or interval (which queue
/// their feed ensure), the sync-group chip and the clone request. Each `Option<String>` is `take`n
/// because the caller still reads the window-control bools off the same `act` afterwards.
fn apply_title_actions(
    w: &mut workspace::WinState,
    act: &mut TitleActions,
    wi: usize,
    out: &mut WindowOut,
) {
    if let Some(s) = act.set_style {
        w.style = s;
    }
    // C2a Task 4: Compare popup — add/remove a %-overlay series. `add_compare`
    // dedups, ignores the window's own symbol, and (on the FIRST overlay)
    // auto-switches the scale to Percent so the overlay is visible; the per-frame
    // gather above ensure-subscribes each compare symbol's feed. Applied AFTER the
    // `scale_out` write above so a first-overlay auto-Percent isn't clobbered by a
    // same-frame manual scale toggle (they never realistically collide). The next
    // frame's gather picks up the mutated `w.compare`.
    if let Some(s) = act.add_compare.take() {
        w.add_compare(&s);
    }
    if let Some(s) = act.remove_compare.take() {
        w.remove_compare(&s);
    }
    // C2b Task 9: apply the per-chip Move-to / Pin-to-scale menu onto the
    // (already-restored above) `w.series_pane`/`w.series_scale`. Each menu
    // item sources `sym` from `w.compare` (the chip loop only iterates
    // compare), so `move_series_to_new_pane` is always called with a live
    // compare symbol — closing Task 5 review Minor-2 by construction.
    if let Some(s) = act.series_to_own_pane.take() {
        w.move_series_to_new_pane(&s);
    }
    if let Some(s) = act.series_to_overlay.take() {
        w.overlay_series(&s);
    }
    if let Some((s, assign)) = act.set_series_scale.take() {
        // Percent = the default ⇒ drop the pin (keep the map clean, so a
        // later remove/re-add defaults correctly); Right = an explicit pin.
        if assign == vike_chart::scale::ScaleAssign::Percent {
            w.series_scale.shift_remove(&s);
        } else {
            w.series_scale.insert(s, assign);
        }
    }
    // SP2 orderflow (Task 7): title-bar Orderflow popup checkboxes/field.
    if let Some(v) = act.cvd_on {
        w.cvd_on = v;
    }
    if let Some(v) = act.profile_on {
        w.profile_on = v;
    }
    if let Some(v) = act.of_tick_size {
        w.of_tick_size = v;
    }
    if let Some(s) = act.new_symbol.take() {
        // C2 tidy (FIX 1): drop any Compare chip that now matches the new
        // primary symbol. `add_compare` already blocks ADDING the window's own
        // symbol, but switching the PRIMARY onto an already-compared symbol
        // used to leave a dangling chip — the price-pane render already
        // self-filters an overlay matching `symbol` (see the
        // `eq_ignore_ascii_case` guard above), so the chip was stale (harmless
        // to the chart, but confusing/removable-for-nothing in the popup).
        w.remove_compare(&s);
        w.symbol = s;
        // Cross-venue: a search hit (or quick-pick) carries its venue, set here
        // ATOMICALLY with the symbol so `w.key()`/`ensure_feed_on` route to the
        // right feed. `new_venue` is always `Some` when `new_symbol` is (both set
        // at every selection site); default to Binance if a caller ever omits it.
        w.venue = act.new_venue.take().unwrap_or_else(|| workspace::DEFAULT_VENUE.to_string());
        // Feed-routing slice 1: set atomically with symbol/venue so `w.asset_class`
        // never lags the symbol it describes (`take()` leaves `None` for the next
        // frame, mirroring `new_venue`'s take-and-default idiom).
        w.asset_class = act.new_asset_class.take();
        // THE OPERATOR CHOSE THIS SERIES, so `series_follow::follow_backend` may never
        // retarget the window again — see `WinState::series_pinned`. ⚠ The interval
        // arm below sets the OTHER flag: picking a resolution is not picking a series,
        // and conflating the two is what disabled adoption for good on one menu click.
        w.series_pinned = true;
        w.retitle();
        // Auto-scale the new symbol: re-engage follow-live + y-autofit and
        // force a full-range refit of both axes (else a same-bar-count swap
        // inherits the old symbol's zoom/pan — the "doesn't auto-scale" bug).
        w.follow.on_series_change();
        out.to_ensure.push((w.venue.clone(), w.symbol.clone(), w.interval.clone(), w.asset_class));
    }
    if let Some(iv) = act.new_interval.take() {
        w.interval = iv;
        // ⚠ THE RESOLUTION, NOT THE SERIES — `interval_pinned`, never `series_pinned`.
        // This line wrote the latter until 2026-09-15, and because NOTHING in the tree
        // ever clears either flag, one click on the interval menu permanently disabled
        // backend adoption for that window. Dropping the write instead is worse and
        // was measured as such (the chart silently snaps back to the daemon's
        // interval); the fix is the split. See `WinState::interval_pinned`.
        w.interval_pinned = true;
        w.retitle();
        w.follow.on_series_change(); // same refit on a timeframe swap
        out.to_ensure.push((w.venue.clone(), w.symbol.clone(), w.interval.clone(), w.asset_class));
    }
    if let Some(g) = act.new_group {
        w.sync_group = g; // task B8: chip click — None -> 1 -> 2 -> 3 -> 4 -> None
    }
    if act.clone {
        out.to_clone.push(wi);
    }
}
