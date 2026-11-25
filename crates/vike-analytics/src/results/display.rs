//! `impl fmt::Display for TradingResults`: the compact human table.

use std::fmt;

use super::{TradingResults, TradingResultsInDollars};

/// A coin ledger's final equity in dollars, under the coin figure; nothing for any other run.
fn write_in_dollars(f: &mut fmt::Formatter<'_>, d: Option<TradingResultsInDollars>) -> fmt::Result {
    use crate::metric_catalog::MetricUnit;
    match d {
        Some(d) => writeln!(
            f,
            "in_dollars:    {} (at {} a coin)",
            MetricUnit::Money.render(d.final_equity),
            MetricUnit::Money.render(d.price)
        ),
        None => Ok(()),
    }
}

impl fmt::Display for TradingResults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // ⚠ **The value cells come from the UNIT, and the substitution is BYTE-IDENTICAL to the
        // `{:.4}%` / `{:.2}` / `{:.4}` literals that used to be spelled here.**
        // `MetricUnit::Percent::render` IS `format!("{:.4}%", v * 100.0)` — the same expression in
        // the same order — and `Money`/`Ratio` are the same for `{:.2}`/`{:.4}`. So no fixture and
        // no operator's muscle memory moves; what changes is that this published table can no
        // longer disagree with `--metrics full` about what a percent is, which it could while both
        // spelled the rule for themselves. `the_compact_table_cells_are_the_units_own_renderings`
        // is the pin.
        //
        // `n_trades` deliberately keeps its integer `{}` rather than routing through
        // `MetricUnit::Count`: the catalog renders a count as the `f64` that
        // `TradingResultsExtendedMetrics::value_of` widens it to, and casting a `usize` here to print it would
        // buy nothing and lose exactness above 2^53.
        use crate::metric_catalog::MetricUnit;

        writeln!(f, "name:          {}", self.name.as_deref().unwrap_or("(unnamed)"))?;
        // A strategy whose hooks failed says so before any number: a run that failed on every bar
        // otherwise reads as a normal report. A clean run adds no line, so its table is byte-identical.
        if !self.strategy_failures.is_empty() {
            writeln!(f, "{}", self.strategy_failures)?;
        }
        // Who produced these results and over what span, in the one wording every view uses
        // (`TradingResults::source_label`). Only when the source is known: a report read with a bare
        // `serde_json` parse has none, and its table is byte-identical.
        if let Some(label) = self.source_label() {
            writeln!(f, "source:        {label}")?;
        }
        // ⚠ **A run booked in another currency than the dollar says so, and a dollar run adds no
        // line** (every existing table is byte-identical). Every money cell below, `final_equity` and
        // `per_symbol_pnl` included, is an amount of THIS currency: a USDJPY run that stated
        // `engine.account_currency = "JPY"` prints 10,000 where a dollar reader would take it for 10,000
        // dollars (it is 66.62 at 150.10). The label comes from the realism stamp, so a report with no
        // stamp (an older run) prints nothing rather than guessing.
        if let Some(currency) = self.realism.as_ref().and_then(|r| r.off_dollar_ledger()) {
            writeln!(f, "account_currency:   {currency}")?;
        }
        // What the run was CHARGED, in words, on every stamped report: the fees (the venue's table
        // a profile that states no fee is charged, a flat rate, or `no fees` for a stated free table),
        // the slippage and the funding. A report with no stamp (an older run) prints nothing rather
        // than guessing.
        if let Some(charged) = self.realism.as_ref().and_then(|r| r.charged.as_ref()) {
            writeln!(f, "costs:         {charged}")?;
        }
        // Above the zero-trade branch, because a run with nothing to trade on is the run most likely
        // to have these. A clean run adds no line, so its table is byte-identical.
        if !self.data_warnings.is_empty() {
            writeln!(f, "data_warnings:")?;
            for line in &self.data_warnings {
                writeln!(f, "  {line}")?;
            }
        }
        // ⚠ A COIN LEDGER's money is coins, where two decimals print a 0.0015 BTC gain as nothing: its
        // money cells take eight, and the final equity is said in dollars too. Every other run keeps
        // the unit's rendering, byte for byte.
        let money = |v: f64| match self.in_dollars {
            Some(_) => format!("{v:.8}"),
            None => MetricUnit::Money.render(v),
        };
        // An INVALID run (its equity reached zero) says so first and prints no return, ratio or
        // percentage below: over a curve that crossed zero none of them means anything. `invalid` is
        // `Some` only for such a run, so every other table is byte-identical.
        if let Some(why) = &self.invalid {
            writeln!(f, "INVALID:       {why}")?;
        }
        let valid = self.invalid.is_none();
        // A zero-trade / flat-equity run prints its DIAGNOSIS instead of the bare all-zero metrics
        // table. `zero_trade` is `Some` only for such a run (see `ZeroTradeReport::analyze`), so a
        // run with trades falls straight through to the unchanged table below and is byte-identical.
        if let Some(zt) = &self.zero_trade {
            writeln!(f, "final_equity:  {}", money(self.final_equity))?;
            write_in_dollars(f, self.in_dollars)?;
            writeln!(f, "n_trades:      0")?;
            write!(f, "{zt}")?;
            return Ok(());
        }
        writeln!(f, "final_equity:  {}", money(self.final_equity))?;
        write_in_dollars(f, self.in_dollars)?;
        if valid {
            writeln!(f, "total_return:  {}", MetricUnit::Percent.render(self.total_return))?;
        }
        writeln!(f, "n_trades:      {}", self.n_trades)?;
        if valid {
            writeln!(f, "win_rate:      {}", MetricUnit::Percent.render(self.win_rate))?;
            writeln!(f, "sharpe:        {}", MetricUnit::Ratio.render(self.sharpe))?;
            writeln!(f, "max_drawdown:  {}", MetricUnit::Percent.render(self.max_drawdown))?;
        }
        // Closed-trade accounting's profit factor (net of commission), only on a run that armed it,
        // so every other table is byte-identical. Labelled by its own catalog id: `profit_factor` is
        // the round trips' before-fee figure `--metrics` and `report.json` give under that name.
        if let Some(c) = self.closed_trades.as_ref().filter(|_| valid) {
            let pf = MetricUnit::Ratio.render(c.all.profit_factor);
            writeln!(f, "closed_profit_factor: {pf} (net of commission)")?;
        }
        // Funding P&L: shown ONLY when nonzero, so a spot / no-funding report is byte-identical to
        // before this field existed (same discipline as the omitted profit_factor row). The field is
        // RECEIVED-positive, so the line says so rather than borrowing its `funding_paid` id: "paid
        // 12.34" read as a cost for a run that was paid 12.34. The JSON key and the catalog id stay.
        if self.funding_paid != 0.0 {
            writeln!(f, "funding (net, + = received): {}", money(self.funding_paid))?;
        }
        // Same discipline as `funding_paid` above: printed only when something actually happened,
        // so a clean run's table does not move. `is_noteworthy` deliberately excludes `warmup` —
        // see its doc.
        if let Some(h) = self.honesty.as_ref().filter(|h| h.is_noteworthy()) {
            writeln!(f, "honesty:")?;
            for line in h.to_string().lines() {
                writeln!(f, "  {line}")?;
            }
        }
        if self.per_symbol_pnl.is_empty() {
            writeln!(f, "per_symbol_pnl: (none)")?;
        } else {
            writeln!(f, "per_symbol_pnl:")?;
            for (symbol, pnl) in &self.per_symbol_pnl {
                match self.in_dollars {
                    Some(_) => writeln!(f, "  {symbol}: {pnl:.8}")?,
                    None => writeln!(f, "  {symbol}: {pnl:.2}")?,
                }
            }
        }
        if let Some(table) = self.mounts_table() {
            write!(f, "{table}")?;
        }
        Ok(())
    }
}

