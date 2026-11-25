//! `data realtime live-tokens [--family F] [--addr HOST:PORT]` — the live window of each rolling Polymarket family the
//! datahub's recorder records (`Request::LiveFamilyTokens`, a read answered from the recorder's memory: no venue is
//! called). Per family, in name order: the window with the earliest end still ahead, the rule the desktop's cockpit
//! follows (`vike-app-core`'s `cockpit_follow`), each outcome's token (`Up`, `Down`) and the window's end in UTC.
//!
//! Exit: 0 with the windows, or with words when none is held (an empty answer is an answer); 1 when the datahub
//! answers that it runs no recorder (its own sentence); 3 when it cannot be reached; 4 when it is older than the read,
//! with nothing sent.

use std::collections::BTreeMap;
use std::process::ExitCode;

use vike_datahub_client::family::LiveFamilyToken;
use vike_datahub_client::proto::FEATURE_LIVE_FAMILY_TOKENS;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_model::time::epoch_ms_to_utc_timestamp;

use super::grammar::address_help;
use super::{BIN, connect, require_feature};
use crate::exit::{CliError, CommandResult};

// As visible as `super::RealtimeVerb`, which carries it. (`//` rather than `///`: `clap` would print a doc comment
// on this struct as help.)
#[derive(clap::Args, Debug)]
pub(in crate::cmd::data) struct LiveTokensFlags {
    #[arg(
        long,
        value_name = "F",
        help = "only this family, as the recorder profile names it (btc-updown-5m)"
    )]
    family: Option<String>,
    #[arg(long, value_name = "HOST:PORT", help = address_help())]
    addr: Option<String>,
}

