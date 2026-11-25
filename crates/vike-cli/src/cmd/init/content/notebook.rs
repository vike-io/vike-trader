//! The notebook `init` ships, over the results the user saves. No sample result ships: a result
//! nobody ran would be invented numbers (the owner's ruling of 2026-11-13, no fake data).

/// `notebooks/backtest_report.ipynb` — a minimal, valid nbformat-4 notebook.
///
/// Standard library only in the cells that matter, so it runs in any interpreter without an
/// install; the one cell needing `matplotlib` is last and announces itself. Kept small on purpose —
/// it is a starting point to edit, not a dashboard to maintain.
///
/// ⚠ The literal is delimited `r####"…"####` because the notebook is JSON whose STRINGS are
/// markdown: a cell beginning `"## Compare several runs` contains `"##`, which closes an `r##"`
/// literal in the middle of the file. Anything shorter than four hashes is a compile error waiting
/// for the next heading somebody adds.
pub const NOTEBOOK_IPYNB: &str = r####"{
 "cells": [
  {
   "cell_type": "markdown",
   "metadata": {},
   "source": [
    "# Backtest report\n",
    "\n",
    "Reads the newest result saved in `../backtest_results/` and prints a summary.\n",
    "\n",
    "No result ships: save one first, e.g. from `user_data/`:\n",
    "`vike-cli backtest run --local --strategy <FILE> --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> --from <FROM> --to <TO> --cash <CASH> --json > backtest_results/run.json`.\n",
    "Set `RESULT` below to read another file."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "import json\n",
    "from pathlib import Path\n",
    "\n",
    "saved = sorted(Path('../backtest_results').glob('*.json'), key=lambda p: p.stat().st_mtime)\n",
    "if not saved:\n",
    "    raise SystemExit('No saved result yet in ../backtest_results/ — run a backtest with --json and save it there.')\n",
    "RESULT = saved[-1]\n",
    "\n",
    "report = json.loads(RESULT.read_text(encoding='utf-8'))\n",
    "report"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "# A readable summary. Percentages are stored as fractions, so 0.0731 -> 7.31%.\n",
    "def pct(x):\n",
    "    return 'n/a' if x is None else f'{x * 100:.2f}%'\n",
    "\n",
    "def num(x, places=4):\n",
    "    return 'n/a' if x is None else f'{x:.{places}f}'\n",
    "\n",
    "rows = [\n",
    "    ('name',          report.get('name')),\n",
    "    ('final equity',  num(report.get('final_equity'), 2)),\n",
    "    ('total return',  pct(report.get('total_return'))),\n",
    "    ('trades',        report.get('n_trades')),\n",
    "    ('win rate',      pct(report.get('win_rate'))),\n",
    "    ('sharpe',        num(report.get('sharpe'))),\n",
    "    ('max drawdown',  pct(report.get('max_drawdown'))),\n",
    "    ('profit factor', num(report.get('profit_factor'))),\n",
    "]\n",
    "width = max(len(label) for label, _ in rows)\n",
    "for label, value in rows:\n",
    "    print(f'{label:<{width}}  {value}')\n",
    "\n",
    "# profit_factor is null when there is no meaningful ratio (no losing trades, or no trades).\n",
    "# sharpe is annualized with 252 periods unless the run was on daily bars: compare runs to each\n",
    "# other, not to a published number."
   ]
  },
  {
   "cell_type": "markdown",
   "metadata": {},
   "source": [
    "## Compare several runs\n",
    "\n",
    "Once you have saved more than one result, this reads the whole folder at once."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "results = []\n",
    "for path in sorted(Path('../backtest_results').glob('*.json')):\n",
    "    data = json.loads(path.read_text(encoding='utf-8'))\n",
    "    results.append((path.name, data.get('total_return'), data.get('sharpe'), data.get('n_trades')))\n",
    "\n",
    "print(f\"{'file':<34}{'return':>10}{'sharpe':>10}{'trades':>9}\")\n",
    "for name, ret, sharpe, trades in results:\n",
    "    r = 'n/a' if ret is None else f'{ret * 100:.2f}%'\n",
    "    s = 'n/a' if sharpe is None else f'{sharpe:.3f}'\n",
    "    print(f'{name:<34}{r:>10}{s:>10}{trades:>9}')"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "# Optional: needs matplotlib (`pip install matplotlib`). Skip this cell if you do not have it.\n",
    "import matplotlib.pyplot as plt\n",
    "\n",
    "names = [r[0] for r in results]\n",
    "returns = [(r[1] or 0.0) * 100 for r in results]\n",
    "\n",
    "fig, ax = plt.subplots(figsize=(8, 0.5 * len(names) + 1.5))\n",
    "ax.barh(names, returns)\n",
    "ax.set_xlabel('total return (%)')\n",
    "ax.axvline(0, linewidth=0.8, color='black')\n",
    "plt.tight_layout()"
   ]
  }
 ],
 "metadata": {
  "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
  "language_info": {"name": "python", "version": "3"}
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
"####;