impl TradingResults {
    /// **A portfolio's mount table as text**, one line a mount with its own trades and win % (an INVALID run
    /// keeps its money and counts), then what no mount's fills hold; `None` for every other run. The human
    /// table ends with it, and `vike-cli backtest run` prints it under a summary's rows.
    #[must_use]
    pub fn mounts_table(&self) -> Option<String> {
        use crate::metric_catalog::MetricUnit;
        let mounts = self.mounts.as_ref()?;
        // A coin ledger's money takes eight decimals, as the table above prints it.
        let money = |v: f64| match self.in_dollars {
            Some(_) => format!("{v:.8}"),
            None => MetricUnit::Money.render(v),
        };
        let mut out = String::from("mounts:\n");
        for m in &mounts.rows {
            let rates = match self.invalid {
                None => format!(
                    ", win_rate {}, profit_factor {}",
                    MetricUnit::Percent.render(m.win_rate),
                    MetricUnit::Ratio.render(m.profit_factor)
                ),
                Some(_) => String::new(),
            };
            let latched = m.latched_at.map_or_else(
                || "never".to_string(),
                |ms| {
                    chrono::DateTime::from_timestamp_millis(ms).map_or_else(
                        || ms.to_string(),
                        |t| t.format("%Y-%m-%d %H:%M UTC").to_string(),
                    )
                },
            );
            out.push_str(&format!(
                "  [{}] {} ({}): n_trades {}{rates}, pnl {}, leverage_refusals {}, latched {latched}\n",
                m.mount,
                m.strategy_file,
                m.symbols.join(", "),
                m.n_trades,
                money(m.pnl),
                m.leverage_refusals,
            ));
        }
        out.push_str(&format!("  unattributed_pnl: {}\n", money(mounts.unattributed_pnl)));
        Some(out)
    }
}
