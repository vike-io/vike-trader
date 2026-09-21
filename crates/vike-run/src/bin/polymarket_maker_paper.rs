//! Paper A-S market-maker mounted on a LIVE Polymarket market — the maker quotes/trades on the real
//! tick feed and fills against the PAPER exchange (no creds, no real money). Behind the `polymarket`
//! feature.
//!
//! ## What it does
//! Picks a Polymarket market — `--auto` (universe selection: the most-liquid tradeable market, no
//! name needed), by name via the Gamma catalog (`--query`), or a `--token-id` directly — reads its
//! `end_date` → A-S `resolution_ts`, builds the A-S [`vike_mm::SpreadMaker`], and mounts it on the
//! PRODUCTION live core ([`vike_core::spawn_core`]) with the paper exchange as the `ExecutionClient`
//! and the real Polymarket [`vike_polymarket::Feeds`] as the tick source. Identical mount to the
//! offline scripted test — only the feed differs.
//!
//! ## Running it (Polymarket is US-geo-blocked → route via the Dublin EU AWS host)
//! The Polymarket CLOB (Gamma catalog read + market WS) is geo-blocked from the US. Run this binary
//! ON (or egressing through) the user's Dublin EU AWS host — the SAME route the Gamma catalog and the
//! live market smokes use (`vike_polymarket::exec::agent`'s proxy). No API keys are needed: the market
//! channel is public/unauthenticated and the mount is PAPER (it never signs or sends an order).
//!
//! ```sh
//! # on the Dublin host (or with its egress), from the repo root:
//! cargo run -p vike-run --features polymarket --bin polymarket_maker_paper -- \
//!     --query "bitcoin" --duration-secs 300
//! # or pin an exact outcome token (skips catalog discovery); optional resolution ts (epoch-ms):
//! cargo run -p vike-run --features polymarket --bin polymarket_maker_paper -- \
//!     --token-id 71321045679252212594626385532706912750332728571942532289631379312455583992563 \
//!     --resolution-ts-ms 1793491200000 --duration-secs 300
//! ```
//!
//! It logs the position / working quotes / paper fills / realized PnL every few seconds, then flushes
//! the last synth bar and shuts the core down cleanly. The LOCAL (US) machine can still compile and
//! run the offline test (`cargo test -p vike-run`) — only THIS live path needs the Dublin route.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink};
use vike_model::ToxicityParams;
use vike_polymarket::{
    ActivityTradeSink, Feeds, RtdsActivityFeed, RtdsConfig, RtdsFeed, UniversePolicy, WalletClass,
    WalletClassMap,
};
use vike_run::{MakerSink, ToxicityEmitter, build_paper_maker_core, live};

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it).
#[derive(Debug)]
struct Args {
    query: Option<String>,
    token_id: Option<String>,
    /// `--auto`: pick the most-liquid market via universe selection (no name/token needed).
    auto: bool,
    /// `--min-liquidity`: liquidity floor for `--auto` (default from `UniversePolicy::default`).
    min_liquidity: Option<f64>,
    resolution_ts_ms: Option<i64>,
    duration_secs: u64,
    qty: Option<f64>,
    page_limit: usize,
    /// Liquidity-rewards opt-in weight (`--reward-weight`, default from `POLY_REWARD_WEIGHT`/`0.0`).
    /// `0.0` ⇒ rewards OFF (byte-identical mount); `> 0` turns on reward-aware quoting on a rewarded
    /// market. Ignored on the `--token-id` path (no market metadata to read rewards from).
    reward_weight: f64,
    /// "Option B" cross-symbol UNDERLYING opt-in (`--underlying <sym>`, e.g. `btcusdt`) — the RTDS
    /// crypto-prices symbol whose spot the A-S maker anchors its fair mid / ATM guard on. `None`
    /// (default) ⇒ no RTDS feed and the underlying-anchored knobs stay inert (byte-identical mount).
    underlying: Option<String>,
    /// A-S underlying-blend WEIGHT (`--underlying-weight`, default `0.5`) applied only when
    /// `--underlying` is set: `(1−w)·book_mid + w·p_up(underlying)`.
    underlying_weight: f64,
    /// A-S window length in SECONDS (`--window-secs`, default `0.0`) applied only when `--underlying`
    /// is set — the up/down market's rolling window (e.g. `300` for a 5-minute market). `0.0` leaves
    /// the blend inert (no window ⇒ no model probability), so set it to activate the anchor.
    window_secs: f64,
    /// A-S ATM settlement-guard scale (`--atm-blackout-scale`, default `0.0`) applied only when
    /// `--underlying` is set — widens the near-resolution blackout near a coin-flip. `0.0` = off.
    atm_blackout_scale: f64,
    /// 5c FLOW-TOXICITY guard opt-in (`--toxicity`). When set, the maker gets [`ToxicityParams`] AND a
    /// [`ToxicityEmitter`] over the RTDS activity tape is started to feed it. Unset (default) ⇒ no
    /// activity feed, no `ToxicityParams`, byte-identical mount.
    toxicity: bool,
    /// Toxic-side WIDEN knob (`--tox-widen`, default `1.0`): widen the toxic side by
    /// `tox · widen · half_spread`. Only used when `--toxicity` is set.
    tox_widen: f64,
    /// Toxic-side SIZE-CUT knob (`--tox-size-cut`, default `0.5`): scale the toxic side's size by
    /// `(1 − tox · size_cut)`. Only used when `--toxicity` is set.
    tox_size_cut: f64,
    /// Toxicity aggregator decay WINDOW in seconds (`--tox-window-secs`, default `30.0` → `window_ms`).
    /// Only used when `--toxicity` is set.
    tox_window_secs: f64,
    /// Path to a JSON wallet→class map (`--wallet-classes`): `{ "0xabc": "sharp", "0xdef": "whale" }`.
    /// Absent ⇒ an empty map ⇒ every wallet is `Unknown` ⇒ never toxic ⇒ the guard stays inert.
    wallet_classes: Option<String>,
}

