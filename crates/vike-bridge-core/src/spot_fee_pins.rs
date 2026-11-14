//! The golden grid of a spot fill's fee paid in the base coin: one walk, four venues (`test-support` only).
//!
//! `crate::event_map`'s `spot_base_fee_net` is the arithmetic; how a fee asset is told to be the base and how the
//! quote is spelled stay each venue's. So the pin is the whole row as a venue books it: each bridge hands its own
//! wrapper in as `row` and compares the walk with its own `tests/fixtures/spot_fee_grid.txt`. All four walk the same
//! pair (base `HYPE`, quote `USDC`) and the same numbers, so two venues' files differ exactly where they disagree.
//!
//! A figure is compared by `to_bits()`, the decimal beside it only for the reader. A NaN is written `NaN`: Rust leaves
//! a computed NaN's sign and payload undefined, so its bits are no contract.

use std::fmt::Write as _;

/// The fee of a whole fill of 10 at 37: smaller, a rebate, none, the fill itself, more than the fill, not a number.
const FEES: [f64; 11] = [
    0.0045,
    -0.0045,
    0.0,
    -0.0,
    10.0,
    20.0,
    -10.0,
    -20.0,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

/// Every other spelling of the fee asset: the base in lower case, the quote, a third coin, a prefix of the base, none.
const ASSETS: [&str; 5] = ["hype", "USDC", "PURR", "HY", ""];

/// `(qty, px, fee)` with the fee in the base: the fills the venues measured, and a quantity or price that is no number.
const ROWS: [(f64, f64, f64); 12] = [
    // bybit's two demo buys (`execution_list_spot_buys.json`)
    (165.7, 0.07545, 0.1657),
    (172.299_536_116_633_53, 0.07545, 0.172_299_536_116_633_54),
    // okx's demo buy, binance's ledger buy
    (0.00002, 76927.2, 0.000_000_02),
    (0.00015, 80571.92, 1.5e-7),
    // a remainder under bybit's rounding
    (0.1, 37.0, 0.099_999_999_999_9),
    (0.0, 37.0, 0.0045),
    (0.0, 37.0, 0.0),
    (f64::NAN, 37.0, 0.0045),
    (f64::INFINITY, 37.0, 0.0045),
    (10.0, 0.0, 0.0045),
    (10.0, f64::NAN, 0.0045),
    (10.0, -1.0, 0.0045),
];

fn figure(x: f64) -> String {
    if x.is_nan() { "NaN".to_string() } else { format!("{x}#{:016x}", x.to_bits()) }
}

/// The grid as `row` books it, one line a cell. `row(side, qty, px, fee, fee asset)` answers the fill's
/// `(quantity, commission, commission asset)`, what it was handed where the venue leaves the fill alone.
pub fn spot_fee_grid(row: impl Fn(i32, f64, f64, f64, &str) -> (f64, f64, String)) -> String {
    let mut out = String::new();
    let mut cell = |side: i32, qty: f64, px: f64, fee: f64, asset: &str| {
        let (net, commission, booked) = row(side, qty, px, fee, asset);
        let word = if side > 0 { "BUY " } else { "SELL" };
        let (net, commission) = (figure(net), figure(commission));
        writeln!(
            out,
            "{word} qty={qty} px={px} fee={fee} in {asset:?} -> qty {net} commission {commission} in {booked:?}"
        )
        .expect("a String takes every write");
    };
    for side in [1, -1] {
        for fee in FEES {
            cell(side, 10.0, 37.0, fee, "HYPE");
        }
        for asset in ASSETS {
            for fee in [0.0045, -0.0045, f64::NAN] {
                cell(side, 10.0, 37.0, fee, asset);
            }
        }
        for (qty, px, fee) in ROWS {
            cell(side, qty, px, fee, "HYPE");
        }
    }
    out
}

/// [`spot_fee_grid`] over `row` is `golden`, cell for cell; a failure names each cell that moved and prints the grid.
pub fn assert_spot_fee_grid(
    golden: &str,
    row: impl Fn(i32, f64, f64, f64, &str) -> (f64, f64, String),
) {
    let grid = spot_fee_grid(row);
    let (now, pinned): (Vec<&str>, Vec<&str>) = (grid.lines().collect(), golden.lines().collect());
    let moved: Vec<String> = now
        .iter()
        .zip(pinned.iter())
        .filter(|(n, p)| n != p)
        .map(|(n, p)| format!("  pinned {p}\n  now    {n}"))
        .collect();
    assert!(
        moved.is_empty() && now.len() == pinned.len(),
        "{} cells moved of {} (the pin holds {} lines):\n{}\n--- the grid now ---\n{grid}",
        moved.len(),
        now.len(),
        pinned.len(),
        moved.join("\n"),
    );
}
