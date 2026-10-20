//! **A MEASUREMENT harness. It stores nothing, decides nothing, and validates nothing.**
//!
//! It answers one question by actually fetching: *for each (venue, instrument type, interval), does
//! this venue serve klines through the dispatch this tree ships, and how many rows come back?* The
//! output is a markdown matrix on stdout for a human to read. There is no capability table here,
//! no menu, nothing asserted about a venue's answer — an operator reads the table and decides.
//!
//! This was a COPY of the datahub's `venue_interval_matrix` live probe, which the datahub's
//! backfill rewiring (decision 0164) deleted: the datahub's table no longer reaches a venue, so
//! the measurement is made where the venue call is made. It drives the dispatch that makes them:
//! the REAL `VenueRest` lane behind the real server, asked through the client crate's
//! `RemoteKlines` exactly as the datahub's table asks. What it cannot do is write to a store —
//! this daemon links none (decision 0084) — so a cell reports the rows the source RETURNED, and
//! the `S` origin of the datahub's old matrix (the store refused the rows) does not exist here.
//!
//! # Why it lives here, behind `data`
//!
//! The measurement is only worth anything if it goes through the SAME dispatch the daemon uses, so
//! it drives `VenueRest::new` — the lane the daemon mounts — rather than calling a venue's source by
//! hand. That lane is behind `data`, so this file is too; a default build compiles it to nothing.
//!
//! ⚠ **Every network probe is `#[ignore]`d, and that is load-bearing rather than tidy.** The
//! `datahub-all` job RUNS this crate's tests, so without the attribute a merge gate would fire real
//! venue REST calls from the box that signs orders, on every PR, on a shared public IP.
//! [`the_probe_roster_only_names_venues_the_production_lane_dispatches`] is the one test here that
//! runs unattended, and it touches no network.
//!
//! # The two window shapes, and why one is not enough
//!
//! * **sized** — `[now - 10*width, now]`. Measures CAPABILITY: ten bars is a request the venue can
//!   answer for any interval it serves at all.
//! * **fixed10m** — `[now - 10min, now]`. Measures what a SMALL request returns. For an interval
//!   coarser than ten minutes this is a window containing no bar OPEN, so `0` here beside a
//!   non-zero `sized` is "the venue serves it, the window was too narrow", not "unsupported".
//!
//! `now` is captured ONCE for the whole run, so the two shapes differ only in their start.
//!
//! # The two error origins, kept apart
//!
//! An empty cell and a failed cell are different facts, and so are the failures:
//!
//! * `T` — the BRIDGE's own interval table refused it (`vike_bybit::data::interval_code` /
//!   `vike_okx::data::bar_code`). **The request never left the box.** Binance has no such table —
//!   `vike_binance::family::klines::klines_url` interpolates the caller's interval straight into the
//!   query string — so a binance `T` is impossible by construction and a binance refusal is always
//!   the venue's.
//! * `V` — the venue answered and the answer was an error (HTTP status, bybit `retCode`, okx `code`).
//!   The request WAS spent.
//!
//! The classification reads the error TEXT, which is what the wire carries too.
//!
//! # Running it
//!
//! ```text
//! cargo test -p vike-gatewayhub --features data --test venue_interval_matrix \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` is not decoration: the probes share one public IP and each venue's pager paces
//! itself on the assumption that it is the only thing talking to that host from this process.

#![cfg(feature = "data")]

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/rest_server.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod rest_server;

use std::collections::BTreeMap;
use std::time::Instant;

use vike_data::source::KlineSource;
use vike_gatewayhub::rest::RestLane;
use vike_gatewayhub::rest::lane::VenueRest;
use vike_gatewayhub_client::history::RemoteKlines;

use rest_server::RestServer;

