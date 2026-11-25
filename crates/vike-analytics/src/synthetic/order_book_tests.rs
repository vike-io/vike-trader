//! `order_book`'s tests: the published defaults, the stream's integrity against an `L2Book` fold,
//! each rate law of the model, CST's Table 3 probabilities, the empty-side rule, and every refusal.

use vike_marketdata::{L2Book, L2BookUpdateKind};

use super::{
    SyntheticInitialQueues, SyntheticOrderBook, SyntheticOrderBookModel, order_book_stream,
};

const T0: i64 = 1_735_689_600_000;
const MINUTE: i64 = 60_000;
/// The paper's sample: Sky Perfect Communications at 74,300 yen (its Table 1).
const START_PRICE: f64 = 74_300.0;

/// `(best bid, its size, best ask, its size)`.
type Top = (f64, f64, f64, f64);

fn cst() -> SyntheticOrderBookModel {
    SyntheticOrderBookModel::cont_stoikov_talreja()
}

/// A model with these rates, the paper's tick and order size, and these fixed starting queues.
fn model(
    limit_rates: Vec<f64>,
    cancel_rates: Vec<f64>,
    market_rate: f64,
    bids: Vec<u64>,
    asks: Vec<u64>,
) -> SyntheticOrderBookModel {
    SyntheticOrderBookModel {
        limit_rates,
        cancel_rates,
        market_rate,
        initial_queues: SyntheticInitialQueues::Fixed { bids, asks },
        ..cst()
    }
}

/// Fold every update of `book` into an `L2Book`, asserting the stream's integrity on the way (one
/// snapshot at `start`, then one-level deltas, contiguous sequences, ordered timestamps before `end`,
/// never a crossed or locked book); the tops while both sides hold orders, each change once.
fn fold(book: &SyntheticOrderBook, tick_size: f64, start: i64, end: i64) -> (L2Book, Vec<Top>) {
    let mut l2 = L2Book::new(tick_size);
    let (mut tops, mut last, mut last_time) = (Vec::new(), None, start);
    for (i, u) in book.updates.iter().enumerate() {
        assert_eq!(u.sequence, i as u64 + 1, "update {i}");
        assert!(u.timestamp >= last_time && u.timestamp < end, "update {i} at {}", u.timestamp);
        assert_eq!(u.tick_size, tick_size);
        last_time = u.timestamp;
        if i == 0 {
            assert_eq!((u.kind, u.timestamp), (L2BookUpdateKind::Snapshot, start));
            l2.apply_snapshot(u.sequence, &u.bids, &u.asks);
        } else {
            assert_eq!(u.kind, L2BookUpdateKind::Delta, "update {i}");
            assert_eq!(u.bids.len() + u.asks.len(), 1, "update {i} changes one level");
            assert!(l2.apply_delta(u.sequence, &u.bids, &u.asks), "update {i} applies");
        }
        let now = match (l2.best_bid(), l2.best_ask()) {
            (Some(b), Some(a)) => {
                assert!(
                    b.price < a.price,
                    "update {i}: crossed or locked at {} / {}",
                    b.price,
                    a.price
                );
                Some((b.price, b.qty, a.price, a.qty))
            }
            _ => None,
        };
        if now != last {
            last = now;
            tops.extend(now);
        }
    }
    (l2, tops)
}

fn quotes(book: &SyntheticOrderBook) -> Vec<Top> {
    book.quotes.iter().map(|q| (q.bid, q.bid_size, q.ask, q.ask_size)).collect()
}

#[test]
fn the_defaults_are_the_published_estimates() {
    let m = cst();
    assert_eq!(m.limit_rates[..5], [1.85, 1.51, 1.09, 0.88, 0.77], "CST Table 2");
    assert_eq!(m.cancel_rates[..5], [0.71, 0.81, 0.68, 0.56, 0.47], "CST Table 2");
    assert_eq!((m.limit_rates.len(), m.cancel_rates.len()), (10, 10));
    for i in 6..=10 {
        assert_eq!(m.limit_rates[i - 1], 1.92 / libm::pow(i as f64, 0.52), "the power-law fit");
        assert_eq!(m.cancel_rates[i - 1], 0.47, "theta(i) = theta(5) beyond 5");
    }
    assert!((m.limit_rates[5] - 0.757).abs() < 0.001, "{}", m.limit_rates[5]);
    assert_eq!((m.market_rate, m.tick_size, m.order_size), (0.94, 100.0, 1.0));
    let SyntheticInitialQueues::Poisson(means) = &m.initial_queues else {
        panic!("the default start is Poisson: {:?}", m.initial_queues);
    };
    assert_eq!(means[0], 1.85 / 0.71);
    assert_eq!(means.len(), 10);
}

