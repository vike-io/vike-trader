// `fixture_fit` — the fixture that pins the LEARNER half of the interpreted contract, both ways.
//
// The deliberate twin of the compiled tier's own `fixture_fit` (under vike-user-research's
// `tests/fixture_user_data/`): same three-row matrix, same two artifacts, same refusal. What it
// proves that the Rust one cannot is that the NARROWED fitting surface is enough — a script cannot
// construct a `TrainData` (it is borrowed) or a `GridPoint` (it is a five-field struct), so
// `ctx.fit` takes the two flat arrays and a map and builds both inside the host.
//
// With a learner it fits, predicts and exports both artifacts. With none it calls `ctx.fit` anyway
// and STOPS — the raised failure comes back to the caller as `StudyError::NoLearner`, the
// documented ceiling of a host with no LightGBM binary, rather than as a number computed without a
// model. `ctx.can_fit()` is how a study that would rather branch asks first.

// Three rows of two features, plus their labels. A fixture, not a dataset: what is under test is
// that a fit REACHES the learner and its artifacts come back, not what the model learned.
const X = [0.0, 1.0, 1.0, 0.5, 2.0, 0.25];
const Y = [0.0, 1.0, 1.0];
const N_COLS = 2;

fn run(ctx, params) {
    let out = outcome();
    out.metric("can_fit", if ctx.can_fit() { 1 } else { 0 });

    // Every key is checked by name: a typo here is a refusal, never a knob that looks armed and is
    // not. The five hyperparameter axes are LightGBM's own names; unset ones keep the default
    // point.
    let fit = ctx.fit(X, Y, N_COLS, #{
        seed: params.seed,
        capture_importance: true,
        capture_text: true,
    });

    out.metric("rows", Y.len());
    out.metric("p_first_row", fit.predict([0.0, 1.0]));

    // A learner that cannot report an artifact answers none even when asked, and the two absences
    // are not symmetric — an EMPTY importance table would read as "no feature mattered". So both
    // accessors RAISE rather than answering empty, and a study asks first.
    if fit.has_text() {
        out.artifact("model.txt", fit.text());
    }
    if fit.has_importance() {
        let imp = fit.importance();
        let tsv = "feature\tgain\tsplits\n";
        for r in imp {
            tsv += "f";
            tsv += r.feature;
            tsv += "\t";
            tsv += r.gain;
            tsv += "\t";
            tsv += r.splits;
            tsv += "\n";
        }
        out.artifact("importance.tsv", tsv);
        out.metric("importance_features", imp.len());
    }

    return out;
}