/// The interval vocabulary this harness asks about — deliberately WIDER than anything in the tree.
///
/// The point is to find each venue's real edge, so the list is the union of what the three venues'
/// own tables name plus `1s` and `3d`, which only some of them do. Two of these (`1w`, `1M`) have no
/// width in `vike_marketdata::interval::interval_ms`'s grammar at all — see [`probe_width_ms`].
static INTERVALS: [&str; 15] =
    ["1s", "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "12h", "1d", "3d", "1w", "1M"];

/// The interval a MISROUTE probe (see [`Probe::intervals`]) is measured at — one cell is enough to
/// establish where an unaddressable instrument type actually lands, and spending fifteen on it would
/// be fifteen requests bought for one fact.
static MISROUTE_INTERVAL: [&str; 1] = [REFERENCE_INTERVAL];

/// The one interval at which every row that returned bars also prints its FIRST BAR, so the tapes
/// can be compared side by side. It is [`MISROUTE_INTERVAL`] by construction: a misroute row is the
/// one whose answer most needs identifying, and an identification is worth nothing without the
/// addressable rows' own bars printed in the same units on the same run.
const REFERENCE_INTERVAL: &str = "1h";

/// How wide one bar of `interval` is, in ms — **the harness's OWN table, and it has to be.**
///
/// `vike_marketdata::interval::interval_ms` splits on a single trailing char and knows `s`/`m`/`h`/`d`, so it
/// answers `None` for `1w` and `1M`. A sized window needs a width for every interval probed, so this
/// table is where the two extra rows live. It is a PROBE parameter, not a vocabulary: nothing here
/// widens what a source accepts, and that gap is itself one of the findings this harness exists to
/// surface.
///
/// `1M` is thirty days. A calendar month is not a fixed multiple and this harness does not pretend
/// otherwise — the number only has to buy a window that spans about ten monthly bars.
fn probe_width_ms(interval: &str) -> i64 {
    match interval {
        "1s" => 1_000,
        "1m" => 60_000,
        "3m" => 180_000,
        "5m" => 300_000,
        "15m" => 900_000,
        "30m" => 1_800_000,
        "1h" => 3_600_000,
        "2h" => 7_200_000,
        "4h" => 14_400_000,
        "6h" => 21_600_000,
        "12h" => 43_200_000,
        "1d" => 86_400_000,
        "3d" => 259_200_000,
        "1w" => 604_800_000,
        "1M" => 2_592_000_000,
        other => panic!("probe_width_ms has no row for {other:?} — add one beside the interval"),
    }
}

/// One (venue, instrument type, symbol) row of the matrix.
struct Probe {
    /// The venue id the production lane dispatches on.
    venue: &'static str,
    /// What THIS TREE calls the instrument type, read from the bridge rather than from a taxonomy.
    kind: &'static str,
    /// The symbol as a CALLER hands it to the source — the store/series key, suffix included.
    symbol: &'static str,
    /// Which intervals to probe. [`INTERVALS`] for an addressable type; [`MISROUTE_INTERVAL`] for a
    /// type the source cannot address, where the finding is WHERE the request lands, not which
    /// intervals it serves.
    intervals: &'static [&'static str],
    /// Why this row is here, printed under the table. For a misroute probe this is the finding.
    note: &'static str,
}

