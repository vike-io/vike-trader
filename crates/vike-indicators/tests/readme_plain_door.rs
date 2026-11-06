//! The README's "Without `Bar`: plain numbers" example, verbatim, so the text cannot drift from the
//! API: a signature that changes breaks this file before it breaks a reader.

use vike_indicators::indicators::{Atr, Rsi};
use vike_indicators::series;

#[test]
fn the_plain_number_door_example_runs_and_agrees_with_itself()
-> Result<(), Box<dyn std::error::Error>> {
    // Plain columns: no `Bar`, no `vike-marketdata`.
    let close: Vec<f64> = (0..200).map(|i| 100.0 + (i as f64 * 0.1).sin() * 5.0).collect();
    let high: Vec<f64> = close.iter().map(|c| c + 1.0).collect();
    let low: Vec<f64> = close.iter().map(|c| c - 1.0).collect();

    // A whole series in one call: one value per input, NaN while warming up.
    let rsi = series::rsi(&close, 14.0)?;
    let [macd, signal, hist] = series::macd(&close, 12.0, 26.0, 9.0)?;
    let atr = series::atr(&high, &low, &close, 14.0)?;
    println!("RSI {:.2}  MACD {:.3} / {:.3} / {:.3}", rsi[199], macd[199], signal[199], hist[199]);

    // Streaming: the same indicator types, fed plain numbers (`next_value` for a close,
    // `next_hlc` for high, low and close).
    let mut r = Rsi::new(14.0)?;
    let mut a = Atr::new(14.0)?;
    for i in 0..close.len() {
        let (streamed_rsi, streamed_atr) =
            (r.next_value(close[i]), a.next_hlc(high[i], low[i], close[i]));
        // The stream and the series give exactly the same bits (two NaNs count as equal).
        assert_eq!(streamed_rsi.to_bits(), rsi[i].to_bits());
        assert_eq!(streamed_atr.to_bits(), atr[i].to_bits());
    }
    Ok(())
}

/// The README's "Two ways to use it" snippets, line for line, so the front page cannot drift from the API.
#[test]
fn the_two_ways_to_use_it_agree_bit_for_bit() -> Result<(), Box<dyn std::error::Error>> {
    use vike_marketdata::Bar;

    let close: Vec<f64> = (0..200).map(|i| 100.0 + (i as f64 * 0.1).sin() * 5.0).collect();
    let bars: Vec<Bar> = close
        .iter()
        .enumerate()
        .map(|(i, &c)| Bar::ohlcv(i as i64 * 60_000, c, c + 1.0, c - 1.0, c, 1_000.0))
        .collect();

    // 1. With bars.
    let mut rsi = Rsi::new(14.0)?;
    let mut streamed = Vec::new();
    for bar in &bars {
        streamed.push(rsi.next(bar));
    }
    let lines =
        vike_indicators::make_with("rsi", &[14.0]).expect("known indicator").vectorize(&bars);

    // 2. With plain numbers.
    let whole = series::rsi(&close, 14.0)?;
    let mut rsi = Rsi::new(14.0)?;
    let streamed_plain: Vec<f64> = close.iter().map(|&c| rsi.next_value(c)).collect();

    for i in 0..close.len() {
        let bits = whole[i].to_bits();
        assert_eq!(streamed[i].to_bits(), bits);
        assert_eq!(lines[0][i].to_bits(), bits);
        assert_eq!(streamed_plain[i].to_bits(), bits);
    }
    Ok(())
}
