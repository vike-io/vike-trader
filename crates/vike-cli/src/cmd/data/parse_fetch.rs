//! `data hist fetch` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11). `parse` keeps the verb
//! and flag loop and every refusal the verbs share, and calls this once the verb is known; each
//! parameter below is a local `parse` held under the same name, so the body reads as it always did.

use super::{Source, Sub, Window, check_spec, refuse_a_window, refuse_foreign_flags, window_from};

/// `fetch`'s arm: the SOURCE decides the whole grammar. Returns the `(spec, window)` pair `parse`
/// builds its `Args` from.
#[allow(clippy::too_many_arguments)] // one parameter per local the arm read inside `parse`
pub(super) fn parse(
    sub: Sub,
    source: Option<Source>,
    spec: Option<String>,
    days: Option<String>,
    from: Option<String>,
    to: Option<String>,
    store: &Option<String>,
    engine: &Option<String>,
    addr: &Option<String>,
) -> Result<(Option<String>, Option<Window>), String> {
    Ok({
        // ⚠ ONE verb, and the SOURCE decides its whole grammar. A venue fetch takes a spec and
        // a window; `starter` and `demo` take NEITHER, because each is a fixed span this side
        // does not choose. Collapsing them lost nothing — the refusals that made them separate
        // subcommands are all still here, keyed on the axis instead of on a verb name.
        // The local is an Option so the guard below can tell 'flag absent' from 'flag
        // given'; the axis itself has no third state — an omitted --source IS a venue.
        let resolved = source.unwrap_or(Source::Venue);
        match resolved {
            Source::Venue => {
                let spec = spec.ok_or(
                    "fetch needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data hist \
                         fetch binance:BTCUSDT:1h --days 180` — or name a source that needs none \
                         (--source starter | demo)",
                )?;
                check_spec(&spec)?;
                // ⚠ The MIRROR of `repair`'s `--addr` refusal, and it arrived with the route:
                // a VENUE fetch has no engine route, so a flag naming one would be silently
                // ignored — the shape this module refuses everywhere else rather than tolerating.
                refuse_foreign_flags(
                    sub,
                    &[("--store", store.is_some()), ("--engine", engine.is_some())],
                    "those flags name a LOCAL store and the engine that opens it, and a VENUE \
                         fetch has no local route: history is fetched by the backend, once, into \
                         the store the datahub has open. Use --addr HOST:PORT (default \
                         127.0.0.1:7878) to say WHICH datahub — or --source starter|demo, which \
                         DO run locally and do take --store",
                )?;
                (Some(spec), Some(window_from(days, from, to)?))
            }
            // Every fetch-shaped flag is REFUSED here rather than ignored, for the reason both
            // share: each is a FIXED span. A `--days 30` that quietly did nothing would leave
            // an operator believing they had seeded or downloaded a month.
            Source::Starter | Source::Demo => {
                let (what, why) = if resolved == Source::Demo {
                    (
                        "--source demo writes the synthetic `demo` tape",
                        "the demo tape is a fixed synthetic span",
                    )
                } else {
                    (
                        "--source starter downloads the PUBLISHED dataset, whose series are fixed",
                        "the starter dataset is a fixed PUBLISHED span",
                    )
                };
                if spec.is_some() {
                    return Err(format!("that source takes no spec — {what}"));
                }
                refuse_a_window(sub, &days, &from, &to, why)?;
                // ⚠ The MIRROR of the `Source::Venue` arm's --store/--engine refusal above,
                // for the opposite reason: `--addr` is a VENUE fetch's route to a datahub, and
                // this source has no datahub in its path at all — the engine runs entirely on
                // THIS machine. A flag naming a remote datahub would be silently ignored,
                // which this module refuses everywhere else rather than tolerating.
                refuse_foreign_flags(
                    sub,
                    &[("--addr", addr.is_some())],
                    "that flag names a REMOTE datahub, and this source has no remote route: \
                         the engine runs on THIS machine and writes into a store named with \
                         --store. Drop --source to fetch a venue instead, which DOES take --addr",
                )?;
                (None, None)
            }
        }
    })
}
