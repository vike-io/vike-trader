use super::*;
use serde_json::json;

#[cfg(test)]
mod ids;
#[cfg(test)]
mod manifest;
#[cfg(test)]
mod meta_marks;
#[cfg(test)]
mod reserved_roster;
#[cfg(test)]
mod series;

fn a_manifest(run_id: &str) -> RunManifest {
    RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: run_id.to_string(),
        kind: BACKTEST_RUN_KIND.to_string(),
        produced_by: "backtest".to_string(),
        started_at: utc_rfc3339(1_756_000_000),
        finished_at: utc_rfc3339(1_756_000_012),
        git_sha: None,
        fingerprint: None,
        config: RunConfig {
            path: Some("profiles/sma.toml".to_string()),
            name: Some("sma cross".to_string()),
        },
        detail: json!({ "strategy": "sma_cross" }),
    }
}

const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