/// The rows, each read out of its bridge's own routing code.
///
/// ⚠ Two instrument types in this list are **not addressable** and are probed anyway, at one
/// interval, so that the claim is an OBSERVATION rather than a code reading: the symbol is handed in
/// and the harness reports where the request actually went.
fn probes() -> Vec<Probe> {
    vec![
        Probe {
            venue: "binance",
            kind: "spot",
            symbol: "BTCUSDT",
            intervals: &INTERVALS,
            note: "api.binance.com/api/v3/klines — `vike_binance::data`'s spot klines base, reached \
                   because `range_target` finds no `vike_catalog::PERP_SUFFIX`.",
        },
        Probe {
            venue: "binance",
            kind: "usdm-perp",
            symbol: "BTCUSDT.P",
            intervals: &INTERVALS,
            note: "fapi.binance.com/fapi/v1/klines — `vike_binance::data`'s fapi klines base. The \
                   `.P` suffix is stripped for the wire and kept as the series key.",
        },
        Probe {
            venue: "binance",
            kind: "coinm-futures",
            symbol: "BTCUSD_PERP.P",
            intervals: &INTERVALS,
            note: "dapi.binance.com/dapi/v1/klines — `vike_binance::data`'s THIRD host. The `.P` \
                   suffix says PERPETUAL (which binance's own listing agrees it is) and \
                   `vike_binance::instruments` — the venue's COIN-M listing, read once per \
                   process — is what says the book is coin-margined, so the route is neither a \
                   symbol-shape guess nor a class claim. ⚠ The host is not what made this \
                   reachable: fapi serves this tape byte-identically. \
                   `vike_binance::family::klines::VolumeColumn` is — index 5 here is a CONTRACT \
                   COUNT, not the base asset.",
        },
        Probe {
            venue: "binance",
            kind: "coinm-futures (BARE — the venue refuses it)",
            symbol: "BTCUSD_PERP",
            intervals: &MISROUTE_INTERVAL,
            note: "THE ORIGINAL FINDING, kept because the REFUSAL is the measurement now. A bare \
                   symbol carries no perpetual marker, so `vike_binance::data::route_target` sends \
                   it to SPOT and spot answers `-1121 Invalid symbol`. That is an ERROR rather \
                   than another book's tape, which is exactly what \
                   `vike_catalog::addressing_for`'s binance `BareSymbol::Unambiguous` claims — and \
                   binance's three `symbol` sets being disjoint is why it holds. The row above is \
                   the spelling that works.",
        },
        Probe {
            venue: "bybit",
            kind: "spot",
            symbol: "BTCUSDT",
            intervals: &INTERVALS,
            note: "`category=spot` on /v5/market/kline — `vike_bybit::data::Category::Spot`, where \
                   an unclaimed unambiguous bare symbol routes.",
        },
        Probe {
            venue: "bybit",
            kind: "linear-perp",
            symbol: "BTCUSDT.P",
            intervals: &INTERVALS,
            note: "`category=linear` — `vike_bybit::data::route_target` asks the venue's own \
                   listings which derivative book carries the stripped symbol \
                   (`vike_bybit::instruments::perp_book`). Bybit V5 unifies the categories under \
                   one host, so this is a query parameter rather than a second host.",
        },
        Probe {
            venue: "bybit",
            kind: "inverse-perp",
            symbol: "BTCUSD.P",
            intervals: &INTERVALS,
            note: "ADDRESSABLE since the 0061 phase-4 routing: `vike_bybit::data::Category` has an \
                   Inverse arm and the book is a venue-LISTING membership rather than a reading of \
                   the symbol's shape. ⚠ THE ORIGINAL FINDING HERE WAS HALF WRONG and is corrected \
                   rather than deleted: it said an inverse symbol was asked of the SPOT book \
                   'which does not list it'. Bybit DOES list BTCUSD on spot (MEASURED 2026-09-16: \
                   spot n inverse = {BTCUSD, ETHUSD}) — the misroute served a VESTIGIAL spot tape, \
                   which is why it was silent rather than an error, and is exactly what \
                   `vike_bybit::data::ambiguous_bare_symbol_refusal` now refuses for a BARE symbol. \
                   The unsuffixed spelling is therefore deliberately NOT probed here: it is a \
                   refusal, not a route.",
        },
        Probe {
            venue: "okx",
            kind: "spot",
            symbol: "BTC-USDT",
            intervals: &INTERVALS,
            note: "`instId=BTC-USDT` on /api/v5/market/history-candles.",
        },
        Probe {
            venue: "okx",
            kind: "linear-swap",
            symbol: "BTC-USDT-SWAP",
            intervals: &INTERVALS,
            note: "`vike_okx::data`'s candles URL interpolates the caller's symbol into `instId` \
                   VERBATIM — no suffix logic, no category. OKX instrument types are therefore \
                   addressable by symbol alone, with no bridge change.",
        },
        Probe {
            venue: "okx",
            kind: "inverse-swap",
            symbol: "BTC-USD-SWAP",
            intervals: &INTERVALS,
            note: "The same verbatim `instId` path — this is the type binance and bybit cannot reach \
                   at all, and OKX reaches it for free.",
        },
    ]
}

/// One window shape: a label and the start it derives from `now`.
struct Shape {
    label: &'static str,
    /// `(now_ms, interval) -> start_ms`.
    start: fn(i64, &str) -> i64,
}

