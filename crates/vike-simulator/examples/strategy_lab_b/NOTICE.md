# Notice

Each file under `strategies/` but `mod.rs` is a published strategy, rewritten by hand in Rust for vike. The strategies whose
licence permits it moved to the shipped strategy files (the repository root's `strategies/`, whose `NOTICE` covers them); the
files left here are the ones not taken. Each opens with its original's licence and attribution:

- the licence, as an SPDX line: `MPL-2.0` where the original states it; `MPL-2.0` where the original states none, by the terms
  of the site that publishes it (the header says so); or `MIT`, with the original's copyright line;
- `Derived from "<title>" by <author>, <page>`: the original's own title, its author and the page it is published on;
- `Changed: rewritten by hand in Rust for vike.`: the file is a changed version of the original, not a copy of it.

The rest of this folder (`main.rs`, `markets.rs`, `sizing.rs`, `timeframe.rs`, `strategies/mod.rs`), and the `desk.rs` and `bars.rs` it
shares from `../strategy_lab/`, are vike's own, under the workspace's licence (`LICENSE.md` at the root).

The reference numbers the harness checks against (each original's trade count and net profit over a window) and the bars it
runs on are not part of this folder: `main.rs` reads both from the directory named on its command line.