#[test]
fn a_stream_is_one_snapshot_then_contiguous_deltas_and_its_quotes_are_its_tops() {
    let end = T0 + 240 * MINUTE;
    let book = order_book_stream(&cst(), START_PRICE, T0, end, 21).unwrap();
    assert!(book.updates.len() > 5_000, "{} updates in four hours", book.updates.len());
    let (_, tops) = fold(&book, 100.0, T0, end);
    assert_eq!(quotes(&book), tops, "a quote at the start and at every change of the top");
    assert_eq!(book.quotes[0].timestamp, T0);
    for w in book.quotes.windows(2) {
        assert!(w[0].timestamp <= w[1].timestamp);
    }
    assert!(!book.trades.is_empty());
    for t in &book.trades {
        assert!(t.timestamp >= T0 && t.timestamp < end && t.size == 1.0, "{t:?}");
        assert!(book.updates.iter().any(|u| u.timestamp == t.timestamp), "a trade moves the book");
    }
    let rows = book.updates.iter().map(|u| u.symbol.as_str());
    assert!(rows.chain(book.quotes.iter().map(|q| q.symbol.as_str())).all(str::is_empty));
}

#[test]
fn one_seed_is_one_stream_and_another_seed_is_another() {
    let end = T0 + 30 * MINUTE;
    let run =
        |seed| format!("{:?}", order_book_stream(&cst(), START_PRICE, T0, end, seed).unwrap());
    assert_eq!(run(5), run(5));
    assert_ne!(run(5), run(6));
}

/// With no cancel and no market order nothing leaves the book, so the best quotes never move and the
/// orders at distance `i` on each side are the starting one plus a Poisson count of mean
/// `lambda(i) * minutes`.
#[test]
fn limit_orders_arrive_at_their_rate_at_each_distance() {
    let rates = cst().limit_rates;
    let m = model(rates.clone(), vec![0.0; 10], 0.0, vec![1; 10], vec![1; 10]);
    let minutes = 600;
    let end = T0 + minutes * MINUTE;
    let book = order_book_stream(&m, START_PRICE, T0, end, 3).unwrap();
    assert!(book.trades.is_empty());
    let (l2, _) = fold(&book, 100.0, T0, end);
    let (bid, ask) = (743.0, 744.0); // the best ticks, which nothing moves
    for (i, rate) in rates.iter().enumerate() {
        let expected = rate * minutes as f64;
        let allowed = 5.0 * expected.sqrt();
        let distance = (i + 1) as f64;
        let on_bid = l2.bid_quantity_at((ask - distance) * 100.0) - 1.0;
        let on_ask = l2.ask_quantity_at((bid + distance) * 100.0) - 1.0;
        for got in [on_bid, on_ask] {
            assert!((got - expected).abs() < allowed, "distance {distance}: {got} vs {expected}");
        }
    }
}

/// Each resting order is cancelled after an exponential time; with one rate at every distance and no
/// other event, the orders left after `t` minutes are a binomial draw with probability `e^-theta*t`.
#[test]
fn each_resting_order_is_cancelled_at_its_rate() {
    let theta = 0.2;
    let m = model(vec![0.0; 10], vec![theta; 10], 0.0, vec![40; 10], vec![40; 10]);
    let end = T0 + 5 * MINUTE;
    let book = order_book_stream(&m, START_PRICE, T0, end, 8).unwrap();
    let (l2, _) = fold(&book, 100.0, T0, end);
    let (bids, asks) = l2.top_n(100);
    let mut left = 0.0;
    for level in bids.iter().chain(&asks) {
        left += level.qty;
    }
    let survive = libm::exp(-theta * 5.0);
    let deviation = (800.0 * survive * (1.0 - survive)).sqrt();
    assert!((left - 800.0 * survive).abs() < 5.0 * deviation, "{left} vs {}", 800.0 * survive);
}

/// Market orders arrive at `mu` per side, take one unit each at the best and print a trade there.
#[test]
fn market_orders_take_the_best_at_their_rate_and_print_trades() {
    let mut m = model(vec![0.0], vec![0.0], 0.94, vec![2_000], vec![2_000]);
    m.order_size = 2.5;
    let minutes = 600;
    let end = T0 + minutes * MINUTE;
    let book = order_book_stream(&m, START_PRICE, T0, end, 4).unwrap();
    let expected = 2.0 * 0.94 * minutes as f64;
    let count = book.trades.len() as f64;
    assert!((count - expected).abs() < 5.0 * expected.sqrt(), "{count} trades vs {expected}");
    let buys = book.trades.iter().filter(|t| !t.is_buyer_maker).count();
    for t in &book.trades {
        let price = if t.is_buyer_maker { 74_300.0 } else { 74_400.0 };
        assert_eq!((t.price, t.size), (price, 2.5), "{t:?}");
    }
    let (l2, _) = fold(&book, 100.0, T0, end);
    assert_eq!(l2.ask_quantity_at(74_400.0), (2_000 - buys) as f64 * 2.5);
    assert_eq!(l2.bid_quantity_at(74_300.0), (2_000 - (book.trades.len() - buys)) as f64 * 2.5);
}

