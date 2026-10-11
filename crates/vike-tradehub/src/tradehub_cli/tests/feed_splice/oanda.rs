//! The OANDA data-only splice: exec stays paper and scripted frames reach the strategy.

use super::*;

// ─── The OANDA data-only case (the first credentialed-data GAP conversion) ────────────────────

/// The label the scripted oanda lanes stamp — the same const the real lanes stamp
/// (`crates/bridges/oanda/src/market_feed.rs`'s `VENUE`).
const OANDA_VENUE: &str = "oanda";

/// One wire-faithful `candles` response (UNIX datetime format, `price=M` midpoints — the grammar
/// `vike_oanda::parse_candles`' own fixtures pin): `n` COMPLETE ascending 1m candles from
/// `first_bucket_s`, plus the still-forming tail candle a real response always carries (dropped
/// by the lossless-lane decode, kept here for wire fidelity). Prices are decimal STRINGS and
/// `time` epoch-seconds strings, exactly as the venue serves them.
fn candles_response(first_bucket_s: i64, n: i64) -> serde_json::Value {
    let mut candles: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            let px = format!("{:.5}", 1.09 + 0.0001 * i as f64);
            serde_json::json!({
                "complete": true,
                "volume": 120,
                "time": format!("{}.000000000", first_bucket_s + 60 * i),
                "mid": { "o": px, "h": px, "l": px, "c": px }
            })
        })
        .collect();
    candles.push(serde_json::json!({
        "complete": false,
        "volume": 7,
        "time": format!("{}.000000000", first_bucket_s + 60 * n),
        "mid": { "o": "1.09100", "h": "1.09100", "l": "1.09100", "c": "1.09100" }
    }));
    serde_json::json!({ "instrument": "EUR_USD", "granularity": "M1", "candles": candles })
}

/// One wire-faithful pricing-stream `PRICE` line (the grammar
/// `vike_oanda::market_data`'s own `price_frame` fixture pins): two-sided, best-first ladders,
/// UNIX datetime format — the decoder's ACCEPT path.
fn oanda_price_line() -> String {
    r#"{"type":"PRICE","time":"60.500000000","instrument":"EUR_USD","bids":[{"price":"1.09000","liquidity":10000000}],"asks":[{"price":"1.09010","liquidity":10000000}],"status":"tradeable","tradeable":true}"#
        .to_string()
}