/// Ten bars wide — the CAPABILITY question.
fn start_sized(now: i64, interval: &str) -> i64 {
    now - 10 * probe_width_ms(interval)
}

/// Ten minutes wide whatever the interval — the SMALL-REQUEST question.
fn start_fixed_ten_minutes(now: i64, _interval: &str) -> i64 {
    now - 600_000
}

static SHAPES: [Shape; 2] = [
    Shape { label: "sized (10 x interval)", start: start_sized },
    Shape { label: "fixed 10 minutes", start: start_fixed_ten_minutes },
];

/// Where a refusal came from — the two different facts a failed cell can be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The bridge's own interval table. The request never left the box.
    BridgeTable,
    /// The venue answered, and the answer was a refusal. The request was spent.
    Venue,
}

impl Origin {
    fn tag(self) -> &'static str {
        match self {
            Origin::BridgeTable => "T",
            Origin::Venue => "V",
        }
    }
}

/// Classify a source error's text.
///
/// The bridges' pre-flight tables are the only producers of `unsupported interval` —
/// `vike_bybit::data::interval_code` and `vike_okx::data::bar_code` both format exactly that, and
/// both run BEFORE any agent is built, which is what makes the distinction a fact about whether a
/// request was spent rather than a guess about severity.
fn classify(err: &str) -> Origin {
    if err.contains("unsupported interval") { Origin::BridgeTable } else { Origin::Venue }
}

/// What one cell measured.
struct Outcome {
    rows: Option<usize>,
    first: Option<(i64, f64, f64)>,
    err: Option<(Origin, String)>,
    elapsed_ms: u128,
}

/// Run one cell through the production dispatch: the real lane, behind the real server, asked
/// through the client crate's own source.
fn probe_cell(server: &RestServer, p: &Probe, interval: &str, start: i64, end: i64) -> Outcome {
    let started = Instant::now();
    let source = RemoteKlines::new(server.link(), p.venue);
    let result = source.fetch(p.symbol, interval, start, end);
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(bars) => Outcome {
            rows: Some(bars.len()),
            first: bars.first().map(|b| (b.ts, b.close, b.volume)),
            err: None,
            elapsed_ms,
        },
        Err(e) => {
            let text = match e {
                vike_data::source::SourceError::Fetch(t)
                | vike_data::source::SourceError::Refused(t) => t,
            };
            Outcome { rows: None, first: None, err: Some((classify(&text), text)), elapsed_ms }
        }
    }
}

/// The one test in this file that runs unattended: the probe roster may only name venues the
/// PRODUCTION lane actually dispatches, so a probe row can never quietly measure nothing.
///
/// Network-free — `VenueRest::new` builds registries and opens no socket, and `facts().klines` is the
/// same list the server's unknown-venue refusal prints.
#[test]
fn the_probe_roster_only_names_venues_the_production_lane_dispatches() {
    let served = VenueRest::new(false).facts().klines;
    for p in probes() {
        assert!(
            served.iter().any(|v| v == p.venue),
            "probe row {}/{} names venue {:?}, which the production lane does not dispatch \
             (it dispatches {:?}) — a row like that would report a refusal that says nothing \
             about the venue",
            p.venue,
            p.kind,
            p.venue,
            served,
        );
        assert!(
            !p.intervals.is_empty(),
            "probe row {}/{} would measure no cell at all",
            p.venue,
            p.kind,
        );
    }
}

