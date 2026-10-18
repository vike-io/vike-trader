//! The OANDA data-only splice: exec stays paper and scripted frames reach the strategy.

use super::*;

// ─── The OANDA data-only case (the first credentialed-data GAP conversion) ────────────────────

/// The label the scripted oanda lanes stamp — the venue's own slug.
const OANDA_VENUE: &str = "oanda";

/// The data-only test's [`FeedCtors`] impl: `client` hands the arm the scripted double (oanda serves
/// quotes and bars only — no trade tape, no book) and records the subscriptions the arm makes. The
/// feed's credentials are no part of this constructor any more: the dial carries none, and the
/// gatewayhub resolves the account behind a `data_only` venue itself.
pub(super) struct ScriptedOandaCtors {
    pub(super) subs: Arc<Mutex<Vec<(&'static str, String)>>>,
}

impl FeedCtors for ScriptedOandaCtors {
    fn strat(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        Err(format!("{venue}: this scripted constructor builds client() feeds only"))
    }

    fn client(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        // Hyperliquid's, deribit's, ctrader's, ig's and ibkr's PRICE-ONLY dials (wired beside the mounts' feeds,
        // no mount names them) are refused: they are not what this test scripts, and a refusal there only leaves
        // that venue without a foreign-symbol price.
        if matches!(venue, "hyperliquid" | "deribit" | "ctrader" | "ig" | "ibkr") {
            return Err(format!("{venue}: this scripted constructor serves oanda only"));
        }
        assert_eq!((venue, network), (OANDA_VENUE, None), "the oanda arm asks for oanda");
        Ok(Box::new(ScriptedFeed::new(OANDA_VENUE, false, sink, Arc::clone(&self.subs))))
    }
}

/// A seam whose `account` table holds oanda's default account at `paper` (one ACTIVE row, id 1),
/// holding FAKE oanda credentials — never real ones, never the real store — with `token` as its
/// `API_KEY` field: the credentialed store, planted by FIELD on the account row (decision 0156
/// item 6), with the default machine policy and nothing else.
///
/// `paper`, not `demo`: these mounts are `data_only`, so oanda's exec credentials are withheld and a
/// `demo` account would hold none and NOT RUN (decision 0156) — the account that stands behind a
/// data-only venue's strategy is a paper one, and it holds the demo login for the FEED alone
/// (`AccountDirectory::data_feed_credentials`).
///
/// ⚠ **No account row would make both tests below fail for the wrong reason**: with none, the
/// strategy's mount is refused outright (`NoAccountRow`) before the feed is ever consulted.
pub(super) fn oanda_armed_policy(token: &str) -> super::SeamPolicy {
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    book.insert_account(1, "API_KEY", token);
    book.insert_account(1, "ACCOUNT_ID", "101-004-0000000-001");
    super::SeamPolicy::armed(&[("oanda", vike_model::AccountTier::Paper)]).holding(book)
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
/// profile + a FAKE-key account through [`super::live_runtime_with`] — every venue's
/// plan gate, the credential WITHHOLD, the real wired-market `assemble_runtime`, the real oanda wire
/// arm — must (1) keep exec on the paper fallback, asserted against `assemble_runtime`'s own
/// `live_venues` ARMING RECORD (the state `vike_tradehub_core::make_engine` writes when it constructs a
/// real exec client — never the banner), (2) claim no live-account lock for it, and (3) still splice: scripted venue frames through the
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
/// client resolves the process-wide HALT sentinel (`vike_boot::halt`'s
/// `halt_path_from_env`), so a box with an armed kill switch at the resolved path would refuse
/// the fill — the CI runners' checkouts carry none, and a HALT file there would be halting the
/// box's real daemons first.
#[test]
fn a_data_only_oanda_mount_keeps_exec_paper_and_scripted_frames_reach_the_strategy() {
    let symbol = super::wired_symbol("oanda").expect("assemble_runtime mounts oanda");

    // The operator's own spelling of the seam — parsed + validated through the daemon's real
    // profile load path, so this test also covers the declaration's parse.
    let mounts = vec![super::buy_hold_mount("oanda", true)];

    // The DEPLOYMENT map carries no account's credential: oanda's ride its account row.
    let vars = HashMap::new();

    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let ctors = ScriptedOandaCtors { subs: Arc::clone(&subs) };

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");
    // An operator risk BUDGET is supplied — not because the green path needs one (every venue is
    // paper, and `require_live_risk_budget` asks nothing of a paper mount), but so the MUTANT
    // this test exists to catch dies on the ARMING ASSERT below and not one gate earlier: a
    // build whose declaration silently arms exec would otherwise refuse the whole mount at
    // `vike_tradehub_core::require_live_risk_budget` (no budget for a live venue), which is a catch, but
    // of the wrong property — proven live on the CI box before this budget was added (the mutant's
    // first red was `MissingRiskBudget`, not the record assert).
    let (handle, teardown, live_venues, late_arm, _not_running) = super::seam_live_runtime(
        mounts,
        Some(fill_sized_budget()),
        &oanda_armed_policy("fake-data-only-token"),
        vars,
        lock_dir.path(),
        &ctors,
    )
    .expect("the data-only live mount stands up — no real store, no network");

    // (1) THE ARMING STATE — the seam's core property, asserted on the record itself.
    assert!(
        live_venues.is_empty(),
        "the `data_only` declaration must keep EVERY venue on the paper fallback — assemble_runtime \
         recorded a live exec client for {live_venues:?}, so the withhold did not hold"
    );

    // (1b) …AND NOTHING WAITS FOR THE GATEWAYHUB, NOR WAS A LOCK CLAIMED HERE. A `data_only` venue is
    // deliberately left without exec, which is its declaration and not an outage, so no late-arm
    // plan exists for it. And this process claims no live-account sentinel any more (the
    // gatewayhub does, at the lease): asserted on the sentinel FILES, a claim's only trace.
    assert!(late_arm.is_none(), "a `data_only` venue is not waiting for the gatewayhub");
    let sentinels = sentinels(lock_dir.path());
    assert!(sentinels.is_empty(), "this process claims no LIVE-<venue>.lock: {sentinels:?}");

    // (2) The feed half is NOT credentialed here: the dial the arm asked for names the venue and
    // carries no credential (`ScriptedOandaCtors::client` takes none). The feed keeps the keys exec
    // loses at the gatewayhub, which reads the `paper` account behind a `data_only` venue itself.

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
