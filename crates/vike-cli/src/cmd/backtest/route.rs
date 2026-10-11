//! Which wire verb carries a resolved profile, and the flags the walk-forward route refuses.

use super::Args;

/// Whether `profile_toml` carries the non-empty `[sweep]` table a parameter search needs — `None`
/// when this side cannot tell and the engine's (or the server's) own parser must answer.
///
/// Mirrors `vike_backtest::harness::profile::BacktestProfile::is_paramscan` (present AND non-empty),
/// which is the predicate BOTH the engine and the compute server branch on. It is a PRESENCE check
/// over one key, not a profile parser: nothing here validates a slice, a strategy or a window.
///
/// ⚠ **It ROUTES the remote arm, and that closes a live divergence between the two modes.** The
/// engine has always branched on this predicate — `backtest sweep.toml` runs `harness::run_paramscan`
/// — so `--local` on a grid profile has always run the grid. The remote arm called
/// `DatahubClient::run_backtest` unconditionally, and the server's `run_backtest` runs
/// `harness::run_backtest`, which IGNORES the `[sweep]` table and reports ONE point. Same profile,
/// same verb, same flags: a grid on this machine and a single backtest on the server, with nothing
/// in the output saying which had happened. Routing here on the same predicate both far sides use
/// is what makes `--local` a rehearsal for `--addr` on a search, which is what this module's doc
/// already promises for `--preset` and `--script`.
///
/// ⚠ It runs on the REWRITTEN text — after `--preset` and `--script` are merged — so a profile
/// whose grid arrives through a preset routes the same way its contents say it should.
///
/// ⚠ **It reads TWO section names, and BOTH are live — permanently.** Ruling 2 renamed the profile
/// section `[sweep]` → `[paramscan]`, and stage 7 landed the parser rename as a serde ALIAS rather
/// than a replacement: `vike_backtest::harness::BacktestProfile`'s field is `paramscan` with
/// `#[serde(alias = "sweep")]`, so every profile already on an operator's disk keeps loading
/// FOREVER. A pre-parse that knew only one name would route the other spelling to `RunBacktest`,
/// which reports ONE point and never mentions the grid: this function's own divergence, re-opened
/// from the other side. The new name is read FIRST; a profile carrying BOTH is a serde
/// duplicate-field error on the far side, which is the right answer and not this side's to produce.
///
/// ⚠ **This paragraph used to say the opposite, and the reversal is the point of stage 7.** It read:
/// *"Accepting `paramscan` HERE costs nothing today and is not an advertisement: no shipped profile
/// carries it, `BacktestProfile` is `deny_unknown_fields` and declares only `sweep` … That is why
/// the USAGE text and every operator-facing message in this crate still say `[sweep]`."* That was
/// true and is not: the engine declares `paramscan` now, so naming it is no longer positive
/// confirmation of something false, and every operator-facing string in this crate says
/// `[paramscan]`.
///
/// ⚠ The IDENTIFIER moved with the section — stage 7's rename task renamed this function from
/// `declares_a_sweep_grid`, and `crates/vike-cli/CLAUDE.md` and
/// `crates/vike-cli/tests/search_walkforward_cli.rs`, which both cite it by name, moved with it.
/// What a rename may NOT touch is anything a PEER compares literally: the wire tags (pinned by
/// `#[serde(rename = "RunSweep")]` and its siblings), the `"sweep"` field key, the capability
/// strings `run_sweep_profile`/`run_sweep`, the MCP tool name `run_sweep`, and the `[sweep]` alias
/// this function reads above. A doc sentence claiming an identifier did NOT move is worth nothing
/// as a guard — the pass that moves the identifier rewrites the sentence too, which is exactly what
/// happened to the paragraph that used to sit here.
pub(super) fn declares_a_paramscan_grid(profile_toml: &str) -> Option<bool> {
    let doc: toml::Value = toml::from_str(profile_toml).ok()?;
    match doc.get("paramscan").or_else(|| doc.get("sweep")) {
        None => Some(false),
        Some(toml::Value::Table(t)) => Some(!t.is_empty()),
        // Present and the wrong SHAPE. The engine refuses it with a type error naming the field,
        // which is a better message than anything this side could produce.
        Some(_) => None,
    }
}

