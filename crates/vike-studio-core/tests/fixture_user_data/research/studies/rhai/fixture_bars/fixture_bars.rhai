// `fixture_bars` — the fixture that pins the READ half of the interpreted contract.
//
// It exercises every part of a real study's data path against the workspace's own store double:
// both arities of a read verb (the run's own window, and an explicit narrower one), the `column`
// projection that is how a study of any size reads its rows, a per-row property getter, and both
// halves of what a study hands back — a metric and a text artifact.
//
// A study is called ONCE with the context and its recipe, and returns an outcome. That is the same
// entry the Rust tier has; only the types are erased.

fn run(ctx, params) {
    let bars = ctx.bars(params.venue, params.symbol, params.interval);

    // The projection, not a per-row loop: ONE host call walks the rows in Rust. Over a real window
    // the difference is the whole reason `column` exists.
    let closes = column(bars, "close");
    let total = 0.0;
    for c in closes {
        total += c;
    }

    let out = outcome();
    out.metric("n_bars", closes.len());
    out.metric("total_close", total);

    // The property getter, for the small-scan case where reading a row directly is clearer.
    out.metric("first_close", bars[0].close);
    out.metric("first_ts", bars[0].ts);

    // `symbol` is an Option on a Bar and the store's codec erases it, so this row's is `()`. It is
    // recorded as a 0/1 flag rather than as a value, because a study must be able to TELL.
    out.metric("first_symbol_is_unit", if bars[0].symbol == () { 1 } else { 0 });

    // The run's own window is the default range of every read verb — the explicit arity is for a
    // study that genuinely needs a different one.
    out.metric("window_start", ctx.window().start);
    out.metric("window_end", ctx.window().end);
    let narrowed = ctx.bars(
        params.venue,
        params.symbol,
        params.interval,
        ts_range(0, params.narrow_end),
    );
    out.metric("n_narrowed", narrowed.len());

    let tsv = "ts\tclose\n";
    for b in bars {
        tsv += b.ts;
        tsv += "\t";
        tsv += b.close;
        tsv += "\n";
    }
    out.artifact("bars.tsv", tsv);

    return out;
}
