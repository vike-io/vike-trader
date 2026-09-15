"""Generate the `cheap_np` parity fixtures from the staged reference cache.

Provenance / how to re-run (needs the Python oracle's venv, which has polars + scipy):

    C:/Projects/vike_db_data_jobs/.venv/Scripts/python.exe \
        crates/vike-backtest/tests/fixtures/cheap_np/make_fixtures.py

Inputs (gitignored, staged at the repo root under `.cheapnp_ref/`):
  * `signals.parquet`     — 7,147,999 pre-joined taker-BUY prints with spot state attached.
  * `resolutions.parquet` — (condition_id, winning_index) settlement truth.
  * `gen_reference.py`    — the Python that produced `cheap_np_ref.parquet`; the parity spec.

Outputs:
  * `.cheapnp_ref/signals.bin`        (gitignored, ~340 MB) — the FULL 7.1M-row row set in a
    fixed-width little-endian record layout the `#[ignore]`d Rust full-parity test mmaps/reads
    with `std::fs` alone (no parquet/arrow dependency added to the workspace).
  * `signals_sample.csv`              (committed, ~1 MB) — every raw print of a deterministic
    sample of whole 5-minute windows, so the ALWAYS-RUN Rust test exercises the same gate
    (band -> TTE -> theta -> first-per-window -> resolution join) end to end.
  * `expected.json`                   (committed) — the Python-computed aggregates for BOTH the
    full file and the committed sample.
  * `trailing_sigma.json`             (committed) — pinned `strategy.trailing_sigma` outputs over
    synthetic sample series (dense / sparse / warmup / duplicate-second / cutoff / long-gap),
    hex-exact, so the Rust port's 1s-grid + linear-gap-interpolation semantics are proven
    against the oracle even though `signals.parquet` ships sigma pre-computed.

The record layout of `signals.bin` (48 bytes, little-endian, `<IIiBbxx dddd`):
    u32 sts | u32 fts | i32 t | u8 outcome_index | i8 winning_index (-1 = unresolved) | 2 pad
    f64 ask | f64 s_open | f64 s_now | f64 sigma_sec
Rows are written in the parquet's own order; the Rust side re-sorts exactly as polars does.
"""

from __future__ import annotations

import json
import math
import struct
import sys
from pathlib import Path

import numpy as np
import polars as pl
from scipy.special import erf

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[4]  # crates/vike-backtest/tests/fixtures/cheap_np -> repo root
REF = ROOT / ".cheapnp_ref"

sys.path.insert(0, "C:/Projects/vike_db_data_jobs")
from trading.fair_value_bot import strategy as S  # noqa: E402  (oracle, read-only)

THETA = 0.055
BETAS = (0.83, 1.36)
H = 300.0
CHEAP_LO, CHEAP_HI = 0.10, 0.35
CHEAP_TTE_LO, CHEAP_TTE_HI = 15.0, 270.0
SQRT2 = np.sqrt(2.0)

SAMPLE_WINDOWS = 60  # committed-CSV size knob: whole windows, evenly spaced over the whole range

REC = struct.Struct("<IIiBbxxdddd")
assert REC.size == 48, REC.size
# entry record: u32 sts | u32 fts | u8 outcome_index | i8 winning_index | 2 pad | f64 ask | f64 edge
ENTRY = struct.Struct("<IIBbxxdd")
assert ENTRY.size == 28, ENTRY.size


def fee(p):
    return 0.072 * p * (1.0 - p)


def wc_edge(oidx, ask, s_now, s_open, sigma, t):
    """Dual-beta worst-case (min) edge — verbatim from `.cheapnp_ref/gen_reference.py`."""
    denom = sigma * np.sqrt(np.maximum(H - t, 1e-9))
    lr = np.log(s_now / s_open)
    f = fee(ask)
    out = None
    for b in BETAS:
        pu = 0.5 * (1.0 + erf((lr * b / denom) / SQRT2))
        prob = np.where(oidx == 0, pu, 1.0 - pu)
        e = prob - ask - f
        out = e if out is None else np.minimum(out, e)
    return out