/// What a successful parse produced. `-h`/`--help` is NOT an error — see `vike-tradehub`'s
/// identical `Parsed::Help` shape, the in-tree precedent this follows (a non-zero `--help` breaks
/// `set -e` and any wrapper that checks a status). This binary has no version flag, so there is no
/// `Version` variant to add.
#[derive(Debug)]
enum Parsed {
    Args(Box<Args>),
    Help,
}

/// The usage line, shared between the `--help` success path and the error path in `main`.
const USAGE: &str = "usage: polymarket_maker_paper (--auto [--min-liquidity <n>] | --query <name> | \
     --token-id <id> [--resolution-ts-ms <ms>]) \
     [--duration-secs <n>] [--qty <shares>] [--page-limit <n>] [--reward-weight <w>] \
     [--underlying <sym> [--underlying-weight <w>] [--window-secs <s>] \
     [--atm-blackout-scale <s>]] \
     [--toxicity [--tox-widen <w>] [--tox-size-cut <c>] [--tox-window-secs <s>] \
     [--wallet-classes <path>]]";

/// Parse one numeric flag's value, folding the FLAG NAME and the offending token into any error —
/// the replacement for the bare `.map_err(|e| format!("{e}"))` every numeric arm used to carry,
/// which reported only `invalid float literal` (or `invalid digit found in string`) with no
/// indication of which of this bin's eighteen risk knobs was mistyped.
fn parse_numeric<T: std::str::FromStr>(flag: &str, raw: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    raw.parse::<T>().map_err(|e| format!("{flag} {raw:?}: {e}"))
}

/// The liquidity-rewards opt-in weight: the `--reward-weight <w>` flag overrides this, else
/// `POLY_REWARD_WEIGHT` from the process env, else `0.0` (OFF — a default mount quotes exactly as
/// before). A positive value turns on reward-aware quoting on a rewarded market (see
/// `vike_run::live::reward_params_from`). Env reads stay in the bin, per the crate convention.
///
/// ⚠ **The `env::var` STAYS HERE, in the bin.** `src/bin/*.rs` is `Layer::Binary` for the settings
/// registry (`vike_ops::settings::SETTINGS` carries this variable's row against `vike-run`); moving
/// the read into a library file would make it a `Layer::Library` row, and that work-list is a
/// shrink-only ratchet (`crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`). Only the
/// PARSE is split out, below, so the silent-default rule is testable without touching process state.
fn reward_weight_from_env() -> f64 {
    parse_reward_weight(std::env::var("POLY_REWARD_WEIGHT").ok().as_deref())
}

/// The `POLY_REWARD_WEIGHT` value → weight rule, as a pure function.
///
/// ⚠ **Anything unparseable degrades SILENTLY to `0.0` — rewards OFF.** That is the pre-existing
/// behaviour and it is deliberately unchanged here, but it is worth knowing which way it fails:
/// `POLY_REWARD_WEIGHT=0,5` (a decimal comma) turns reward-aware quoting off rather than on, with no
/// diagnostic. `0.0` is also what an ABSENT variable means, so the two are indistinguishable.
fn parse_reward_weight(raw: Option<&str>) -> f64 {
    raw.and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0)
}

/// Parse one wallet-class label (case-insensitive) from the `--wallet-classes` JSON map. Anything
/// unrecognized maps to [`WalletClass::Unknown`] (never toxic) rather than erroring — a forgiving
/// parse so a typo degrades to inert, not a crash.
fn parse_wallet_class(s: &str) -> WalletClass {
    match s.trim().to_ascii_lowercase().as_str() {
        "sharp" => WalletClass::Sharp,
        "whale" => WalletClass::Whale,
        "retail" => WalletClass::Retail,
        _ => WalletClass::Unknown,
    }
}

/// Load the `--wallet-classes` JSON file — a flat `{ "0xwallet": "sharp"|"whale"|"retail", … }` object
/// — into a [`WalletClassMap`]. `Err` (a missing file or malformed JSON) is surfaced to the caller,
/// which degrades to an empty (inert) map rather than aborting the mount.
fn load_wallet_classes(path: &str) -> Result<WalletClassMap, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let table: std::collections::HashMap<String, String> =
        serde_json::from_str(&raw).map_err(|e| format!("parse {path}: {e}"))?;
    let by_wallet =
        table.into_iter().map(|(wallet, class)| (wallet, parse_wallet_class(&class))).collect();
    Ok(WalletClassMap::new(by_wallet))
}

/// The process-argv entry point: [`parse_args_from`] over `std::env::args().skip(1)`, with the
/// reward-weight DEFAULT resolved from the environment here — the two ambient inputs this parser
/// has, both decided in the binary and neither read below.
fn parse_args() -> Result<Parsed, String> {
    parse_args_from(std::env::args().skip(1), reward_weight_from_env())
}