/// Which wire verb carries the run. See [`route_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// `[walkforward]` present — the out-of-sample walk, over `RunWalkforwardProfile`. It carries
    /// a grid-bearing profile too: the window is a MODIFIER, not an alternative to searching.
    Walkforward,
    /// A non-empty grid section and no window — the parameter search, over `RunParamscanProfile`.
    Search,
    /// Everything else — one backtest, over `RunBacktest`.
    Single,
}

/// Which WIRE VERB carries this resolved profile — ruling 7's two axes, as one function:
///
/// ```text
///                        WHAT runs
///                        one backtest            a parameter search
///                                                (the grid section, non-empty)
/// HOW validated
///   one slice            RunBacktest             RunParamscanProfile
///   walked forward       RunWalkforwardProfile   RunWalkforwardProfile
///   ([walkforward])
/// ```
///
/// ⚠ **A walk-forward is a MODIFIER, not a third kind of run** — ruling 7, and it is why this
/// function is a two-axis map rather than §5.1's "window beats axis beats single". The two axes do
/// not compete: the grid says WHAT is computed and `[walkforward]` says HOW it is VALIDATED,
/// and a profile declaring both COMPOSES — each window re-searches the grid on its own training
/// half. `RunWalkforwardProfile` carries both walked-forward cells because the profile TOML crosses
/// the wire WHOLE and the far side's one parser reads the grid for itself; this side picks the
/// carrier and makes no claim about what runs inside it.
///
/// ⚠ **There is deliberately NO refusal here** — ruling 5. An earlier draft refused the pair unless
/// `[walkforward]` said `search = "sweep"`; ruling 4 deleted that key (the presence of the two
/// sections IS the statement) and ruling 5 made composition the answer. Do not re-introduce a
/// client-side refusal: this side sends the profile whole and guesses at nothing, which is the
/// property that makes `--local` a byte-identical rehearsal of `--addr`.
///
/// ⚠ **Declared residual, and it is a FAR-SIDE one.** Until ruling 4's deletion lands (the
/// walk-forward stage owns it, with `WalkforwardCfg`'s growth), an absent `walkforward.search`
/// still resolves to `WindowSearch::None`, whose driver arm trades `[strategy.params]` and never
/// reads the grid — so a composed profile routes correctly from here and runs UNSEARCHED there.
/// Nothing on this side can fix that without guessing, and guessing is what was withdrawn.
///
/// ⚠ **The two presence tests are DIFFERENT and the code is what decides.** A grid section
/// present-and-EMPTY is no search (`BacktestProfile::is_paramscan` is present-AND-non-empty, and
/// [`declares_a_paramscan_grid`] mirrors it). `[walkforward]` present-and-empty routes to the window
/// anyway, because `WalkforwardCfg::n_splits` carries no `#[serde(default)]` — so the far side's
/// parser answers with the error naming the missing key, which is a better message than anything
/// this side could produce. A key of the wrong SHAPE, and text this side cannot parse at all, both
/// go to the plain backtest for the same reason.
///
/// ⚠ It runs on the REWRITTEN text — after `--preset` and `--script` are merged — so a profile
/// routes on what it actually says.
pub(super) fn route_of(profile_toml: &str, search_requested: bool) -> Route {
    let walked = match toml::from_str::<toml::Value>(profile_toml) {
        Ok(doc) => matches!(doc.get("walkforward"), Some(toml::Value::Table(_))),
        // Text this side cannot parse at all: the plain backtest, whose
        // `BacktestProfile::from_toml_str` produces the parse error naming the line.
        Err(_) => false,
    };
    // ⚠ **OR A WRITTEN SEARCH KNOB, and that second term is what closes the hole stage 7 would
    // otherwise have opened.** `--optimizer tpe` on a profile with NO grid used to route to
    // `RunBacktest`, whose server arm IGNORES a method — the same silent-downgrade class, one verb
    // over. It routes to the search verb now, where the server.s own "profile has no [paramscan]
    // table" refusal answers on the same rung the local engine.s pre-flight does (`vike_backtest`.s
    // `SearchFlags::requested && !profile.is_paramscan()`).
    //
    // ⚠ TWO TERMS, never a third BRANCH. Ruling R7 settles that walk-forward is a MODIFIER over a
    // run rather than a third run kind, so `--optimizer tpe` on a profile that ALSO declares
    // `[walkforward]` means *walk forward, searching inside each window* — and the walk-forward
    // arm is tested FIRST, below, so it cannot flatten into one search over the whole range.
    let searching = declares_a_paramscan_grid(profile_toml) == Some(true) || search_requested;
    match (walked, searching) {
        (true, _) => Route::Walkforward,
        (false, true) => Route::Search,
        (false, false) => Route::Single,
    }
}