def run_gate(sig: pl.DataFrame) -> dict:
    """The exact `gen_reference.py` gate, returning every intermediate count + the aggregates."""
    tte = H - sig["t"].to_numpy().astype(np.float64)
    ask = sig["ask"].to_numpy().astype(np.float64)
    keep = (ask >= CHEAP_LO) & (ask < CHEAP_HI) & (tte >= CHEAP_TTE_LO) & (tte <= CHEAP_TTE_HI)
    q = sig.filter(pl.Series(keep))
    n_band = q.height

    e = wc_edge(
        q["outcome_index"].to_numpy(),
        q["ask"].to_numpy().astype(np.float64),
        q["s_now"].to_numpy().astype(np.float64),
        q["s_open"].to_numpy().astype(np.float64),
        q["sigma_sec"].to_numpy().astype(np.float64),
        q["t"].to_numpy().astype(np.float64),
    )
    q = q.with_columns(edge=pl.Series(e)).filter(pl.col("edge") > THETA)
    n_theta = q.height

    entries = q.sort(["slug", "fts", "outcome_index", "ask"]).group_by("slug", maintain_order=True).first()
    n_entries = entries.height

    j = entries.filter(pl.col("winning_index") >= 0)
    n_joined = j.height
    j = j.with_columns(won=(pl.col("outcome_index") == pl.col("winning_index")).cast(pl.Float64))
    j = j.with_columns(pnl=pl.col("won") - pl.col("ask") - (0.072 * pl.col("ask") * (1 - pl.col("ask"))))

    return {
        "rows": sig.height,
        "after_band_tte": n_band,
        "after_theta": n_theta,
        "entries": n_entries,
        "after_resolution_join": n_joined,
        "win_rate": float(j["won"].mean()),
        "avg_ask": float(j["ask"].mean()),
        "total_pnl": float(j["pnl"].sum()),
        "pnl_per_trade": float(j["pnl"].mean()),
        "sum_edge": float(j["edge"].sum()),
    }, j.sort(["sts", "fts"])