/// **THE RULE: a valued flag must be GIVEN a value, and a token beginning with `--` is a FLAG,
/// never a value.** The same rule `vike_backfill::cli::flag_value` spells for the backfill bins;
/// this crate cannot depend on that one (nothing may — it pulls every bridge crate), so the
/// spelling is repeated rather than shared.
///
/// It is `--`, not a bare `-`, and that half is load-bearing HERE more than anywhere: a SINGLE-dash
/// token must still reach the numeric arm, because that arm is what judges it. `--min-liquidity -1`
/// is a negative floor this bin accepts verbatim (its consumer treats it as a no-op filter), and
/// `--qty -5` is refused BY `--qty`'s OWN bound rather than by this predicate — see
/// `qty_now_refuses_negative_while_the_already_clamped_knobs_stay_unchecked`. A
/// `starts_with('-')` test here would take that judgement away from the arm that owns it and
/// report every signed value as a malformed command line instead.
///
/// What this closes is the one defect on this parser that cost money: `--wallet-classes --toxicity`
/// used to set the wallet-class FILENAME to `--toxicity` and leave `args.toxicity` FALSE, and
/// `main` gates the entire 5c flow-toxicity path on that bool — so the maker quoted with no toxic-
/// side widening and no size cut while the operator believed the guard was armed, and the file
/// named `--toxicity` was never opened (it is read only inside `if args.toxicity`), so nothing
/// anywhere complained. `--query --auto` is the same shape, selecting a market literally named
/// `--auto` instead of auto-picking the most-liquid one.
///
/// ⚠ **It never fires on `-h`/`--help`, and the one place the two rules MEET is a value slot.**
/// `--help` takes no value, so it never reaches this function; but `--wallet-classes --help` does,
/// and it is refused as a swallow rather than short-circuiting to [`Parsed::Help`] — the
/// `--wallet-classes` arm matches the token in FLAG position first and asks for its value. That is
/// the intended order: printing usage and exiting 0 would silently discard the `--wallet-classes`
/// the operator typed, which is the exact shape of failure this rule exists to end. They still see
/// the usage — `main` prints it on the error path — plus a line naming both tokens. `--help` FIRST,
/// or after a fed flag, still short-circuits to a success.
fn flag_value(flag: &str, next: Option<String>) -> Result<String, String> {
    match next {
        None => Err(format!("{flag} needs a value")),
        Some(v) if v.starts_with("--") => {
            Err(format!("{flag} needs a value, but the next argument is another flag ({v})"))
        }
        Some(v) => Ok(v),
    }
}

/// Parse an already-`argv[0]`-stripped argument stream. PURE — no environment, no filesystem:
/// `reward_weight_default` is what [`reward_weight_from_env`] resolved, and `--reward-weight`
/// overrides it.
///
/// Every valued flag resolves through [`flag_value`], so a flag that was MENTIONED and not fed is
/// an error — a missing value, or a value that is itself a flag. An unmentioned flag still keeps
/// its default. The numeric arms then hand what survives to [`parse_numeric`], so the two checks
/// compose: `--tox-size-cut --toxicity` is a swallow, `--tox-size-cut lots` is a numeric error, and
/// both name the flag.
fn parse_args_from(
    argv: impl Iterator<Item = String>,
    reward_weight_default: f64,
) -> Result<Parsed, String> {
    let mut a = Args {
        query: None,
        token_id: None,
        auto: false,
        min_liquidity: None,
        resolution_ts_ms: None,
        duration_secs: 120,
        qty: None,
        page_limit: 100,
        reward_weight: reward_weight_default,
        underlying: None,
        underlying_weight: 0.5,
        window_secs: 0.0,
        atm_blackout_scale: 0.0,
        toxicity: false,
        tox_widen: 1.0,
        tox_size_cut: 0.5,
        tox_window_secs: 30.0,
        wallet_classes: None,
    };
    let argv: Vec<String> = argv.collect();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        let mut val = || {
            i += 1;
            flag_value(flag, argv.get(i).cloned())
        };
        match flag {
            "--query" => a.query = Some(val()?),
            "--token-id" => a.token_id = Some(val()?),
            "--auto" => a.auto = true,
            "--min-liquidity" => {
                let v = val()?;
                a.min_liquidity = Some(parse_numeric(flag, &v)?);
            }
            "--resolution-ts-ms" => {
                let v = val()?;
                a.resolution_ts_ms = Some(parse_numeric(flag, &v)?);
            }
            "--duration-secs" => {
                let v = val()?;
                a.duration_secs = parse_numeric(flag, &v)?;
            }
            "--qty" => {
                let v = val()?;
                let q: f64 = parse_numeric(flag, &v)?;
                // **FIX (defect #3): a negative maker order size is provably nonsensical** — `cfg.qty`
                // is multiplied straight into `bid_qty`/`ask_qty` in `vike_mm::quote::requote`
                // (`self.cfg.qty * bid_mult`, `bid_mult`/`ask_mult` themselves floored at `0.0` in
                // `skew_multipliers`) with NOTHING downstream to floor a negative size, so a negative
                // `qty` would reach `broker.submit_limit_tagged` as a signed order size. `0.0` is kept
                // legal (an inert, zero-size mount is not nonsensical, just idle).
                if q < 0.0 {
                    return Err(format!(
                        "{flag} must not be negative (a maker order size can't be signed): {q}"
                    ));
                }
                a.qty = Some(q);
            }
            "--page-limit" => {
                let v = val()?;
                a.page_limit = parse_numeric(flag, &v)?;
            }
            "--reward-weight" => {
                let v = val()?;
                a.reward_weight = parse_numeric(flag, &v)?;
            }
            "--underlying" => a.underlying = Some(val()?),
            "--underlying-weight" => {
                let v = val()?;
                a.underlying_weight = parse_numeric(flag, &v)?;
            }
            "--window-secs" => {
                let v = val()?;
                a.window_secs = parse_numeric(flag, &v)?;
            }
            "--atm-blackout-scale" => {
                let v = val()?;
                a.atm_blackout_scale = parse_numeric(flag, &v)?;
            }
            "--toxicity" => a.toxicity = true,
            "--tox-widen" => {
                let v = val()?;
                a.tox_widen = parse_numeric(flag, &v)?;
            }
            "--tox-size-cut" => {
                let v = val()?;
                a.tox_size_cut = parse_numeric(flag, &v)?;
            }
            "--tox-window-secs" => {
                let v = val()?;
                a.tox_window_secs = parse_numeric(flag, &v)?;
            }
            "--wallet-classes" => a.wallet_classes = Some(val()?),
            "-h" | "--help" => return Ok(Parsed::Help),
            other => return Err(format!("unknown flag {other:?}")),
        }
        i += 1;
    }
    if a.query.is_none() && a.token_id.is_none() && !a.auto {
        return Err("provide --auto, --query <name>, or --token-id <id>".to_string());
    }
    Ok(Parsed::Args(Box::new(a)))
}