/// CST's Table 3 (Laplace column): with a one-tick spread, `b` orders at the bid and `a` at the ask,
/// the mid moves UP first with probability 0.336 for `(b, a) = (1, 2)` and 0.741 for `(3, 1)` under
/// Table 2's `lambda(1)`, `theta(1)` and `mu`. Before the mid first moves only the two best queues
/// matter (the paper's Lemma 2), so a one-level model with a small queue behind each best is enough.
#[test]
fn the_mid_moves_up_first_with_the_published_probability() {
    for (b, a, published) in [(1, 2, 0.336), (3, 1, 0.741)] {
        let m = model(vec![1.85], vec![0.71], 0.94, vec![b, 3], vec![a, 3]);
        let (runs, mut up, mut moved) = (4_000, 0, 0);
        for seed in 0..runs {
            let book = order_book_stream(&m, START_PRICE, T0, T0 + 30 * MINUTE, seed).unwrap();
            let first = &book.quotes[0];
            let Some(change) =
                book.quotes.iter().find(|q| (q.bid, q.ask) != (first.bid, first.ask))
            else {
                continue;
            };
            moved += 1;
            if change.ask > first.ask {
                up += 1;
            } else {
                assert!(change.bid < first.bid, "a one-tick spread moves only outward: {change:?}");
            }
        }
        assert!(moved >= runs * 99 / 100, "{moved} of {runs} runs saw the mid move");
        let p = up as f64 / moved as f64;
        let allowed = 4.0 * (published * (1.0 - published) / moved as f64).sqrt();
        assert!((p - published).abs() < allowed, "(b, a) = ({b}, {a}): {p} vs {published}");
    }
}

#[test]
fn an_emptied_side_trades_nothing_and_quotes_nothing() {
    let m = model(vec![0.0], vec![0.0], 1.0, vec![2], vec![1]);
    let end = T0 + 120 * MINUTE;
    let book = order_book_stream(&m, START_PRICE, T0, end, 2).unwrap();
    assert_eq!(book.trades.len(), 3, "three resting orders, three trades, then nothing to take");
    let (l2, tops) = fold(&book, 100.0, T0, end);
    assert!(l2.best_bid().is_none() && l2.best_ask().is_none());
    assert_eq!(quotes(&book), tops, "no quote once a side is empty");
}

#[test]
fn every_bad_parameter_is_refused_in_words() {
    let refused = |m: SyntheticOrderBookModel, price: f64, end: i64, words: &str| {
        let why = order_book_stream(&m, price, T0, end, 0).unwrap_err();
        assert!(why.contains(words), "{why}");
    };
    let end = T0 + MINUTE;
    refused(
        SyntheticOrderBookModel { limit_rates: vec![], ..cst() },
        START_PRICE,
        end,
        "at least one",
    );
    refused(
        SyntheticOrderBookModel { cancel_rates: vec![0.1], ..cst() },
        START_PRICE,
        end,
        "per limit",
    );
    let mut negative = cst();
    negative.limit_rates[2] = -1.0;
    refused(negative, START_PRICE, end, "limit_rates[2]");
    refused(
        SyntheticOrderBookModel { market_rate: f64::NAN, ..cst() },
        START_PRICE,
        end,
        "market_rate",
    );
    refused(SyntheticOrderBookModel { tick_size: 0.0, ..cst() }, START_PRICE, end, "tick_size");
    refused(SyntheticOrderBookModel { order_size: -1.0, ..cst() }, START_PRICE, end, "order_size");
    let crowded = SyntheticInitialQueues::Poisson(vec![1e6]);
    refused(
        SyntheticOrderBookModel { initial_queues: crowded, ..cst() },
        START_PRICE,
        end,
        "at most",
    );
    refused(cst(), 50.0, end, "at least one tick");
    refused(cst(), START_PRICE, T0, "end after it starts");
    let deep = model(vec![1.0], vec![1.0], 1.0, vec![1, 1, 1], vec![1]);
    refused(deep, 100.0, end, "too low");
}