def main() -> None:
    sig = pl.read_parquet(
        REF / "signals.parquet",
        columns=["slug", "condition_id", "sts", "fts", "t", "outcome_index", "ask", "s_open", "s_now", "sigma_sec"],
    )
    res = pl.read_parquet(REF / "resolutions.parquet").select(["condition_id", "winning_index"]).unique(
        subset=["condition_id"]
    )
    # Attach winning_index per ROW (slug <-> condition_id is 1:1 here, verified), so the join can be
    # applied post-entry-selection exactly as gen_reference.py does it. -1 = no on-chain resolution.
    sig = sig.join(res, on="condition_id", how="left").with_columns(
        pl.col("winning_index").fill_null(-1).cast(pl.Int8)
    )
    print(f"rows={sig.height} slugs={sig['slug'].n_unique()}", file=sys.stderr)

    full, full_entries = run_gate(sig)
    print(json.dumps(full, indent=2), file=sys.stderr)

    # entry-for-entry reference (the 12,639 settled entries), for the ignored full-file diff
    ebin = REF / "entries.bin"
    with ebin.open("wb") as fh:
        for r in full_entries.select(["sts", "fts", "outcome_index", "winning_index", "ask", "edge"]).iter_rows():
            fh.write(ENTRY.pack(r[0], r[1], r[2], r[3], r[4], r[5]))
    print(f"wrote {ebin} ({full_entries.height} entries)", file=sys.stderr)

    # ---- full binary (gitignored) ------------------------------------------------------------
    out = REF / "signals.bin"
    cols = sig.select(["sts", "fts", "t", "outcome_index", "winning_index", "ask", "s_open", "s_now", "sigma_sec"])
    with out.open("wb") as fh:
        buf = bytearray()
        for row in cols.iter_rows():
            buf += REC.pack(row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7], row[8])
            if len(buf) >= 1 << 22:
                fh.write(buf)
                buf = bytearray()
        fh.write(buf)
    print(f"wrote {out} ({out.stat().st_size} bytes)", file=sys.stderr)

    # ---- committed CSV sample ---------------------------------------------------------------
    slugs = sorted(sig["slug"].unique().to_list())
    step = max(1, len(slugs) // SAMPLE_WINDOWS)
    picked = slugs[::step][:SAMPLE_WINDOWS]
    samp = sig.filter(pl.col("slug").is_in(picked))
    sample_stats, sample_entries = run_gate(samp)
    print(json.dumps(sample_stats, indent=2), file=sys.stderr)

    ecsv = HERE / "entries_sample.csv"
    with ecsv.open("w", newline="\n", encoding="ascii") as fh:
        fh.write("sts,fts,outcome_index,winning_index,ask,edge\n")
        for r in sample_entries.select(
            ["sts", "fts", "outcome_index", "winning_index", "ask", "edge"]
        ).iter_rows():
            fh.write(f"{r[0]},{r[1]},{r[2]},{r[3]},{r[4]!r},{r[5]!r}\n")
    print(f"wrote {ecsv} ({sample_entries.height} entries)", file=sys.stderr)

    csv = HERE / "signals_sample.csv"
    with csv.open("w", newline="\n", encoding="ascii") as fh:
        fh.write("sts,fts,t,outcome_index,winning_index,ask,s_open,s_now,sigma_sec\n")
        for r in samp.select(
            ["sts", "fts", "t", "outcome_index", "winning_index", "ask", "s_open", "s_now", "sigma_sec"]
        ).iter_rows():
            fh.write(
                f"{r[0]},{r[1]},{r[2]},{r[3]},{r[4]},{r[5]!r},{r[6]!r},{r[7]!r},{r[8]!r}\n"
            )
    print(f"wrote {csv} ({csv.stat().st_size} bytes, {samp.height} rows)", file=sys.stderr)

    (HERE / "expected.json").write_text(
        json.dumps({"full": full, "sample": sample_stats}, indent=2) + "\n", encoding="ascii"
    )

    # ---- pinned trailing_sigma vectors -------------------------------------------------------
    cases = []

    def case(name, samples, lookback, now):
        v = S.trailing_sigma(samples, lookback, now)
        cases.append(
            {
                "name": name,
                "lookback_s": lookback,
                "now": now,
                "samples": [[float(a), float(b)] for a, b in samples],
                "sigma": v,
                "sigma_bits": (None if v is None else format(struct.unpack("<Q", struct.pack("<d", v))[0], "016x")),
            }
        )

    def walk(n, seed, start=60000.0, vol=0.0004):
        rng = np.random.default_rng(seed)
        px = [start]
        for _ in range(n - 1):
            px.append(px[-1] * math.exp(float(rng.normal(0.0, vol))))
        return [round(p, 2) for p in px]

    t0 = 1_772_323_200.0

    # 1. dense contiguous 60s
    px = walk(60, 1)
    case("dense_60s", [(t0 + i, px[i]) for i in range(60)], 1800, t0 + 59)

    # 2. sparse: ~11% of seconds missing over 200s (the real spot_1s shape) -> interpolation matters
    px = walk(200, 2)
    rng = np.random.default_rng(99)
    drop = set(int(i) for i in rng.choice(np.arange(1, 199), size=22, replace=False))
    case("sparse_11pct_200s", [(t0 + i, px[i]) for i in range(200) if i not in drop], 1800, t0 + 199)

    # 3. below the 30-observed-second warmup floor -> None
    px = walk(29, 3)
    case("warmup_29s_none", [(t0 + i, px[i]) for i in range(29)], 1800, t0 + 28)

    # 4. duplicate samples inside one second: LAST write wins
    px = walk(40, 4)
    dup = []
    for i in range(40):
        dup.append((t0 + i + 0.10, px[i] * 1.0005))
        dup.append((t0 + i + 0.90, px[i]))
    case("dup_second_last_wins", dup, 1800, t0 + 39)

    # 5. cutoff: samples older than now-lookback are dropped entirely
    px = walk(80, 5)
    case("cutoff_drops_old", [(t0 + i, px[i]) for i in range(80)], 40, t0 + 79)

    # 6. one long stall in the middle -> a smooth ramp, not one giant return
    px = walk(80, 6)
    stalled = [(t0 + i, px[i]) for i in range(80) if not (30 <= i < 55)]
    case("long_gap_ramp", stalled, 1800, t0 + 79)

    (HERE / "trailing_sigma.json").write_text(json.dumps(cases, indent=2) + "\n", encoding="ascii")
    print(f"wrote {HERE / 'trailing_sigma.json'} ({len(cases)} cases)", file=sys.stderr)


if __name__ == "__main__":
    main()
