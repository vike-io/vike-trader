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
/// `report` and the `tearsheet --journal DIR` tool over one journal print the same
/// document. The payload is `serde_json::to_string_pretty`, which is the spelling
/// `tearsheet --json` uses: `vike_tradehub_client::tearsheet` hands this text to its
/// caller VERBATIM, so a compact spelling here would make the remote door's bytes differ from the
/// local door's for one set of fills — the exact drift [`Response::Tearsheet`]'s own doc carries
/// the JSON (rather than a typed payload) to avoid.
///
/// # WHICH journal
///
/// [`SettingsShowSource::journal`]: the ONE journal the daemon resolved at boot
/// (`crate::profile_rows::journal_config_for` over the `config.journal_dir` row and the active
/// `run` row's tuning) and handed to its paper core, so this reply cannot name a journal the
/// core is not writing.
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
    let Some(journal) = src.journal.as_ref() else {
        return Response::Error(
            "tearsheet unavailable: this node runs with no command journal, so there are no \
             fills to fold. Journaling is on exactly when a `config.journal_dir` row is set \
             (`vike-cli config set config.journal_dir <dir>`); restart the daemon after setting \
             it."
            .into(),
        );
    };
    let seed = seed.unwrap_or(TEARSHEET_DEFAULT_SEED);
    let ppy = periods_per_year.unwrap_or(vike_analytics::report::DAILY_PERIODS_PER_YEAR);
    // Read-only and off the fold: `fills_from_journal` opens the journal's own segment files and
    // `tearsheet_from_fills` folds them (as held, in journal order) through the shared
    // reconstruction on THIS connection thread. Nothing touches the core, the publisher or the
    // snapshot cell — the module's hot-path guarantee is intact — and a large journal costs this
    // one peer's thread, never another client's.
    match vike_journal::fills_from_journal(&journal.dir) {
        Ok(fills) => match serde_json::to_string_pretty(
            &vike_analytics::tearsheet_from_fills(
                fills,
                vike_analytics::FillOrder::AsHeld,
                seed,
                ppy,
            )
            .0,
        ) {
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
/// It is the SAME number the `tearsheet --seed` flag (`crates/vike/src/tearsheet_cli.rs`) defaults
/// to, and it is spelled here because that default is a `parse_num` argument inside a CLI body
/// rather than a named constant anything can import. ⚠ **If the two ever disagree, the remote and
/// local doors publish different `total_return`/`cagr`/`sharpe` for one journal** — every one of
/// those scales with the equity base — and nothing would say so, because both answers are
/// internally consistent. The fix when that day comes is ONE named constant in a crate both can
/// import (`vike-analytics`, beside `report::DAILY_PERIODS_PER_YEAR`; the CLI sits in
/// `crates/vike` now, above this crate, so it can no longer be that home) and this one deleted,
/// not an edit of this number.
pub(super) const TEARSHEET_DEFAULT_SEED: f64 = 10_000.0;
