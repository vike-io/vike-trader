//! A MEASUREMENT, not a gate (`#[ignore]`d, print-only): the bytes a desktop chart's history costs per
//! action, the way it reads before and after the view-driven read (`resample_to`, `bar_resample`), over
//! three years of synthetic `1m` bars at Binance precision. Every action dials its own handle through a
//! relay that counts what crosses in each direction, so the handshake is counted too, as the desktop pays
//! it (one handle per batch of reads). Run on a lane:
//! `cargo test -p vike-datahub --release --test default_build -- --ignored --nocapture chart_resolution_bytes`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_marketdata::Bar;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// 2024-01-01T00:00:00Z, the end of the three years and the desktop's "now".
const NOW: i64 = 1_704_067_200_000;
const DAYS: i64 = 1_095;

/// Three years of `1m` BTCUSDT, a deterministic random walk at Binance's precision (0.01 price, 0.00001
/// volume), so a reply's size is what real candles of that market take.
fn three_years() -> Vec<Bar> {
    let (mut seed, mut close) = (0x2545_f491_4f6c_dd1d_u64, 42_000.0_f64);
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % 10_000) as f64 / 10_000.0
    };
    let cents = |x: f64| (x * 100.0).round() / 100.0;
    (0..DAYS * 1_440)
        .map(|i| {
            let open = close;
            close = cents(open + (next() - 0.5) * 40.0);
            let high = cents(open.max(close) + next() * 15.0);
            let low = cents(open.min(close) - next() * 15.0);
            let volume = (next() * 40.0 * 100_000.0).round() / 100_000.0;
            Bar::ohlcv(NOW - DAYS * DAY + i * MINUTE, open, high, low, close, volume)
        })
        .collect()
}

/// A relay in front of the server that counts the bytes each way, per action.
struct Relay {
    address: SocketAddr,
    up: Arc<AtomicU64>,
    down: Arc<AtomicU64>,
}

impl Relay {
    fn spawn(server: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the relay");
        let address = listener.local_addr().expect("the relay's address");
        let (up, down) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (u, d) = (Arc::clone(&up), Arc::clone(&down));
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let to_server = TcpStream::connect(server).expect("the server");
                let pipe = |mut from: TcpStream, mut to: TcpStream, count: Arc<AtomicU64>| {
                    thread::spawn(move || {
                        let mut buffer = vec![0u8; 64 * 1024];
                        while let Ok(n) = from.read(&mut buffer) {
                            if n == 0 || to.write_all(&buffer[..n]).is_err() {
                                break;
                            }
                            count.fetch_add(n as u64, Ordering::SeqCst);
                        }
                        let _ = to.shutdown(std::net::Shutdown::Both);
                    })
                };
                pipe(client.try_clone().unwrap(), to_server.try_clone().unwrap(), Arc::clone(&u));
                pipe(to_server, client, Arc::clone(&d));
            }
        });
        Self { address, up, down }
    }

    /// The bytes `action` moved, each way, and what it returned.
    fn count<T>(&self, action: impl FnOnce(SocketAddr) -> T) -> (u64, u64, T) {
        self.up.store(0, Ordering::SeqCst);
        self.down.store(0, Ordering::SeqCst);
        let out = action(self.address);
        thread::sleep(std::time::Duration::from_millis(50)); // the relay counts after it forwards
        (self.up.load(Ordering::SeqCst), self.down.load(Ordering::SeqCst), out)
    }
}

fn line(action: &str, up: u64, down: u64, bars: usize) {
    let secs = |rate: f64| down as f64 / (rate * 1_024.0);
    println!(
        "| {action:<52} | {bars:>9} | {up:>7} | {down:>11} | {:>7.2} | {:>7.2} |",
        secs(40.0),
        secs(13.0)
    );
}

#[test]
#[ignore = "a measurement: run by hand on a lane, --release --nocapture"]
fn chart_resolution_bytes() {
    let store = HistoryStoreMemory::default();
    store.append_bars("binance", "BTCUSDT", "1m", &three_years(), None).expect("seed");
    let relay = Relay::spawn(spawn_server(Arc::new(store)));
    let desktop =
        |address: SocketAddr| HistoryStoreDatahub::new(address.to_string()).with_compressed_bars();
    let native = |range: TimestampRange| {
        move |address: SocketAddr| {
            desktop(address).load_bars("binance", "BTCUSDT", "1m", range).unwrap().len()
        }
    };
    let resampled = |range: TimestampRange, to: &'static str| {
        move |address: SocketAddr| {
            desktop(address)
                .load_bars_resampled("binance", "BTCUSDT", "1m", range, to)
                .unwrap()
                .unwrap()
                .len()
        }
    };
    println!("| action | bars to PC | up B | down B | s @40 KB/s | s @13 KB/s |");
    println!("|---|---|---|---|---|---|");

    // BEFORE, on main: the open's 3,000 `1m` bars as JSON 10,000-row pages (the old pager), once per session.
    let today = TimestampRange::of(NOW - 3_000 * MINUTE, NOW);
    let (up, down, n) = relay.count(|address| {
        let mut client = DatahubClient::connect(address).unwrap();
        let (mut start, mut n) = (today.start, 0);
        loop {
            let page = client
                .load_bars(
                    "binance",
                    "BTCUSDT",
                    "1m",
                    TimestampRange { start, end: today.end },
                    Some(10_000),
                )
                .unwrap();
            let Some(last) = page.last().map(|b| b.timestamp) else { break n };
            n += page.len();
            start = Some(last + 1);
        }
    });
    line("before (main, JSON): open 1m chart", up, down, n);
    // BEFORE, on perf/backtest-data-path: the same read as the desktop's LZ4 columns.
    let (up, down, n) = relay.count(native(today));
    line("before (LZ4 columns): open 1m chart", up, down, n);
    line("before: zoom out / zoom in / scroll a week", 0, 0, 0);
    let all = TimestampRange::of(NOW - DAYS * DAY, NOW);
    let (up, down, n) = relay.count(native(all));
    line("before, naive (LZ4 columns): every 1m bar of all", up, down, n);

    // AFTER: what the planner asks for a 2,000-point chart (`vike-app-core`'s `history_tests`).
    let open = TimestampRange::of(NOW - 1_999 * MINUTE, NOW + MINUTE - 1);
    let (up, down, n) = relay.count(native(open));
    line("after: open 1m chart (2,000 x 1m)", up, down, n);
    let zoom_out = TimestampRange::of(NOW - DAYS * DAY, NOW + DAY - 1);
    let (up, down, n) = relay.count(resampled(zoom_out, "1d"));
    line("after: zoom out to all (1d, resampled)", up, down, n);
    let day = NOW - 30 * DAY;
    let (up, down, n) = relay.count(native(TimestampRange::of(day, day + DAY - 1)));
    line("after: zoom in to one day (1,440 x 1m)", up, down, n);
    let week_back = day - 7 * DAY;
    let (up, down, n) = relay.count(native(TimestampRange::of(week_back, week_back + DAY - 1)));
    line("after: scroll back a week (the missing day)", up, down, n);
    line("after: back to a seen view (cache)", 0, 0, 0);
}
