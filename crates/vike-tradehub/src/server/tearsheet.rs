//! `server::tearsheet` — the LIVE-JOURNAL report verb's renderer (ruling 16): the body of the
//! `Request::Tearsheet` arm, split out so the whole reply is decidable without a socket.
//!
//! Split out of `server.rs` as a pure move; the module doc there carries the verb's contract
//! (post-auth under either scope, executes nothing, advertised as `FEATURE_TEARSHEET`).

use vike_tradehub_client::proto::Response;

use super::settings::SettingsShowSource;

/// Render the LIVE-JOURNAL tearsheet this node's own journal answers (ruling 16) — the body of the
/// [`vike_tradehub_client::proto::Request::Tearsheet`] arm, split out so the whole reply is
/// decidable without a socket.
///
/// `seed` / `periods_per_year` are the wire's optionals; `None` means *the caller has no opinion*
/// and resolves to the renderer's own default rather than to a number invented here
/// ([`TEARSHEET_DEFAULT_SEED`] and `vike_analytics::report::DAILY_PERIODS_PER_YEAR`), so a remote
/// `report` and `vike-report`'s own `tearsheet --journal DIR` over one journal print the same
/// document. The payload is `serde_json::to_string_pretty`, which is the spelling
/// `tearsheet --json` uses: `vike_tradehub_client::tearsheet` hands this text to its
/// caller VERBATIM, so a compact spelling here would make the remote door's bytes differ from the
/// local door's for one set of fills — the exact drift [`Response::Tearsheet`]'s own doc carries
/// the JSON (rather than a typed payload) to avoid.
///
/// # WHICH journal — and the rung this build cannot see
///
/// The directory is resolved by `vike_core::journal_config_from` over
/// [`SettingsShowSource::env`], the daemon's OWN startup `std::env::vars()` sweep. That is the same
/// pure resolver, over the same three variables, that the live core's `CoreConfig::journal` was
/// built from, so this reply cannot name a journal the core is not writing.
///
/// ⚠ **It sees `VIKE_RUN_PROFILE` and `VIKE_JOURNAL_DIR`; it does NOT see `config.journal_dir`.**
/// `crates/vike-tradehub/src/tradehub_cli/flags.rs`'s `journal_vars` folds that settings key into the map
/// it hands the core (env winning where both are present), and the folded map is not threaded into
/// the `server` module. So on a daemon journalling purely through `config.journal_dir` this arm answers an ERROR,
/// and the error SAYS SO BY NAME rather than reporting "journaling is off" — a node that is
/// journalling must never be described as one that is not, and the operator who reads this needs to
/// know which of the two facts they are holding. Closing it is one edit and no new parameter: make
/// that function `pub(crate)` and call it here instead of reading `env` directly. It is written as a
/// residual rather than done because `journal_vars`' precedence has exactly one home and a second
/// copy of *"the environment wins"* in this file would be the drift, not the fix.
pub(super) fn tearsheet_reply(
    settings: Option<&SettingsShowSource>,
    seed: Option<f64>,
    periods_per_year: Option<f64>,
) -> Response {
    // A server constructed without a settings source (possible through `serve(.., None)`; the
    // shipped daemon always passes one) knows nothing about its own environment — the same honest
    // refusal the identity-less `StrategyStatus` and source-less `SettingsShow` arms make, never a
    // fabricated empty tearsheet.
    let Some(src) = settings else {
        return Response::Error(
            "tearsheet unavailable: this node was started without a settings source, so it cannot \
             resolve its own journal directory"
                .into(),
        );
    };
    let Some(journal) = vike_core::journal_config_from(&src.env) else {
        return Response::Error(
            "tearsheet unavailable: this node's environment enables no command journal, so there \
             are no fills to fold (set VIKE_JOURNAL_DIR, or a VIKE_RUN_PROFILE with a \
             [sinks.journal] section, and restart). ⚠ If journaling is configured through \
             the `config.journal_dir` setting instead, THIS BUILD CANNOT SEE IT — the node may well be \
             journalling; run `vike-report`'s `tearsheet --journal <dir>` on the daemon's own box \
             to read it."
                .into(),
        );
    };
    let seed = seed.unwrap_or(TEARSHEET_DEFAULT_SEED);
    let ppy = periods_per_year.unwrap_or(vike_analytics::report::DAILY_PERIODS_PER_YEAR);
    // Read-only and off the fold: `tearsheet_from_journal` opens the journal's own segment files
    // and folds them through the shared reconstruction on THIS connection thread. Nothing touches
    // the core, the publisher or the snapshot cell — the module's hot-path guarantee is intact —
    // and a large journal costs this one peer's thread, never another client's.
    match vike_report::tearsheet_from_journal(&journal.dir, seed, ppy) {
        Ok(sheet) => match serde_json::to_string_pretty(&sheet) {
            Ok(json) => Response::Tearsheet(json),
            // Effectively unreachable for this type — every field is a scalar and serde_json
            // writes a non-finite float as `null` rather than failing — but it is an arm rather
            // than an `unwrap` because a panic here kills the connection thread that is holding a
            // MAX_CONNECTIONS slot, and "the report could not be serialized" is a sentence.
            Err(e) => Response::Error(format!("tearsheet could not be serialized: {e}")),
        },
        Err(e) => Response::Error(format!(
            "tearsheet unavailable: the journal at {} could not be read ({e})",
            journal.dir.display()
        )),
    }
}

/// The starting capital a [`vike_tradehub_client::proto::Request::Tearsheet`] carrying no `seed`
/// is folded with.
///
/// It is the SAME number `vike-report`'s `tearsheet --seed` defaults to, and it is spelled here
/// because that default is a `parse_num` argument inside a CLI body rather than a named constant
/// anything can import. ⚠ **If the two ever disagree, the remote and local doors publish different
/// `total_return`/`cagr`/`sharpe` for one journal** — every one of those scales with the equity
/// base — and nothing would say so, because both answers are internally consistent. The fix when
/// that day comes is to name the constant in `vike_report::tearsheet_cli` and delete this one, not
/// to edit this number.
pub(super) const TEARSHEET_DEFAULT_SEED: f64 = 10_000.0;
