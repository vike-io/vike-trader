# Vike agent skills

One directory per procedure, one `SKILL.md` each, in the [Agent Skills](https://code.claude.com/docs/en/skills)
format. Each teaches an agent something it can do with `vike-cli mcp` — the MCP server this
repository ships — and each carries the trigger phrases that decide when it fires.

```sh
npx skills add vike-io/vike-trader
```

| skill | tools it calls | what it is for |
| --- | --- | --- |
| [`arm-a-venue`](arm-a-venue/SKILL.md) | `node_snapshot` | What to do when a user asks an agent to arm, enable, activate, go live on, switch to demo on, connect, or "turn on" a venue (binance, bybit, okx, deribit, or any other venue) for real trading through Vike. |
| [`arm-the-dead-man-switches`](arm-the-dead-man-switches/SKILL.md) | `node_snapshot` | Configure what a running vike-tradehub node does when it loses sight of a venue while orders are resting — the CONNECTION dead-man (`link_deadman_grace_ms`, on by default) and the SILENCE dead-man (`deadman_timeout_ms`, off by default), both set only in `<project>/settings/policy.toml`. |
| [`connect-to-a-node`](connect-to-a-node/SKILL.md) | `list_templates`, `node_snapshot` | Get vike-cli mcp talking to a running vike-tradehub node so node_snapshot and the order-write tools work. |
| [`get-market-data`](get-market-data/SKILL.md) | `list_series` | Get market data into the local history store so a backtest has something to run over. |
| [`manage-the-credential-store`](manage-the-credential-store/SKILL.md) | none | Inspect and write the ONE credential store Vike reads, `<project>/settings/secrets.env`, from the command line. |
| [`read-a-backtest-report`](read-a-backtest-report/SKILL.md) | `run_backtest` | Read a vike backtest report honestly — every BacktestReport row (final_equity, total_return, n_trades, win_rate, sharpe, max_drawdown, profit_factor, funding_paid, per_symbol_pnl, zero_trade), what the Sharpe annualization factor is for the profile's interval, the overfitting checks (n_candidates, deflated Sharpe, PBO, walk-forward consistency) to run before believing a Sharpe, and what the compact report cannot tell you (no trade list, no equity curve, no Monte Carlo). |
| [`record-and-replay-a-session`](record-and-replay-a-session/SKILL.md) | `list_series`, `run_backtest` | Record a live market feed to a tape with `vike-backend datahub --record`, prove the recorder is really recording, then replay the result — a recorded market TAPE through the backtest engine via list_series + run_backtest, or a live session's COMMAND JOURNAL through the real core via replay_journal. |
| [`run-a-backtest`](run-a-backtest/SKILL.md) | `list_strategies`, `list_series`, `run_backtest` | Run a backtest of a strategy over stored history on the vike-datahub data service and read the report back. |
| [`run-a-backtest-locally`](run-a-backtest-locally/SKILL.md) | none | Run a backtest or a parameter search on THIS machine with `vike-cli backtest --local`, against a local history store, with no vike-datahub server anywhere. |
| [`stop-liquidate-restart`](stop-liquidate-restart/SKILL.md) | `node_snapshot`, `set_trading_state`, `mass_cancel`, `flatten`, `market_exit` | Stop trading safely on a running vike-tradehub node from an agent — halt new risk (set_trading_state halted / reducing / active), cancel every resting order (mass_cancel), close positions (flatten, market_exit), and restart cleanly. |
| [`sweep-and-walk-forward`](sweep-and-walk-forward/SKILL.md) | `run_sweep`, `run_walk_forward`, `run_backtest` | Optimize a strategy's parameters honestly on the Vike datahub - expand a [sweep] grid with run_sweep, rank it (rank_by sharpe/return/max_dd/equity), then prove the winner out of sample with run_walk_forward's anchored windows and read wf_consistency before believing the rank. |
| [`trade-on-a-node`](trade-on-a-node/SKILL.md) | `node_snapshot`, `submit_order`, `cancel_order`, `modify`, `flatten`, `mass_cancel`, `set_trading_state`, `market_exit` | Place, change or cancel orders on a running vike-tradehub node through the vike-cli mcp write tools (submit_order, cancel_order, modify, flatten, mass_cancel, market_exit, set_trading_state) using the mandatory two-call preview gate — preview first, then confirm with the preview_token — reading node_snapshot before and after, and settling an unknown outcome before any retry. |
| [`triage-a-stuck-order`](triage-a-stuck-order/SKILL.md) | `node_snapshot`, `cancel_order` | Diagnose an order on a running vike-tradehub node that is neither filled nor gone — stuck, hanging, pending, un-acked, still Submitted, never accepted, not moving, missing from the venue, or an OrphanLocalOrder / MissingTerminal / UnknownOrder reconciliation alert. |
| [`write-a-rhai-strategy`](write-a-rhai-strategy/SKILL.md) | `list_templates`, `list_indicators`, `discover_params`, `validate_strategy`, `run_backtest` | Author a Rhai trading strategy for Vike offline — start from a starter template, check every indicator name against the host-bound roster, declare param() knobs, compile it (compile IS validation), read the knobs back, then backtest it. |

## How to read a page

Each page's frontmatter carries the `description` an agent matches against (it is the whole
trigger — the format has no separate field), and a `metadata.tools` list naming the MCP tools the
procedure calls. Every tool named there is one the server really serves: the roster is held equal
to `crates/vike-cli/src/cmd/mcp.rs`'s `tools_spec` by a test in that same file, so a renamed tool
reddens the skill that teaches it.

## These pages are generated

The tables of tool arguments, exit codes, `vike-cli` verbs and policy bounds are rendered from the
code they describe — `tools_spec`, `crates/vike-cli/src/exit.rs`'s `Exit` enum,
`crates/vike-cli/src/lib.rs`'s `COMMANDS`, `crates/vike-config/src/policy.rs` — and re-rendered and
compared byte for byte on every change. The procedures around them are written by hand, because a
generator has nothing to say about the order of the steps or about when NOT to use a skill.

That is also why this table has no total written under it: a hand-maintained count drifts from the
set it counts, so the roster above is rendered from the directories that ship and there is no
second number anywhere to disagree with it.