/// Run `data realtime live-tokens`.
pub(super) fn run(
    flags: &LiveTokensFlags,
    keys: Option<&HubKeys>,
    configured_address: Option<&str>,
) -> ExitCode {
    match execute(flags, keys, configured_address, vike_model::now_ms()) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

fn execute(
    flags: &LiveTokensFlags,
    keys: Option<&HubKeys>,
    configured_address: Option<&str>,
    now_ms: i64,
) -> CommandResult<Vec<String>> {
    let addr =
        vike_client::resolved::datahub_dial_address(flags.addr.as_deref(), configured_address);
    let mut client = connect(&addr, keys, Scope::Read)?;
    require_feature(&client, FEATURE_LIVE_FAMILY_TOKENS, "say which tokens its recorder follows")?;
    let tokens = client.live_family_tokens().map_err(CliError::failed)?;
    Ok(window_lines(&tokens, flags.family.as_deref(), now_ms))
}

/// Each family's live window (the earliest end still ahead of `now_ms`) with its outcomes' tokens; a family whose every
/// window has ended, and an answer that holds none of `family`, say so.
fn window_lines(tokens: &[LiveFamilyToken], family: Option<&str>, now_ms: i64) -> Vec<String> {
    let mut families: BTreeMap<&str, Vec<&LiveFamilyToken>> = BTreeMap::new();
    for token in tokens.iter().filter(|t| family.is_none_or(|f| t.family == f)) {
        families.entry(token.family.as_str()).or_default().push(token);
    }
    if families.is_empty() {
        let which = family.map_or_else(
            || "a rolling Polymarket family".to_string(),
            |f| format!("the family {f}"),
        );
        return vec![format!("the datahub's recorder holds no live window of {which} yet")];
    }
    let mut lines = Vec::new();
    for (name, held) in families {
        let Some(end) = held.iter().filter_map(|t| t.end_ms).filter(|end| *end > now_ms).min()
        else {
            lines.push(format!(
                "{name}: no live window (every window held has ended or states no end)"
            ));
            continue;
        };
        lines.push(format!("{name}: window ends {}", epoch_ms_to_utc_timestamp(end)));
        for token in held.iter().filter(|t| t.end_ms == Some(end)) {
            lines.push(format!("  {:<4}  {}", token.outcome, token.token));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;

    use vike_datahub_client::proto::{Request, Response};
    use vike_datahub_client::test_support::spawn_fake_peer;

    use super::*;
    use crate::exit::Exit;

    /// 2025-11-17T12:00:00Z.
    const NOW: i64 = 1_763_380_800_000;
    const MINUTE: i64 = 60_000;

    fn token(family: &str, token: &str, outcome: &str, end_ms: Option<i64>) -> LiveFamilyToken {
        LiveFamilyToken {
            family: family.to_string(),
            token: token.to_string(),
            outcome: outcome.to_string(),
            end_ms,
        }
    }

    fn flags(family: Option<&str>, addr: &str) -> LiveTokensFlags {
        LiveTokensFlags { family: family.map(str::to_string), addr: Some(addr.to_string()) }
    }

    /// What the recorder holds: an ended window, the live one and the next, for two families.
    fn held() -> Vec<LiveFamilyToken> {
        vec![
            token("btc-updown-5m", "11", "Up", Some(NOW - MINUTE)),
            token("btc-updown-5m", "12", "Down", Some(NOW - MINUTE)),
            token("btc-updown-5m", "21", "Up", Some(NOW + 4 * MINUTE)),
            token("btc-updown-5m", "22", "Down", Some(NOW + 4 * MINUTE)),
            token("btc-updown-5m", "31", "Up", Some(NOW + 9 * MINUTE)),
            token("btc-updown-5m", "32", "Down", Some(NOW + 9 * MINUTE)),
            token("eth-updown-15m", "41", "Up", Some(NOW - MINUTE)),
            token("eth-updown-15m", "42", "Down", None),
        ]
    }

    /// The live window is the earliest end still ahead: not the ended one, not the next one; a family with no window
    /// ahead says so, and `--family` narrows to one.
    #[test]
    fn each_family_prints_its_live_window_with_the_up_and_down_tokens() {
        assert_eq!(
            window_lines(&held(), None, NOW),
            [
                "btc-updown-5m: window ends 2025-11-17T12:04:00Z",
                "  Up    21",
                "  Down  22",
                "eth-updown-15m: no live window (every window held has ended or states no end)",
            ]
        );
        assert_eq!(window_lines(&held(), Some("btc-updown-5m"), NOW).len(), 3);
        assert_eq!(
            window_lines(&held(), Some("sol-updown-5m"), NOW),
            ["the datahub's recorder holds no live window of the family sol-updown-5m yet"]
        );
        assert_eq!(
            window_lines(&[], None, NOW),
            ["the datahub's recorder holds no live window of a rolling Polymarket family yet"]
        );
    }

    /// Against a datahub that serves the read: one `LiveFamilyTokens` request, the windows printed.
    #[test]
    fn the_verb_asks_the_datahub_once_and_prints_what_it_holds() {
        let (addr, received, _) =
            spawn_fake_peer(vec![FEATURE_LIVE_FAMILY_TOKENS.to_string()], |r| {
                matches!(r, Request::LiveFamilyTokens).then(|| Response::LiveFamilyTokens(held()))
            });
        let lines = execute(&flags(Some("btc-updown-5m"), &addr.to_string()), None, None, NOW)
            .expect("answered");
        assert_eq!(lines[1], "  Up    21", "{lines:?}");
        assert_eq!(received.recv().expect("the fake reports").len(), 1);
    }

    /// An older datahub is refused in the desktop's words, naming the CLI, with nothing sent after the handshake.
    #[test]
    fn an_older_datahub_is_refused_and_sent_nothing() {
        let (addr, received, _) = spawn_fake_peer(vec!["load_bars".to_string()], |_| None);
        let err = execute(&flags(None, &addr.to_string()), None, None, NOW).expect_err("older");
        assert_eq!(err.exit, Exit::Refused);
        assert!(err.message.starts_with("this datahub is older than the CLI"), "{}", err.message);
        assert!(err.message.contains(FEATURE_LIVE_FAMILY_TOKENS), "{}", err.message);
        assert!(received.recv().expect("the fake reports").is_empty(), "nothing was sent");
    }

    /// A REAL datahub that runs no recorder answers in its own words, and the verb fails on them; an unreachable one is
    /// the connect rung.
    #[test]
    fn a_real_datahub_without_a_recorder_says_so() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("addr").to_string();
        let store: Arc<dyn vike_data::HistoryStore + Send + Sync> =
            Arc::new(vike_data::HistoryStoreMemory::new());
        std::thread::spawn(move || {
            let _ = vike_datahub::serve(listener, store);
        });
        let err = execute(&flags(None, &addr), None, None, NOW).expect_err("no recorder");
        assert_eq!(err.exit, Exit::Failed);
        assert!(err.message.contains("runs no recorder"), "{}", err.message);

        let err =
            execute(&flags(None, "127.0.0.1:1"), None, None, NOW).expect_err("nothing listens");
        assert_eq!(err.exit, Exit::Connect);
    }
}