/// THE SWEEP. Real venue REST calls — see this file's module doc for the command.
///
/// Prints, per window shape, a markdown table of `venue x instrument type x interval` whose cells
/// are the rows returned or a one-letter error origin keyed to a numbered legend of verbatim errors.
/// It asserts NOTHING about any venue's answer: an assertion here would be this harness deciding
/// something, which is exactly what it must not do.
///
/// The only panic path is the harness's own — an interval with no width row.
#[test]
#[ignore = "real venue REST calls from a shared public IP — run explicitly, see the module doc"]
fn venue_interval_matrix() {
    let now = vike_model::now_ms();
    let all = probes();
    let server = RestServer::start(VenueRest::new(false));
    let mut calls = 0usize;
    let mut by_venue_calls: BTreeMap<&str, usize> = BTreeMap::new();

    println!("\n# venue x instrument type x interval — measured, not decided");
    println!("\n`now` = {now} (epoch ms, captured once for the whole run)");

    for shape in &SHAPES {
        // Verbatim errors, deduplicated, in first-seen order; cells cite them by index.
        let mut legend: Vec<(Origin, String)> = Vec::new();
        let mut rendered: Vec<(String, Vec<String>)> = Vec::new();

        for p in &all {
            let mut cells = Vec::with_capacity(INTERVALS.len());
            for interval in INTERVALS.iter() {
                if !p.intervals.contains(interval) {
                    cells.push("·".to_string());
                    continue;
                }
                let start = (shape.start)(now, interval);
                let out = probe_cell(&server, p, interval, start, now);
                calls += 1;
                *by_venue_calls.entry(p.venue).or_default() += 1;
                let cell = match (&out.rows, &out.err) {
                    (Some(n), _) => n.to_string(),
                    (None, Some((origin, msg))) => {
                        let idx = match legend.iter().position(|(_, m)| m == msg) {
                            Some(i) => i,
                            None => {
                                legend.push((*origin, msg.clone()));
                                legend.len() - 1
                            }
                        };
                        format!("{}{}", origin.tag(), idx + 1)
                    }
                    (None, None) => unreachable!("a cell is either rows or an error"),
                };
                println!(
                    "cell shape={} venue={} kind={} symbol={} interval={interval} start={start} \
                     end={now} -> {cell} ({} ms)",
                    shape.label, p.venue, p.kind, p.symbol, out.elapsed_ms,
                );
                // WHICH BOOK ANSWERED, at one reference interval, for the rows that returned any.
                //
                // A row count alone cannot tell a REFUSAL apart from a wrong-book ANSWER, and the
                // difference matters most exactly where the source cannot address the instrument
                // type it was asked for: a venue that answers such a request returns somebody's
                // bars under the caller's symbol and reports success. Volume is the discriminator
                // that needs no second request — a BTC-denominated spot tape and a USD-denominated
                // inverse-contract tape differ by orders of magnitude, not by a rounding.
                if interval == &REFERENCE_INTERVAL
                    && let Some((ts, close, volume)) = out.first
                {
                    println!(
                        "  first-bar venue={} kind={} symbol={} ts={ts} close={close} \
                         volume={volume}",
                        p.venue, p.kind, p.symbol,
                    );
                }
                cells.push(cell);
            }
            rendered.push((format!("{} / {} (`{}`)", p.venue, p.kind, p.symbol), cells));
        }

        println!("\n## window shape: {}\n", shape.label);
        println!("| venue / instrument type (symbol) | {} |", INTERVALS.join(" | "));
        println!("|---|{}", "---|".repeat(INTERVALS.len()));
        for (label, cells) in &rendered {
            println!("| {label} | {} |", cells.join(" | "));
        }
        println!(
            "\n`·` = not probed for this row. `T`/`V` = refused by the bridge's own interval \
             table (never left the box) / by the venue; the number keys the legend."
        );
        if legend.is_empty() {
            println!("\nno refusals in this shape.");
        } else {
            println!("\n### legend — verbatim errors\n");
            for (i, (origin, msg)) in legend.iter().enumerate() {
                println!("{}. `{}{}` — `{msg}`", i + 1, origin.tag(), i + 1);
            }
        }
    }

    println!("\n## probe notes (why each row is in the table)\n");
    for p in &all {
        println!("* **{} / {}** (`{}`) — {}", p.venue, p.kind, p.symbol, p.note);
    }

    println!("\n## calls issued\n");
    println!("| venue | fetch calls |");
    println!("|---|---|");
    for (venue, n) in &by_venue_calls {
        println!("| {venue} | {n} |");
    }
    println!("| **total** | **{calls}** |");
    println!(
        "\nA CALL is one `Fetch` through the gateway. Requests per call are the pager's business and \
         are not observable from this seam: binance additionally GETs its host's `exchangeInfo` \
         ONCE per call before paging (`vike_binance::family::klines`'s discovered-pacing doc), and \
         bybit/okx GET only pages. A call whose cell reads `T` issued NO request."
    );
}