/// The flags the WALK-FORWARD route cannot carry, refused BY NAME rather than dropped.
///
/// `DatahubClient::run_walkforward_profile` takes the profile TOML and NOTHING else — no ranking
/// field, no method selector — so a `--rank-by` or an `--optimizer` on this route configures
/// nothing at all. A window's own ranking is `[walkforward].rank_by`, in the profile, where the one
/// parser reads it.
///
/// ⚠ This is NOT the `--rank-by`-is-ignored-on-a-gridless-profile case. That one is the ENGINE's
/// documented behaviour on a route that genuinely carries the flag, and this module deliberately
/// does not second-guess it. Here the flag is dropped by THIS side, which is the thing this
/// module's doc says not to do: *"silently dropping either is how somebody comes to believe a run
/// used a store, or a host, that it never touched."*
///
/// ⚠ The message names the `[walkforward]` TABLE and no key inside it beyond `rank_by`, and that
/// is deliberate. Ruling 4 deletes `walkforward.search` — whether a window re-searches becomes the
/// grid's presence — but that deletion has NOT landed:
/// `vike_backtest::harness::profile::BacktestProfile::window_search` still reads the key, so a
/// message asserting the post-ruling rule would be positive confirmation of something false, and
/// one NAMING the key would go stale the day it is deleted. The table is true in both worlds.
pub(super) fn refuse_a_walkforward_flag(args: &Args) -> Result<(), String> {
    for (flag, present) in
        [("--rank-by", args.rank_by.is_some()), ("--optimizer", args.optimizer.is_some())]
    {
        if present {
            return Err(format!(
                "{flag} configures nothing on a walk-forward run: the wire verb it goes over \
                 carries the profile TOML and nothing else. A window's ranking is \
                 [walkforward].rank_by, in the profile, read by the one parser that runs it — and \
                 what each window searches is that same [walkforward] table's business, never a \
                 flag on this side."
            ));
        }
    }
    Ok(())
}

/// `--local` cannot run a walk-forward, and the reason is a property of the ENGINE rather than of
/// this verb: `crates/vike-backtest/src/backtest_cli.rs` has one profile path, branching on
/// `BacktestProfile::is_paramscan` and nothing else, so NEITHER driver
/// (`vike_backtest::harness::run_walkforward` or its optimizing sibling) is reachable from any
/// binary. There is nothing to spawn. `crates/vike-cli/src/cmd/walkforward.rs`'s module doc
/// carries the condition that would change it.
pub(super) const WALKFORWARD_HAS_NO_LOCAL_ARM: &str = "--local cannot run a walk-forward: this profile declares a [walkforward] table, and the \
     standalone engine has ONE profile path — it branches on the [paramscan] grid and nothing else, so \
     neither walk-forward driver is reachable from any binary and there is nothing local to \
     spawn.\n\
     Drop --local to run it on the compute daemon, or drop the [walkforward] table to backtest \
     this profile here.";