/// The scripted stand-in [`FeedCtors::oanda`] hands the REAL arm: records every subscribe the
/// arm makes (the LABEL half) and serves each lane one batch of documented-grammar frames
/// through the venue's own decode (the DATA half), on [`FeedRegistry`] threads with the
/// production stop/join bookkeeping. The venue's real derivations run too —
/// `vike_oanda::granularity` must map the subscribed interval and
/// `vike_oanda::to_oanda_instrument` names the wire instrument — so the scripted frames
/// are FOR the subscribed series the way the venue serves what was asked for, and the emissions
/// keep the real lanes' labelling rule: the [`OANDA_VENUE`] const plus the SUBSCRIBED series,
/// never the frame's own `EUR_USD` spelling.
struct ScriptedOandaFeed {
    sink: Arc<dyn LiveDataSink>,
    subs: Arc<Mutex<Vec<(&'static str, String)>>>,
    registry: FeedRegistry,
}

impl DataClient for ScriptedOandaFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.subs.lock().expect("subs").push(("bars", format!("{symbol}@{interval}")));
        // The venue's REAL derivations — the same calls `bars_main`'s poll makes. `granularity`
        // was proven mappable by `oanda_plan` before the arm ran; a double that skipped it could
        // serve bars for an interval the venue has no candle series for.
        let gran = vike_oanda::granularity(interval)
            .expect("the wired interval maps — oanda_plan proved it before the arm ran");
        assert_eq!(gran, "M1", "this scripted lane serves 1m candles");
        let instrument = vike_oanda::to_oanda_instrument(symbol);
        assert_eq!(instrument, "EUR_USD", "the wired symbol lowers to the venue instrument form");
        let sink = Arc::clone(&self.sink);
        let (series, interval) = (symbol.to_string(), interval.to_string());
        self.registry
            .spawn(format!("scripted-oanda-{series}@{interval}"), move |_stop| {
                // The venue's REAL lossless-lane decode over the wire-faithful response, emitted
                // exactly as `bars_main` emits fresh closes (no seed: the runtime's `BarSeed` arm
                // stores history and drives no strategy, the deribit case's verdict). Four closes
                // are enough to trigger the mounted `buy_hold` (its first event) AND fill its
                // market order (paper fills are next-bar, through the core's `on_bar` seam).
                for b in vike_oanda::parse_candles(&candles_response(60, 4)) {
                    sink.close_bar(OANDA_VENUE, &series, &interval, b);
                }
            })
            .map_err(|e| LiveDataError::Subscribe(e.to_string()))
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subs.lock().expect("subs").push(("quotes", symbol.to_string()));
        let sink = Arc::clone(&self.sink);
        let series = symbol.to_string();
        self.registry
            .spawn(format!("scripted-oanda-{series}-quotes"), move |_stop| {
                // The venue's REAL pricing decode + the real lane's labelling rule: relabel with
                // the SUBSCRIBED series and stamp receive time (`quotes_main`'s fold does both).
                let v: serde_json::Value =
                    serde_json::from_str(&oanda_price_line()).expect("wire-faithful PRICE line");
                match vike_oanda::market_data::decode_pricing_frame(&v) {
                    vike_oanda::market_data::PricingFrame::Quote(mut q) => {
                        q.symbol = series.clone();
                        q.local_ts = vike_model::now_ms();
                        sink.quote(OANDA_VENUE, &series, q);
                    }
                    other => panic!("the fixture must decode as a quote, got {other:?}"),
                }
            })
            .map_err(|e| LiveDataError::Subscribe(e.to_string()))
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // The real client's caps refusal (`live_data.trades = false` — no trade tape exists on
        // this venue), kept so an arm padded out to "look like its neighbours" fails the mount
        // loudly here exactly as it would in production.
        let _ = symbol;
        Err(LiveDataError::Unsupported("oanda serves no trade tape"))
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Same caps refusal as trades (`live_data.book = false` — an unsequenced snapshot ladder
        // is not an L2 book).
        let _ = symbol;
        Err(LiveDataError::Unsupported("oanda serves no L2 book"))
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

/// The data-only test's [`FeedCtors`] impl: the oanda method hands back the scripted double AND
/// records the credentials the REAL arm handed it — the feed half of the declaration's contract
/// (the plan resolved the store's keys BEFORE the withhold took them from exec). Every other
/// venue keeps the trait's production default (this test never reaches one — its plan is
/// oanda's).
pub(super) struct ScriptedOandaCtors {
    pub(super) subs: Arc<Mutex<Vec<(&'static str, String)>>>,
    pub(super) got_creds: Arc<Mutex<Option<(String, String)>>>,
}

impl FeedCtors for ScriptedOandaCtors {
    fn oanda(
        &self,
        config: &vike_oanda::OandaConfig,
        sink: Arc<dyn LiveDataSink>,
    ) -> Box<dyn DataClient + Send> {
        *self.got_creds.lock().expect("got_creds") =
            Some((config.api_token.clone(), config.account_id.clone()));
        Box::new(ScriptedOandaFeed {
            sink,
            subs: Arc::clone(&self.subs),
            registry: FeedRegistry::new(),
        })
    }
}

/// A seam whose `account` table arms oanda's default account at `demo` (one ACTIVE row), with the
/// default machine policy and nothing else.
///
/// ⚠ **No account row would make both tests below pass for the wrong reason.** With none, EVERY
/// venue mounts `paper` (`NoAccountRow`), and a paper-tier account returns the paper engine above
/// the arm entirely — so the mutant these tests hunt (a build whose `data_only` declaration
/// silently arms exec anyway) would die on the missing row rather than on the withhold, and the
/// `live_venues.is_empty()` record would prove nothing about the declaration. Arming oanda is what
/// leaves `data_only` as the only thing standing between the fake credentials and a live exec
/// client.
pub(super) fn oanda_armed_policy() -> super::SeamPolicy {
    super::SeamPolicy::armed(&[("oanda", vike_config::VenueMode::Demo)])
}

/// The credentialed store: FAKE oanda keys in a plain map — never real ones, never the real store —
/// with `token` as the API token. Spelled through the venue's own key-name authority
/// (`vike_oanda::oanda_env_var_names`) so a key-grid rename reddens here.
pub(super) fn fake_oanda_store(token: &str) -> HashMap<String, String> {
    let (key_k, acct_k) =
        vike_oanda::oanda_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    HashMap::from([(key_k, token.to_string()), (acct_k, "101-004-0000000-001".to_string())])
}

/// An operator risk budget for the data-only mounts: budget fields only, so the venue-owned grid
/// fields stay `None` and the paper engines' `VenueFetched` merge is untouched, with caps far
/// above the scripted 1-unit fill.
pub(super) fn fill_sized_budget() -> vike_model::ProfileRisk {
    vike_model::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(10_000_000.0),
        ..vike_model::ProfileRisk::default()
    }
}

/// THE DATA-ONLY SEAM, deterministically, over the REAL mount path: a `data_only = true` oanda
/// profile + a FAKE-key credentialed map through [`super::live_mount_with`] — every venue's
/// plan gate, the credential WITHHOLD, the real wired-market `build_node`, the real oanda wire
/// arm — must (1) keep exec on the paper fallback, asserted against `build_node`'s own
/// `live_venues` ARMING RECORD (the state `vike_mount::make_engine` writes when it constructs a
/// real exec client — never the banner), (2) hand the FEED the store's credentials (the ctor
/// records what the plan resolved), and (3) still splice: scripted venue frames through the
/// arm's own wiring reach the mounted strategy, whose order the PAPER book fills — a live exec
/// client ignores the core's `on_bar` fill seam, so the fill is behavioural proof of (1) on top
/// of the record.
///
/// The arming assert comes FIRST: it is the seam's core property, and the mutant it exists to
/// catch — a build where the declaration silently arms exec anyway — must fail HERE, on the
/// record, before any timing-dependent wait can muddy the verdict. (Under that mutant the fake
/// keys would spawn a real exec client whose auth then fails against the practice host — the
/// fill wait would eventually fail too, but the record fails first and names the venue.)
///
/// Residual this test accepts, shared with every live-path paper mount: `make_engine`'s paper
/// client resolves the process-wide HALT sentinel (`vike_bridge_core::halt`'s
/// `halt_path_from_env`), so a box with an armed kill switch at the resolved path would refuse
/// the fill — the CI runners' checkouts carry none, and a HALT file there would be halting the
/// box's real daemons first.
#[test]
fn a_data_only_oanda_mount_keeps_exec_paper_and_scripted_frames_reach_the_strategy() {
    let symbol = super::wired_symbol_for("oanda").expect("build_node mounts oanda");

    // The operator's own spelling of the seam — parsed + validated through the daemon's real
    // profile load path, so this test also covers the declaration's parse.
    let mounts = vec![super::buy_hold_mount("oanda", true)];

    let vars = fake_oanda_store("fake-data-only-token");

    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let got_creds: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let ctors = ScriptedOandaCtors { subs: Arc::clone(&subs), got_creds: Arc::clone(&got_creds) };

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");
    // An operator risk BUDGET is supplied — not because the green path needs one (every venue is
    // paper, and `require_live_risk_budget` asks nothing of a paper mount), but so the MUTANT
    // this test exists to catch dies on the ARMING ASSERT below and not one gate earlier: a
    // build whose declaration silently arms exec would otherwise refuse the whole mount at
    // `vike_mount::require_live_risk_budget` (no budget for a live venue), which is a catch, but
    // of the wrong property — proven live on the CI box before this budget was added (the mutant's
    // first red was `MissingRiskBudget`, not the record assert).
    let (handle, teardown, live_venues, live_locks) = super::seam_mount(
        mounts,
        Some(fill_sized_budget()),
        &oanda_armed_policy(),
        vars,
        lock_dir.path(),
        &ctors,
    )
    .expect("the data-only live mount stands up — no real store, no network");

    // (1) THE ARMING STATE — the seam's core property, asserted on the record itself.
    assert!(
        live_venues.is_empty(),
        "the `data_only` declaration must keep EVERY venue on the paper fallback — build_node \
         recorded a live exec client for {live_venues:?}, so the withhold did not hold"
    );

    // (1b) …AND NO ACCOUNT LOCK WAS CLAIMED FOR IT. The B11 claims read the map AFTER the
    // withhold, so a `data_only` venue — which this daemon deliberately leaves on paper — must not
    // take the sentinel that refuses a second live process on that account. Asserted on the
    // sentinel FILES as well as on the count: a claim's only trace is the file it creates, and the
    // count alone would still pass a build that claimed and then dropped.
    assert!(
        live_locks.is_empty(),
        "a `data_only` venue arms nothing, so it must claim no live-account lock — {} claimed",
        live_locks.len()
    );
    let sentinels = sentinels(lock_dir.path());
    assert!(
        sentinels.is_empty(),
        "no LIVE-<venue>.lock may exist for a mount that armed nothing: {sentinels:?}"
    );

    // (2) The feed half: the arm handed the constructor the credentials the plan resolved from
    // the store — the same keys the withhold took away from exec.
    assert_eq!(
        got_creds.lock().expect("got_creds").clone(),
        Some(("fake-data-only-token".to_string(), "101-004-0000000-001".to_string())),
        "the feed must authenticate with the store's own credentials — the plan resolved them \
         BEFORE the withhold, and the arm must thread them through"
    );

    // (3) THE SPLICE + the paper-exec behaviour: scripted candle closes through the real arm
    // reach the mounted `buy_hold` on its own dispatch key, and the PAPER book fills its order
    // through the core's `on_bar` seam.
    let filled = wait_until(10, || {
        handle
            .snapshot()
            .orders
            .iter()
            .any(|o| o.venue == "oanda" && o.symbol == symbol && o.filled_qty > 0.0)
    });
    assert!(
        filled,
        "no filled oanda order within 10s: no scripted venue event crossed the arm's own wiring \
         into the paper book (subscriptions the arm made: {:?}; orders seen: {:?})",
        subs.lock().expect("subs"),
        handle.snapshot().orders
    );

    // The LABEL half: the arm subscribed THIS mount's series — the two verbs the venue serves,
    // the mount's own key, once each (trades/book are caps refusals the arm must not call).
    let got = subs.lock().expect("subs").clone();
    let expect: Vec<(&'static str, String)> =
        vec![("bars", format!("{symbol}@1m")), ("quotes", symbol.to_string())];
    assert_eq!(got, expect, "the arm's subscription set IS the mount's own dispatch key");

    super::tear_down(handle, teardown);
}