fn main() -> ExitCode {
    // `project_dir`: default the rolling trace file to `<project>/settings/state/logs` instead of
    // vike-log's `<exe_dir>/logs` last resort. `$VIKE_LOG_DIR` still wins.
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "poly-maker-paper".to_string(),
        project_dir: std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_log_dir(&cwd)),
        ..Default::default()
    });
    let args = match parse_args() {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Args(a)) => a,
        Err(e) => {
            tracing::error!("bad args: {e}");
            tracing::error!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    // Resolve the market → mount config. Both paths go through the SAME `MakerMountConfig` the offline
    // test uses; only discovery differs (catalog search vs a pinned token).
    let mut cfg = match (&args.token_id, &args.query) {
        // Raw --token-id: look the token up in the catalog and use the SAME builder the
        // --query/--auto paths use (config_for_market → real tick + rewards + resolution). Not found
        // (or fetch failed) ⇒ the bare config_for_token fallback (default tick; the maker still
        // self-corrects from the first L2 book grid).
        (Some(token), _) => match live::market_for_token(token, args.page_limit) {
            Some(market) => live::config_for_market(&market, args.reward_weight)
                .unwrap_or_else(|_| live::config_for_token(token, args.resolution_ts_ms)),
            None => live::config_for_token(token, args.resolution_ts_ms),
        },
        (None, Some(query)) => match live::select_market(query, args.page_limit) {
            Ok(market) => {
                tracing::info!(
                    slug = market.slug,
                    question = market.question,
                    end_date = market.end_date,
                    volume = market.volume,
                    "selected market"
                );
                match live::config_for_market(&market, args.reward_weight) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("cannot build config: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            Err(e) => {
                tracing::error!("market selection failed: {e}");
                return ExitCode::FAILURE;
            }
        },
        // --auto: universe selection auto-picks the most-liquid tradeable market (parse_args
        // guarantees this arm only when `--auto` was passed).
        (None, None) => {
            let mut policy = UniversePolicy { max_markets: 1, ..Default::default() };
            if let Some(floor) = args.min_liquidity {
                policy.min_liquidity = floor;
            }
            match live::select_top_market(&policy, args.page_limit) {
                Ok(market) => {
                    tracing::info!(
                        slug = market.slug,
                        question = market.question,
                        liquidity = market.liquidity,
                        volume = market.volume,
                        "auto-selected most-liquid market (universe selection)"
                    );
                    match live::config_for_market(&market, args.reward_weight) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::error!("cannot build config: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("auto market selection failed: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };
    if let Some(q) = args.qty {
        cfg.qty = q;
    }
    // "Option B" cross-symbol underlying opt-in: when `--underlying` is set, declare the reference
    // symbol on the mount AND raise the A-S underlying-anchored knobs (all operator-tunable). Unset ⇒
    // no underlying, knobs stay inert (byte-identical mount).
    if let Some(underlying) = args.underlying.clone() {
        cfg.underlying_symbol = Some(underlying);
        cfg.as_params.underlying_weight = args.underlying_weight;
        cfg.as_params.window_secs = args.window_secs;
        cfg.as_params.atm_blackout_scale = args.atm_blackout_scale;
    }
    // 5c flow-toxicity opt-in: give the maker the guard knobs. The activity-tape PRODUCER that feeds
    // it is started after the mount is up (below). Unset ⇒ `cfg.toxicity` stays `None` (byte-identical).
    if args.toxicity {
        cfg.toxicity = Some(ToxicityParams { widen: args.tox_widen, size_cut: args.tox_size_cut });
    }
    tracing::info!(
        token = cfg.token_id,
        resolution_ts = ?cfg.as_params.resolution_ts,
        qty = cfg.qty,
        tick_size = cfg.tick_size,
        reward = ?cfg.reward,
        underlying = ?cfg.underlying_symbol,
        underlying_weight = cfg.as_params.underlying_weight,
        window_secs = cfg.as_params.window_secs,
        toxicity = ?cfg.toxicity,
        "mounting A-S paper maker"
    );

    // Spawn the paper maker core (the PRODUCTION runtime), then bridge the REAL feed onto it.
    let mount = build_paper_maker_core(&cfg);
    let sink = Arc::new(MakerSink::new(
        &mount.handle,
        cfg.venue.clone(),
        cfg.token_id.clone(),
        cfg.interval.clone(),
        cfg.interval_ms,
    ));
    let mut feeds = Feeds::new(Arc::clone(&sink) as Arc<dyn LiveDataSink>, || {});
    let sub = match feeds.subscribe_book(&cfg.token_id) {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("subscribe_book failed: {e:?}");
            mount.handle.shutdown_and_join();
            return ExitCode::FAILURE;
        }
    };
    // "Option B": when an underlying is configured, ALSO start the RTDS crypto-prices feed for it
    // against the SAME MakerSink. Its `mark_tick("polymarket", "<sym>", …)` publishes onto the core's
    // mark lane, where `drain_market` routes it into the maker's `on_mark` (a DIFFERENT symbol than
    // the token the book feed drives). Skipped entirely when no underlying is configured.
    let mut rtds: Option<RtdsFeed> = match &cfg.underlying_symbol {
        Some(underlying) => {
            let mut feed = RtdsFeed::new(
                RtdsConfig::crypto_prices(underlying.clone()),
                Arc::clone(&sink) as Arc<dyn LiveDataSink>,
            );
            match feed.start() {
                Ok(_id) => {
                    tracing::info!(underlying, "started RTDS underlying-price feed");
                    Some(feed)
                }
                Err(e) => {
                    tracing::error!("RTDS underlying feed start failed: {e:?}");
                    None
                }
            }
        }
        None => None,
    };
    // 5c flow-toxicity PRODUCER: when `--toxicity` is set, load the wallet-class map, build a
    // `ToxicityEmitter` over the SAME core handle the MakerSink uses (`mount.handle.tick_sender()` —
    // the identical accessor `MakerSink::new` obtains its tick lane from), and start an
    // `RtdsActivityFeed` (platform-wide `activity`/`trades` tape) feeding it. Skipped entirely when
    // `--toxicity` is unset ⇒ no activity socket, no flow updates (byte-identical mount).
    let mut tox_feed: Option<RtdsActivityFeed> = None;
    if args.toxicity {
        let classes = match &args.wallet_classes {
            Some(path) => match load_wallet_classes(path) {
                Ok(map) => {
                    tracing::info!(wallets = map.len(), path, "loaded wallet-class map");
                    map
                }
                Err(e) => {
                    tracing::error!(
                        "wallet-classes load failed ({e}) — using an empty (inert) map"
                    );
                    WalletClassMap::default()
                }
            },
            None => {
                tracing::warn!(
                    "--toxicity set without --wallet-classes: every wallet is Unknown ⇒ never toxic ⇒ \
                     the guard stays inert"
                );
                WalletClassMap::default()
            }
        };
        let window_ms = (args.tox_window_secs * 1_000.0) as i64;
        let emitter = Arc::new(ToxicityEmitter::new(
            cfg.venue.clone(),
            cfg.token_id.clone(),
            window_ms,
            classes,
            mount.handle.tick_sender(),
        ));
        let mut feed = RtdsActivityFeed::new(
            RtdsConfig::activity_trades(),
            Arc::clone(&emitter) as Arc<dyn ActivityTradeSink>,
        );
        match feed.start() {
            Ok(_id) => {
                tracing::info!(
                    window_ms,
                    widen = args.tox_widen,
                    size_cut = args.tox_size_cut,
                    "started RTDS activity-tape flow-toxicity producer"
                );
                tox_feed = Some(feed);
            }
            Err(e) => tracing::error!("RTDS activity feed start failed: {e:?}"),
        }
    }
    tracing::info!(
        "subscribed to the live book — running the paper maker for {}s",
        args.duration_secs
    );

    // Run: periodically report the mount's state (position / working quotes / fills / PnL).
    let deadline = Instant::now() + Duration::from_secs(args.duration_secs);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(5));
        let snap = mount.handle.snapshot();
        let position =
            snap.positions.iter().find(|p| p.symbol == cfg.token_id).map_or(0.0, |p| p.size);
        let working = snap.orders.iter().filter(|o| o.filled_qty < o.qty).count();
        let fills = mount.fills.lock().expect("fills mutex").len();
        tracing::info!(
            position,
            working_quotes = working,
            fills,
            realized_pnl = snap.portfolio.realized_pnl,
            fees = snap.portfolio.fees_paid,
            "paper maker state"
        );
        if let Some(fault) = &snap.fault {
            tracing::error!("core faulted: {fault}");
            break;
        }
    }

    // Teardown: stop the feeds (book + any RTDS underlying), flush the last synth window (so its
    // fills land), join the core.
    feeds.unsubscribe(sub);
    feeds.shutdown();
    if let Some(feed) = rtds.as_mut() {
        feed.shutdown();
    }
    if let Some(feed) = tox_feed.as_mut() {
        feed.shutdown();
    }
    sink.flush_bar();
    let fills = mount.fills.lock().expect("fills mutex").len();
    let snap = mount.handle.snapshot();
    tracing::info!(
        total_fills = fills,
        realized_pnl = snap.portfolio.realized_pnl,
        fees = snap.portfolio.fees_paid,
        "paper maker done — shutting down"
    );
    mount.handle.shutdown_and_join();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reward-weight default a test uses when it does not care about that knob. Deliberately NOT
    /// `0.0`: a fixture that used the real default could not tell "the parser carried the default
    /// through" from "the parser reset it".
    const SENTINEL_REWARD: f64 = 0.125;

    fn args(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// A parse with the sentinel reward default and a market selector already supplied. Unwraps
    /// through `Parsed::Args` (every call site here supplies `--auto` and no `--help`, so `Help`
    /// is unreachable) so every existing call site — written against a plain `Result<Args, String>`
    /// before the `Parsed::Help` outcome existed — keeps compiling unchanged.
    fn parse(v: &[&str]) -> Result<Args, String> {
        let mut argv = vec!["--auto"];
        argv.extend_from_slice(v);
        match parse_args_from(args(&argv), SENTINEL_REWARD) {
            Ok(Parsed::Args(a)) => Ok(*a),
            Ok(Parsed::Help) => panic!("unexpected --help outcome from {v:?}"),
            Err(e) => Err(e),
        }
    }

    /// The parsed `Args`, or a panic naming what came back instead — for the handful of tests below
    /// that call [`parse_args_from`] directly (without the `parse` helper's `--auto` prefix) and
    /// expect a run rather than help.
    fn run_args_raw(v: &[&str]) -> Args {
        match parse_args_from(args(v), SENTINEL_REWARD) {
            Ok(Parsed::Args(a)) => *a,
            other => panic!("expected a run from {v:?}, got {other:?}"),
        }
    }

    /// **The MARKET-SELECTOR gate**: one of `--auto` / `--query` / `--token-id` is required, because
    /// `main`'s `(None, None)` arm is reachable ONLY through `--auto` and would otherwise auto-pick
    /// the most-liquid market for an operator who asked for none.
    #[test]
    fn a_market_selector_is_required_and_all_three_satisfy_it() {
        let e = parse_args_from(args(&["--duration-secs", "300"]), SENTINEL_REWARD)
            .expect_err("no selector must not run");
        assert!(e.contains("--auto") && e.contains("--query") && e.contains("--token-id"), "{e}");
        assert!(parse_args_from(args(&["--auto"]), SENTINEL_REWARD).is_ok());
        assert!(parse_args_from(args(&["--query", "bitcoin"]), SENTINEL_REWARD).is_ok());
        assert!(parse_args_from(args(&["--token-id", "713210"]), SENTINEL_REWARD).is_ok());
    }

    /// **Every one of the eighteen flags lands in its own field.** Written as one invocation rather
    /// than eighteen tests because the failure this guards against is a MISWIRING — two flags
    /// assigning the same field, or a copy-pasted arm assigning its neighbour's — which only a
    /// simultaneous parse with eighteen DISTINCT values can catch.
    #[test]
    fn all_eighteen_flags_land_in_their_own_field() {
        let a = run_args_raw(&[
            "--query",
            "bitcoin",
            "--token-id",
            "713210",
            "--auto",
            "--min-liquidity",
            "1500.5",
            "--resolution-ts-ms",
            "1793491200000",
            "--duration-secs",
            "300",
            "--qty",
            "42.5",
            "--page-limit",
            "250",
            "--reward-weight",
            "0.75",
            "--underlying",
            "btcusdt",
            "--underlying-weight",
            "0.6",
            "--window-secs",
            "300",
            "--atm-blackout-scale",
            "1.5",
            "--toxicity",
            "--tox-widen",
            "2.25",
            "--tox-size-cut",
            "0.8",
            "--tox-window-secs",
            "45",
            "--wallet-classes",
            "/w/classes.json",
        ]);
        assert_eq!(a.query.as_deref(), Some("bitcoin"));
        assert_eq!(a.token_id.as_deref(), Some("713210"));
        assert!(a.auto);
        assert_eq!(a.min_liquidity, Some(1500.5));
        assert_eq!(a.resolution_ts_ms, Some(1_793_491_200_000));
        assert_eq!(a.duration_secs, 300);
        assert_eq!(a.qty, Some(42.5));
        assert_eq!(a.page_limit, 250);
        assert_eq!(a.reward_weight, 0.75, "--reward-weight beats the env-derived default");
        assert_eq!(a.underlying.as_deref(), Some("btcusdt"));
        assert_eq!(a.underlying_weight, 0.6);
        assert_eq!(a.window_secs, 300.0);
        assert_eq!(a.atm_blackout_scale, 1.5);
        assert!(a.toxicity);
        assert_eq!(a.tox_widen, 2.25);
        assert_eq!(a.tox_size_cut, 0.8);
        assert_eq!(a.tox_window_secs, 45.0);
        assert_eq!(a.wallet_classes.as_deref(), Some("/w/classes.json"));
    }

    /// **The DEFAULTS, written out longhand.** Each of these is what the maker quotes with when the
    /// operator names no flag, and each one is a number that reaches the A-S model — so a silent
    /// change to any of them is a silent change to a live-feed maker's behaviour. The two BOOLEANS
    /// are the load-bearing pair: both off means a byte-identical mount (no RTDS feed, no
    /// `ToxicityParams`, no activity socket).
    #[test]
    fn the_defaults_are_the_byte_identical_mount() {
        let a = run_args_raw(&["--auto"]);
        assert_eq!(a.duration_secs, 120);
        assert_eq!(a.page_limit, 100);
        assert_eq!(a.qty, None, "absent ⇒ the mount config's own qty is kept");
        assert_eq!(a.min_liquidity, None, "absent ⇒ UniversePolicy::default's floor");
        assert_eq!(a.reward_weight, SENTINEL_REWARD, "the env-derived default is carried through");
        assert_eq!(a.underlying, None);
        assert_eq!(a.underlying_weight, 0.5);
        assert_eq!(a.window_secs, 0.0, "0 leaves the underlying blend inert");
        assert_eq!(a.atm_blackout_scale, 0.0, "0 = the settlement guard is off");
        assert!(!a.toxicity, "no --toxicity ⇒ no activity socket and no ToxicityParams");
        assert_eq!(a.tox_widen, 1.0);
        assert_eq!(a.tox_size_cut, 0.5);
        assert_eq!(a.tox_window_secs, 30.0);
        assert_eq!(a.wallet_classes, None);
    }

    /// **Every NUMERIC flag refuses garbage rather than silently keeping its default.** This is the
    /// dangerous class on a maker: a `--tox-size-cut` that fell back to `0.5` when the operator asked
    /// for `0.9` quotes larger size into flow they had classified as toxic, and a `--qty` that fell
    /// back keeps the config's own size. Every numeric arm is checked in ONE loop — no count is
    /// written down, because a count here would rot the first time a knob is added.
    #[test]
    fn every_numeric_flag_refuses_garbage_instead_of_defaulting() {
        for flag in [
            "--min-liquidity",
            "--resolution-ts-ms",
            "--duration-secs",
            "--qty",
            "--page-limit",
            "--reward-weight",
            "--underlying-weight",
            "--window-secs",
            "--atm-blackout-scale",
            "--tox-widen",
            "--tox-size-cut",
            "--tox-window-secs",
        ] {
            assert!(parse(&[flag, "lots"]).is_err(), "{flag} must refuse a non-number");
            assert!(parse(&[flag]).is_err(), "a trailing {flag} must be an error");
        }
        // The three INTEGER ones additionally refuse a decimal and a negative, which the float ones
        // accept — worth pinning because `--duration-secs 1.5` looks reasonable and is not.
        for flag in ["--resolution-ts-ms", "--duration-secs", "--page-limit"] {
            assert!(parse(&[flag, "1.5"]).is_err(), "{flag} is an integer");
        }
        for flag in ["--duration-secs", "--page-limit"] {
            assert!(parse(&[flag, "-1"]).is_err(), "{flag} is unsigned");
        }
    }

    /// **FIXED.** The numeric arms used to report the raw `ParseFloatError`/`ParseIntError` and
    /// NEVER name the flag, so an operator who mistyped a numeric flag was told only `invalid float
    /// literal` and had to guess which of this bin's many numeric knobs it was about. Every numeric
    /// arm now routes its raw token through `parse_numeric`, which folds the flag name AND the
    /// offending value into the error — matching the two non-numeric failure shapes below, which
    /// already named theirs. It mattered here more than anywhere else in this sweep because these
    /// are the RISK knobs of a maker and the candidate set is large.
    #[test]
    fn a_numeric_parse_error_names_the_flag_and_the_offending_value() {
        let e = parse(&["--tox-size-cut", "point nine"]).expect_err("not a float");
        assert!(e.contains("--tox-size-cut"), "the error names the flag: {e}");
        assert!(e.contains("point nine"), "…and the offending value: {e}");
        // …matching the two NON-numeric failure shapes, which already named theirs.
        let trailing = parse(&["--tox-size-cut"]).expect_err("no value");
        assert!(trailing.contains("--tox-size-cut"), "{trailing}");
        let unknown = parse(&["--tox-sizecut", "0.9"]).expect_err("a typo");
        assert!(unknown.contains("--tox-sizecut"), "{unknown}");
    }

    /// **THE MONEY DEFECT, now refused.** A valued flag used to consume WHATEVER token followed,
    /// including a TOGGLE, and the toggle was then never seen. `--wallet-classes --toxicity` left
    /// `toxicity` FALSE: `main` gates the whole 5c flow-toxicity guard on that bool, so the maker
    /// quoted with NO toxic-flow widening and NO size cut, the wallet-class file was never even
    /// opened (it is read only inside `if args.toxicity`), and nothing anywhere said the
    /// `--toxicity` the operator typed had been consumed as a filename. The mirror case,
    /// `--query --auto`, selected a market literally named `--auto` instead of auto-picking the
    /// most-liquid one.
    ///
    /// Both are now usage errors naming BOTH tokens — the flag that went unfed and the flag that
    /// would have been eaten — so the operator learns which of the two they mistyped. The old
    /// accepting behaviour is asserted GONE (`expect_err`), which is the half that keeps a future
    /// edit from reintroducing it. Note the `expect_err` on `parse_args_from` itself rather than an
    /// unwrap through [`Parsed`]: the refusal happens BEFORE any `Parsed` is produced, so there is
    /// nothing to unwrap.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_toggle() {
        let e =
            parse_args_from(args(&["--auto", "--wallet-classes", "--toxicity"]), SENTINEL_REWARD)
                .expect_err("the flow-toxicity guard may not be eaten as a filename");
        assert!(
            e.contains("--wallet-classes") && e.contains("--toxicity"),
            "both tokens are named: {e}"
        );

        let q = parse_args_from(args(&["--query", "--auto"]), SENTINEL_REWARD)
            .expect_err("…and the market selector may not be eaten as a market NAME");
        assert!(q.contains("--query") && q.contains("--auto"), "{q}");

        // **The rule is `--`, not `-`.** A single-dash token still reaches the numeric arm, which is
        // what judges it — demonstrated on `--min-liquidity`, whose negative value this bin accepts
        // verbatim. `--qty -5` reaches its arm the same way and is refused by `--qty`'s OWN bound
        // rather than by the swallow rule, which is
        // `qty_now_refuses_negative_while_the_already_clamped_knobs_stay_unchecked`'s property.
        assert_eq!(
            parse(&["--min-liquidity", "-1"])
                .expect("a negative number is a value, not a flag")
                .min_liquidity,
            Some(-1.0)
        );
    }

    /// …and the two flags STILL work when they are given real values — the property the refusal
    /// above must not have bought. `--wallet-classes <path> --toxicity` is the invocation the guard
    /// is actually armed with, in both orders.
    #[test]
    fn the_toxicity_guard_still_arms_with_a_real_wallet_class_path() {
        for line in [
            &["--auto", "--wallet-classes", "/w/classes.json", "--toxicity"][..],
            &["--auto", "--toxicity", "--wallet-classes", "/w/classes.json"][..],
        ] {
            let a = run_args_raw(line);
            assert!(a.toxicity, "{line:?}");
            assert_eq!(a.wallet_classes.as_deref(), Some("/w/classes.json"), "{line:?}");
        }
    }

    /// **The interaction between this rule and the `--help` short-circuit**, which the two fixes
    /// that landed together both have an opinion about. `--help` takes no value, so the swallow
    /// rule can never fire on it — but a `--help` sitting in a VALUE slot reaches the valued flag's
    /// arm first, and is refused as a swallow rather than printing usage and exiting 0. That is the
    /// deliberate order, and on THIS bin it is the same argument as the money defect itself: a
    /// success there would silently discard the `--wallet-classes` the operator typed. `--help` in
    /// FLAG position — first, or after a fed flag — still short-circuits, which is the property
    /// [`help_short_circuits_to_a_success_even_without_a_market_selector`] owns.
    #[test]
    fn a_help_in_value_position_is_a_swallow_while_one_in_flag_position_still_succeeds() {
        let e = parse_args_from(args(&["--auto", "--wallet-classes", "--help"]), SENTINEL_REWARD)
            .expect_err("a value slot, not a plea");
        assert!(e.contains("--wallet-classes") && e.contains("--help"), "both are named: {e}");
        assert!(matches!(
            parse_args_from(args(&["--help", "--wallet-classes"]), SENTINEL_REWARD),
            Ok(Parsed::Help)
        ));
        assert!(matches!(
            parse_args_from(args(&["--wallet-classes", "/w/c.json", "--help"]), SENTINEL_REWARD),
            Ok(Parsed::Help)
        ));
    }

    /// A typo'd flag is REJECTED, not ignored — including one that differs from a real flag by a
    /// single character, which is the shape a mistyped risk knob actually takes.
    #[test]
    fn an_unknown_flag_is_rejected_and_named() {
        for typo in ["--toxicty", "--tox-widen-", "--rewardweight", "-toxicity"] {
            let e = parse(&[typo]).expect_err("a typo must not run");
            assert!(e.contains(typo), "the error names the offending token: {e}");
        }
    }

    /// The `POLY_REWARD_WEIGHT` parse, and the direction it fails in: anything unparseable — a
    /// decimal comma, a stray unit, an empty export — degrades SILENTLY to `0.0`, which is also what
    /// an absent variable means. So a typo'd export turns reward-aware quoting OFF rather than on;
    /// that is the safe direction, and it is pinned so a future edit cannot flip it.
    #[test]
    fn an_unparseable_reward_weight_degrades_to_rewards_off() {
        assert_eq!(parse_reward_weight(Some("0.75")), 0.75);
        assert_eq!(
            parse_reward_weight(Some("  0.75  ")),
            0.75,
            "surrounding whitespace is trimmed"
        );
        assert_eq!(parse_reward_weight(None), 0.0, "absent ⇒ rewards OFF");
        for bad in ["0,75", "75%", "", "   ", "off"] {
            assert_eq!(parse_reward_weight(Some(bad)), 0.0, "{bad:?} must degrade to rewards OFF");
        }
    }

    /// **`--qty` FIXED; the other three deliberately left as found, each for a reason read off its
    /// consumer rather than assumed.** `--qty` was an `f64` parse with no range check, so `--qty -5`
    /// reached `cfg.qty`, which `vike_mm::quote::requote` multiplies straight into `bid_qty`/
    /// `ask_qty` (`self.cfg.qty * bid_mult`) with NOTHING downstream to floor a negative size — so
    /// it now REFUSES a negative value: a maker's order size can't be signed. It was caught here
    /// only because this is a PAPER mount; the same shape on a live maker would submit a
    /// negative-size order.
    ///
    /// The other three float knobs this test used to list beside it stay UNCHECKED, because each
    /// one's consumer already neutralizes the out-of-range value before it can do anything:
    /// - `--tox-size-cut`: `vike_mm::quote::requote` computes `(1 − tox·size_cut).max(0.0)` — the
    ///   clamp is DOCUMENTED on `ToxicityParams::size_cut` itself ("`1.0` fully withdraws the side
    ///   at `tox == 1`"), so a value above `1.0` just withdraws the side at a lower toxicity
    ///   reading; the raw product can go negative but the SIZE that reaches the order never does.
    /// - `--underlying-weight`: `AsState::blended_anchor` clamps its OWN copy of the weight to
    ///   `[0, 1]` (`let w = w.clamp(0.0, 1.0);`) before folding it into the fair-value blend, so a
    ///   value outside `[0, 1]` is silently clamped at the point of use.
    /// - `--min-liquidity`: only ever compared as a filter floor
    ///   (`m.liquidity >= policy.min_liquidity` in `vike_polymarket::universe::select_universe`);
    ///   a negative floor is a no-op (accepts every market), never a signed value reaching an order.
    ///
    /// Bounding any of those three at the parser would refuse a configuration the consumer already
    /// handles safely — exactly the invented-bound harm this sweep was told to avoid.
    #[test]
    fn qty_now_refuses_negative_while_the_already_clamped_knobs_stay_unchecked() {
        let e = parse(&["--qty", "-5"]).expect_err("a negative maker order size must be refused");
        assert!(e.contains("--qty"), "{e}");
        assert_eq!(parse(&["--qty", "0"]).expect("zero is not negative").qty, Some(0.0));

        assert_eq!(parse(&["--tox-size-cut", "2"]).expect("accepted").tox_size_cut, 2.0);
        assert_eq!(
            parse(&["--underlying-weight", "9"]).expect("accepted").underlying_weight,
            9.0,
            "a blend weight outside [0,1] is accepted verbatim — clamped at the point of use"
        );
        assert_eq!(parse(&["--min-liquidity", "-1"]).expect("accepted").min_liquidity, Some(-1.0));
    }

    /// The LAST spelling of a repeated flag wins, silently — pinned because an operator appending an
    /// override to a shell-history line depends on exactly that direction.
    #[test]
    fn a_repeated_flag_takes_the_last_value() {
        let a = parse(&["--qty", "10", "--qty", "20"]).expect("a repeat is not an error");
        assert_eq!(a.qty, Some(20.0));
    }

    /// **FIXED.** `-h`/`--help` used to fall into the `other` arm (`unknown flag "--help"`) and exit
    /// **1** — the same defect class `vike_tradehub`'s `Parsed::Help` was introduced to fix (a
    /// non-zero `--help` breaks `set -e` and any wrapper that checks a status). Both spellings now
    /// short-circuit to `Parsed::Help`, even with NO market selector supplied — help must not be
    /// gated behind the `--auto`/`--query`/`--token-id` requirement.
    #[test]
    fn help_short_circuits_to_a_success_even_without_a_market_selector() {
        for flag in ["-h", "--help"] {
            assert!(
                matches!(parse_args_from(args(&[flag]), SENTINEL_REWARD), Ok(Parsed::Help)),
                "{flag}"
            );
        }
        assert!(matches!(
            parse_args_from(args(&["--auto", "--help"]), SENTINEL_REWARD),
            Ok(Parsed::Help)
        ));
    }
}
