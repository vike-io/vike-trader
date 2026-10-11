//! `impl fmt::Display for BacktestReport`: the compact human table.

use std::fmt;

use super::BacktestReport;

impl fmt::Display for BacktestReport {
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
        // `ExtendedMetrics::value_of` widens it to, and casting a `usize` here to print it would
        // buy nothing and lose exactness above 2^53.
        use crate::metric_catalog::MetricUnit;

        writeln!(f, "name:          {}", self.name.as_deref().unwrap_or("(unnamed)"))?;
        // A zero-trade / flat-equity run prints its DIAGNOSIS instead of the bare all-zero metrics
        // table. `zero_trade` is `Some` only for such a run (see `ZeroTradeReport::analyze`), so a
        // run with trades falls straight through to the unchanged table below and is byte-identical.
        if let Some(zt) = &self.zero_trade {
            writeln!(f, "final_equity:  {}", MetricUnit::Money.render(self.final_equity))?;
            writeln!(f, "n_trades:      0")?;
            write!(f, "{zt}")?;
            return Ok(());
        }
        writeln!(f, "final_equity:  {}", MetricUnit::Money.render(self.final_equity))?;
        writeln!(f, "total_return:  {}", MetricUnit::Percent.render(self.total_return))?;
        writeln!(f, "n_trades:      {}", self.n_trades)?;
        writeln!(f, "win_rate:      {}", MetricUnit::Percent.render(self.win_rate))?;
        writeln!(f, "sharpe:        {}", MetricUnit::Ratio.render(self.sharpe))?;
        writeln!(f, "max_drawdown:  {}", MetricUnit::Percent.render(self.max_drawdown))?;
        // Funding P&L: shown ONLY when nonzero, so a spot / no-funding report is byte-identical to
        // before this field existed (same discipline as the omitted profit_factor row).
        if self.funding_paid != 0.0 {
            writeln!(f, "funding_paid:  {}", MetricUnit::Money.render(self.funding_paid))?;
        }
        // ⚠ **A FRICTIONLESS run says so on the human table, and nothing else new does.** The
        // stamp itself is a whole block and belongs behind `--metrics realism`; this one line is
        // here because the harm it answers is specific: `fee_rate` and `slippage` both default to
        // `0.0`, so an unfinished profile and a deliberately costless one printed identical tables,
        // and the first is the one somebody acts on. A COSTED run adds nothing, so every existing
        // report that charged anything is byte-identical.
        if let Some(why) = self.realism.as_ref().and_then(|r| r.frictionless.as_deref()) {
            writeln!(f, "realism:       FRICTIONLESS — {why}")?;
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
            for (sym, pnl) in &self.per_symbol_pnl {
                writeln!(f, "  {sym}: {pnl:.2}")?;
            }
        }
        Ok(())
    }
}
