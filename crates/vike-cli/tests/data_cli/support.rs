//! Shared fixtures: the seeded loopback datahubs, their bar/fill rows and the port-premise probe.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_model::{AssetClass, Bar, SymbolProperties};

// ─── the read half, against a REAL loopback datahub ─────────────────────────────────────────────

/// Epoch-ms per UTC day — the seeded fixture's step, so its three bars land on three distinct days
/// and the `DAYS` column has something to count.
pub(super) const DAY_MS: i64 = 86_400_000;

/// A bar for the seeded store. The prices are irrelevant; the TIMESTAMP is not — the coverage this
/// verb renders is folded from it.
pub(super) fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Bind an ephemeral loopback listener, seed an in-memory store with TWO series of DIFFERENT kinds
/// — a `bar` series (which sub-partitions by interval) and a `properties` series (which has none) —
/// and serve it on a detached thread.
///
/// Two kinds is the point rather than convenience: a listing that assumed one shape would render
/// the other wrongly, and a single-series fixture could not tell a real `interval` column from a
/// hardcoded one.
///
/// ⚠ **`properties` rather than `trade`, and that is a measurement rather than a taste.**
/// `MemHistStore::append_trades` is a NO-OP returning `Ok(0)` — the double holds no tick maps at
/// all — so a trade-seeded fixture reports a successful seed, enumerates ONE series, and leaves
/// every assertion below silently weaker than it reads. `MemHistStore::catalog` is the authority
/// for which kinds this double can actually enumerate; `properties` is one of them and, like every
/// tick-shaped kind, carries no interval, which is the property this fixture needs.
pub(super) fn spawn_seeded_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = MemHistStore::new();
    store
        .append_bars("binance", "BTCUSDT", "1h", &[bar(0), bar(DAY_MS), bar(2 * DAY_MS)], None)
        .expect("seed bars");
    store
        .append_symbol_properties("okx", "BTC-USDT", &[(DAY_MS, SymbolProperties::default())], None)
        .expect("seed properties");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// A datahub over a store seeded to hold ALL THREE class outcomes at once — the fixture for
/// `--class`, and a SECOND one rather than a widening of [`spawn_seeded_datahub`] because two of
/// this file's cases pin that store's series COUNT and a third instrument would break them for a
/// reason having nothing to do with what they test.
///
/// * `binance/BTCUSDT` — bars, and NO properties row anywhere. The `no-properties` verdict: nothing
///   has ever recorded an instrument grid for it.
/// * `bybit/BTCUSDT` — one properties row whose `asset_class` is `None`. The `unclassified`
///   verdict, and the signal this whole flag exists for: a producer recorded a grid and named no
///   class. `SymbolProperties::default()` genuinely leaves it `None`, so this is the real shape a
///   not-yet-wired venue writes rather than a mock of one.
/// * `okx/BTC-USDT-SWAP` — TWO properties rows, the earlier naming no class and the later naming
///   [`AssetClass::CryptoPerp`]. Two rather than one, so the case also proves the as-of rule: the
///   probe asks `properties_as_of` at `i64::MAX` and must come back with the LATER answer. A
///   single-row fixture would pass identically whether the code took the first or the last.
pub(super) fn spawn_class_probe_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = MemHistStore::new();
    store.append_bars("binance", "BTCUSDT", "1h", &[bar(0), bar(DAY_MS)], None).expect("seed bars");
    store
        .append_symbol_properties(
            "bybit",
            "BTCUSDT",
            &[(DAY_MS, SymbolProperties::default())],
            None,
        )
        .expect("seed a class-less grid");
    store
        .append_symbol_properties(
            "okx",
            "BTC-USDT-SWAP",
            &[
                (DAY_MS, SymbolProperties::default()),
                (
                    2 * DAY_MS,
                    SymbolProperties {
                        asset_class: Some(AssetClass::CryptoPerp),
                        ..SymbolProperties::default()
                    },
                ),
            ],
            None,
        )
        .expect("seed a classified grid");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// How many times [`an_unreachable_datahub_is_the_connect_rung`] re-rolls onto a fresh port when
/// its premise — that nothing is listening on the address it chose — is MEASURED to be false.
/// Bounded, and exhausting it PANICS: a case that skipped itself when the port was stolen would be
/// worse than the flake it replaces.
pub(super) const PREMISE_ATTEMPTS: usize = 8;

/// How long the premise probe waits for a loopback connect to answer. A closed loopback port
/// refuses immediately and an open one accepts immediately, so this bounds only the pathological
/// case; it is not a tuning knob any assertion depends on.
const PREMISE_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Is anything listening on `addr` RIGHT NOW? The strongest premise check available here — the
/// same syscall, to the same address, that the child process is about to make.
pub(super) fn is_listening(addr: &SocketAddr) -> bool {
    TcpStream::connect_timeout(addr, PREMISE_PROBE_TIMEOUT).is_ok()
}

/// One fill for the SHARED-store fixture. The numbers are irrelevant; what matters is that the
/// row lands under `kind=exec_fill` at the SAME `(venue, symbol)` as the bars.
pub(super) fn fill(ts: i64) -> vike_data::ExecFillRow {
    vike_data::ExecFillRow {
        ts,
        trade_id: "t-1".to_string(),
        client_order_id: "c-1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        px: 100.0,
        commission: 0.0,
        mark_price: None,
        liquidity_side: String::new(),
        commission_asset: String::new(),
    }
}

/// A datahub over a SHARED store — market data and this account's own activity under ONE
/// `(venue, symbol)`, which is the shape a real box has: the account plane writes beside the
/// market plane in the same tree.
///
/// A THIRD fixture rather than a widening of [`spawn_seeded_datahub`], for
/// [`spawn_class_probe_datahub`]'s stated reason: two of this file's cases pin that store's series
/// COUNT, and an extra series would break them for a reason having nothing to do with what they
/// test.
pub(super) fn spawn_shared_account_store_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = MemHistStore::new();
    store
        .append_bars("binance", "BTCUSDT", "1h", &[bar(0), bar(DAY_MS), bar(2 * DAY_MS)], None)
        .expect("seed bars");
    store.append_exec_fills("binance", "BTCUSDT", &[fill(DAY_MS)], None).expect("seed a fill");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// Every line of a `--format jsonl` stdout, as documents — and the assertion that each one IS one.
pub(super) fn jsonl_rows(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("every jsonl line is one document ({e}): {l}"))
        })
        .collect()
}

/// Midnight UTC of `y-m-d`, in epoch-ms.
pub(super) fn utc_day(y: i64, m: u32, d: u32) -> i64 {
    vike_model::time::days_from_civil(y, m, d) * DAY_MS
}
