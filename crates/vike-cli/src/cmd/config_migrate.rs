//! `vike-cli config migrate-store` — apply the settings store's pending migrations (today: decision
//! 0095's `live`-means-mainnet ceiling rewrite) and say what they did.
//!
//! It is the command a daemon's boot refusal names when the daemon cannot write `settings/db`
//! (`vike_boot::Ceilings::Interpret`). This process's own boot will usually have applied it already
//! — `vike-cli` boots with `Ceilings::InterpretOrMark` — in which case its rewrite lines were
//! printed to stderr at startup and this verb reports the store as current. Journalled with
//! `Actor::cli("vike-cli")`.

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::live_means_mainnet::{LiveMeansMainnet, REASON};

use crate::exit::{CliError, CmdResult, Exit};

const USAGE: &str = "usage: vike-cli config migrate-store\n\n  Applies the settings store's pending \
     migrations (decision 0095: `live` ceilings of binance/bybit/okx/hyperliquid become `demo`), \
     journalled. Run it as the user that owns <project>/settings/db.";

/// What this verb needs from the dispatcher's one boot.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub settings_dir: Option<&'a Path>,
    pub now_ms: i64,
}

/// Entry point. `args` is everything after `config migrate-store`.
pub fn run(mut args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    if let Some(a) = args.next() {
        if matches!(a.as_str(), "-h" | "--help") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        eprintln!("vike-cli config migrate-store: unexpected argument `{a}`\n{USAGE}");
        return Exit::Usage.into();
    }
    match execute(&ctx) {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vike-cli config migrate-store: {}", e.msg);
            e.exit.into()
        }
    }
}

fn execute(ctx: &Ctx<'_>) -> CmdResult<Vec<String>> {
    let Some(dir) = ctx.settings_dir else {
        return Err(CliError::failed(
            "no settings directory resolved — `cd` into the project, or name it with \
             $VIKE_SETTINGS_DIR"
                .to_string(),
        ));
    };
    let outcome = vike_secrets::live_means_mainnet::apply_live_means_mainnet(
        dir,
        vike_model::change_journal::Actor::cli("vike-cli"),
        vike_model::change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
        ctx.now_ms,
    )
    .map_err(|e| {
        CliError::failed(format!(
            "the store could not be migrated: {e} — run this as the user that owns {}",
            vike_secrets::db_path_in(dir).display()
        ))
    })?;
    Ok(report_lines(&outcome))
}

/// PURE, so the wording is unit-tested.
fn report_lines(outcome: &LiveMeansMainnet) -> Vec<String> {
    match outcome {
        LiveMeansMainnet::NoDatabase => {
            vec!["no settings database on this box — nothing to migrate".to_string()]
        }
        LiveMeansMainnet::NotPending => vec![
            "the settings store is current: decision 0095's ceiling migration has nothing left to do"
                .to_string(),
        ],
        LiveMeansMainnet::Applied { rewrites, journal_error } => {
            let mut out = vec![format!(
                "decision 0095 applied: {} ceiling row(s) rewritten from `live` to `demo`",
                rewrites.len()
            )];
            out.extend(rewrites.iter().map(|r| format!("  {}: live -> demo", r.key())));
            out.push(format!("  why: {REASON}"));
            if let Some(e) = journal_error {
                out.push(format!("  ⚠ the change journal could not record it: {e}"));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::live_means_mainnet::CeilingRewrite;

    #[test]
    fn an_applied_migration_names_every_row_and_the_reason() {
        let lines = report_lines(&LiveMeansMainnet::Applied {
            rewrites: vec![CeilingRewrite { venue: "hyperliquid".to_string(), label: None }],
            journal_error: None,
        });
        let text = lines.join("\n");
        assert!(text.contains("policy.venues.hyperliquid: live -> demo"), "{text}");
        assert!(text.contains("0095"), "{text}");
    }

    #[test]
    fn a_current_store_says_so() {
        assert!(report_lines(&LiveMeansMainnet::NotPending)[0].contains("current"));
    }
}
