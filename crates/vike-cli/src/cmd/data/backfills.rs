//! `data hist running` and `data hist cancel` — the operator's door onto the backfills a datahub is
//! RUNNING, rendered. `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md`
//! §4 is the design; `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`
//! decides who may send each verb.
//!
//! # What the two verbs are FOR
//!
//! A `fetch` whose client goes away stops on its own — the datahub peeks the connection between
//! chunks and stops at the next boundary. What that cannot see is a `fetch` the operator cannot
//! reach: one left running in another session or on another machine, or a client whose socket is
//! half-open (the laptop lost power behind an SSH tunnel). Before these verbs the only stop for
//! those was restarting the data daemon, which takes the recorder down with it. `running` shows what
//! is fetching; `cancel VENUE:SYMBOL:INTERVAL` — the spec the operator gave `fetch` — raises the stop
//! flag of every running request on that series.
//!
//! # What a cancel does, and does NOT do
//!
//! - **It stops at the next CHUNK BOUNDARY.** The chunk in flight finishes and is stored; nothing
//!   partial is written; every chunk before it stays stored; repeating the same `fetch` resumes after
//!   it. That is why there is NO typed confirmation, unlike `rm`: nothing is removed.
//! - **It does not wait.** The answer comes back at once naming what was flagged; the flagged request
//!   is still listed by `running` until it reaches its boundary, and its OWN `fetch` is then answered
//!   with an error naming the cancel.
//! - **A ONE-BATCH lane cannot be stopped** — the keyless kline venues and the funding lane fetch a
//!   whole window in one request and commit it once, so there is no boundary to stop at. Such a
//!   request is listed, and a cancel names it as unstoppable with
//!   `vike_datahub_client::proto::BACKFILL_ONE_BATCH` rather than pretending.
//!
//! # Why the renderers live here
//!
//! For `crate::cmd::data`'s `get` module's reason: every line below is a pure function over the
//! wire's own rows and an address, so the wording is tested without a socket, and the two verbs'
//! `execute_*` functions in `crate::cmd::data` are a dial, one round trip and a print.

use std::time::Duration;

use vike_datahub_client::proto::{BACKFILL_ONE_BATCH, BackfillCancelDone, RunningBackfill};
use vike_model::epoch_ms_to_utc_date;

use super::fetch_split::elapsed;

/// The spec a running request was asked for — `VENUE:SYMBOL:INTERVAL`, the operator's own spelling,
/// so a `running` row can be pasted straight into `cancel`.
pub(super) fn spec_of(row: &RunningBackfill) -> String {
    format!("{}:{}:{}", row.venue, row.symbol, row.interval)
}

/// What a cancel can do to `row`, in words — the `STOP` cell and the tail of a cancel's line.
fn stop_cell(row: &RunningBackfill) -> &'static str {
    match (row.stoppable, row.cancelled) {
        (true, false) => "at its next chunk boundary",
        (true, true) => "CANCEL RAISED — stops at its next chunk boundary",
        (false, _) => BACKFILL_ONE_BATCH,
    }
}

/// One `running` row's summary cells, in column order.
fn cells(row: &RunningBackfill) -> [String; 7] {
    [
        format!("#{}", row.id),
        spec_of(row),
        format!("{} .. {}", epoch_ms_to_utc_date(row.start), epoch_ms_to_utc_date(row.end)),
        elapsed(Duration::from_millis(row.elapsed_ms)),
        row.boundaries.to_string(),
        row.peer.clone().unwrap_or_else(|| "-".to_string()),
        stop_cell(row).to_string(),
    ]
}

/// The human `running` answer: one aligned row per request, in the order the datahub began them,
/// and a closing line saying how to stop one. An empty registry says so in a sentence rather than
/// printing a header over nothing.
///
/// ⚠ `BOUNDARIES` is the datahub's count of chunk boundaries the request has reached — chunks
/// BEGUN, the design's free progress figure — and NOT a fraction: the server does not know a lane's
/// chunk count in advance. A one-batch lane never reaches one, so it reads `0` there for its whole
/// life; its `STOP` cell says why.
pub(super) fn running_lines(rows: &[RunningBackfill], addr: &str) -> Vec<String> {
    if rows.is_empty() {
        return vec![format!("no backfill is running on the datahub at {addr}")];
    }
    const HEAD: [&str; 7] = ["ID", "SERIES", "WINDOW", "RUNNING", "BOUNDARIES", "PEER", "STOP"];
    let table: Vec<[String; 7]> = rows.iter().map(cells).collect();
    let widths: Vec<usize> = (0..HEAD.len())
        .map(|i| table.iter().map(|r| r[i].chars().count()).max().unwrap_or(0).max(HEAD[i].len()))
        .collect();
    let line = |cells: &[&str]| -> String {
        let mut out = String::from("  ");
        for (i, cell) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                out.push_str(cell);
            } else {
                out.push_str(&format!("{cell:<w$}  ", w = widths[i]));
            }
        }
        out.trim_end().to_string()
    };
    let noun = if rows.len() == 1 { "backfill" } else { "backfills" };
    let mut out = vec![format!("{} {noun} running on the datahub at {addr}:", rows.len())];
    out.push(line(&HEAD));
    for row in &table {
        out.push(line(&row.each_ref().map(String::as_str)));
    }
    out.push(format!(
        "stop one with `vike-cli data hist cancel SERIES --addr {addr}` — it stops at its next \
         chunk boundary, keeps every chunk stored before it, and repeating its fetch resumes there"
    ));
    out
}

/// One row as both documents carry it: every field the datahub sent, plus the `spec` a caller pastes
/// into `cancel`. Nothing is renamed, so a consumer reads the wire's own words.
fn row_json(row: &RunningBackfill) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "spec": spec_of(row),
        "venue": row.venue,
        "symbol": row.symbol,
        "interval": row.interval,
        "start": row.start,
        "end": row.end,
        "peer": row.peer,
        "started_ms": row.started_ms,
        "elapsed_ms": row.elapsed_ms,
        "lane": row.lane,
        "stoppable": row.stoppable,
        "cancelled": row.cancelled,
        "boundaries": row.boundaries,
    })
}

/// The `running --json` document: the datahub that answered and every running request. An empty
/// `running` array is the answer "nothing is running", never a failure.
pub(super) fn running_json(rows: &[RunningBackfill], addr: &str) -> serde_json::Value {
    serde_json::json!({
        "addr": addr,
        "running": rows.iter().map(row_json).collect::<Vec<_>>(),
    })
}

/// The human `cancel` answer: one line per request the cancel FLAGGED, one per request on the
/// series it could NOT stop, and — when neither — a sentence saying nothing was running on that
/// series, which is a success: the cancel is idempotent and asking twice is not a mistake.
pub(super) fn cancel_lines(done: &BackfillCancelDone, spec: &str, addr: &str) -> Vec<String> {
    if done.flagged.is_empty() && done.unstoppable.is_empty() {
        return vec![format!(
            "nothing is running on {spec} at the datahub at {addr} — nothing to cancel. \
             `vike-cli data hist running --addr {addr}` lists what is"
        )];
    }
    let mut out = Vec::new();
    for row in &done.flagged {
        out.push(format!(
            "cancel raised on #{} {} [{} .. {}], {} boundaries reached: it stops at its next chunk \
             boundary and its own fetch is answered with the cancel. Every chunk before the \
             boundary stays stored, and repeating that fetch resumes there",
            row.id,
            spec_of(row),
            epoch_ms_to_utc_date(row.start),
            epoch_ms_to_utc_date(row.end),
            row.boundaries
        ));
    }
    for row in &done.unstoppable {
        out.push(format!(
            "NOT stopped: #{} {} [{} .. {}] — {BACKFILL_ONE_BATCH}: its lane fetches the whole \
             window in one request and commits it once, so it runs to its end",
            row.id,
            spec_of(row),
            epoch_ms_to_utc_date(row.start),
            epoch_ms_to_utc_date(row.end)
        ));
    }
    if !done.flagged.is_empty() {
        out.push(format!(
            "the cancel does not wait: `vike-cli data hist running --addr {addr}` lists a flagged \
             request until it has stopped"
        ));
    }
    out
}

/// The `cancel --json` document: the series asked about, the datahub that answered, and the two
/// lists the datahub returned, each row carrying [`row_json`]'s fields.
pub(super) fn cancel_json(done: &BackfillCancelDone, spec: &str, addr: &str) -> serde_json::Value {
    serde_json::json!({
        "addr": addr,
        "spec": spec,
        "flagged": done.flagged.iter().map(row_json).collect::<Vec<_>>(),
        "unstoppable": done.unstoppable.iter().map(row_json).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "127.0.0.1:7878";

    fn row(id: u64, stoppable: bool, cancelled: bool) -> RunningBackfill {
        RunningBackfill {
            id,
            venue: "oanda".to_string(),
            symbol: "EUR_USD".to_string(),
            interval: "5s".to_string(),
            start: 1_104_710_400_000, // 2005-01-03
            end: 1_767_225_599_999,   // 2025-12-31
            peer: Some("127.0.0.1:50000".to_string()),
            started_ms: 1_700_000_000_000,
            elapsed_ms: 3 * 3_600_000 + 12 * 60_000,
            lane: if stoppable { "CredentialedKlines" } else { "Klines" }.to_string(),
            stoppable,
            cancelled,
            boundaries: 812,
        }
    }

    /// A `running` row carries the spec an operator pastes into `cancel`, the window by date, how
    /// long it has run, the boundary count, the peer and what a cancel can do — and the closing line
    /// names the verb that stops one, at the same address.
    #[test]
    fn a_running_table_names_each_request_and_how_to_stop_it() {
        let lines = running_lines(&[row(4, true, false), row(5, false, false)], ADDR);
        let text = lines.join("\n");
        assert!(lines[0].starts_with("2 backfills running on the datahub at 127.0.0.1:7878"));
        for header in ["ID", "SERIES", "WINDOW", "RUNNING", "BOUNDARIES", "PEER", "STOP"] {
            assert!(lines[1].contains(header), "{header}: {}", lines[1]);
        }
        assert!(lines[2].contains("#4") && lines[2].contains("oanda:EUR_USD:5s"), "{text}");
        assert!(lines[2].contains("2005-01-03 .. 2025-12-31"), "{text}");
        assert!(lines[2].contains("3h12m") && lines[2].contains("812"), "{text}");
        assert!(lines[2].contains("127.0.0.1:50000"), "{text}");
        assert!(lines[2].ends_with("at its next chunk boundary"), "{text}");
        assert!(lines[3].ends_with(BACKFILL_ONE_BATCH), "a one-batch row says so: {text}");
        assert!(text.contains("vike-cli data hist cancel SERIES --addr 127.0.0.1:7878"), "{text}");
        // The columns line up: every row's spec starts where the SERIES header does.
        let at = lines[1].find("SERIES").expect("header");
        assert_eq!(lines[2].find("oanda").expect("row"), at, "{text}");
    }

    /// A raised cancel is visible in the listing until the request reaches its boundary.
    #[test]
    fn a_flagged_request_reads_as_cancel_raised_until_it_stops() {
        let lines = running_lines(&[row(4, true, true)], ADDR);
        assert!(lines[0].starts_with("1 backfill running"), "singular: {}", lines[0]);
        assert!(
            lines[2].ends_with("CANCEL RAISED — stops at its next chunk boundary"),
            "{lines:?}"
        );
    }

    /// Nothing running is a sentence, never a bare header.
    #[test]
    fn an_empty_registry_is_one_sentence() {
        assert_eq!(
            running_lines(&[], ADDR),
            vec!["no backfill is running on the datahub at 127.0.0.1:7878".to_string()]
        );
    }

    /// The `--json` rows carry every wire field under its wire name, plus the pasteable spec.
    #[test]
    fn the_running_document_carries_every_wire_field_and_the_spec() {
        let doc = running_json(&[row(4, true, false)], ADDR);
        assert_eq!(doc["addr"], ADDR);
        let r = &doc["running"][0];
        assert_eq!(r["spec"], "oanda:EUR_USD:5s");
        for (key, want) in [
            ("id", serde_json::json!(4)),
            ("venue", serde_json::json!("oanda")),
            ("symbol", serde_json::json!("EUR_USD")),
            ("interval", serde_json::json!("5s")),
            ("start", serde_json::json!(1_104_710_400_000_i64)),
            ("end", serde_json::json!(1_767_225_599_999_i64)),
            ("peer", serde_json::json!("127.0.0.1:50000")),
            ("started_ms", serde_json::json!(1_700_000_000_000_i64)),
            ("elapsed_ms", serde_json::json!(11_520_000)),
            ("lane", serde_json::json!("CredentialedKlines")),
            ("stoppable", serde_json::json!(true)),
            ("cancelled", serde_json::json!(false)),
            ("boundaries", serde_json::json!(812)),
        ] {
            assert_eq!(r[key], want, "{key}: {r}");
        }
        assert_eq!(running_json(&[], ADDR)["running"], serde_json::json!([]));
    }

    /// A cancel names what it flagged, says it does not wait and that the stored chunks stay; it
    /// names what it could NOT stop with the shared one-batch words; and a series with nothing
    /// running is an empty SUCCESS that points at `running`.
    #[test]
    fn a_cancel_says_what_it_flagged_what_it_could_not_stop_and_when_there_was_nothing() {
        let done = BackfillCancelDone {
            flagged: vec![row(4, true, true)],
            unstoppable: vec![row(5, false, false)],
        };
        let lines = cancel_lines(&done, "oanda:EUR_USD:5s", ADDR);
        let text = lines.join("\n");
        assert!(
            lines[0].starts_with("cancel raised on #4 oanda:EUR_USD:5s [2005-01-03 .. 2025-12-31]")
        );
        assert!(lines[0].contains("stops at its next chunk boundary"), "{text}");
        assert!(lines[0].contains("stays stored") && lines[0].contains("resumes"), "{text}");
        assert!(lines[1].starts_with("NOT stopped: #5"), "{text}");
        assert!(lines[1].contains(BACKFILL_ONE_BATCH), "{text}");
        assert!(lines[2].contains("does not wait"), "{text}");

        let nothing = cancel_lines(&BackfillCancelDone::default(), "oanda:EUR_USD:5s", ADDR);
        assert_eq!(nothing.len(), 1);
        assert!(nothing[0].starts_with("nothing is running on oanda:EUR_USD:5s"), "{nothing:?}");
        assert!(nothing[0].contains("vike-cli data hist running --addr 127.0.0.1:7878"));

        // Only an unstoppable match: no "does not wait" line, because nothing was flagged.
        let only =
            BackfillCancelDone { flagged: Vec::new(), unstoppable: vec![row(5, false, false)] };
        let lines = cancel_lines(&only, "oanda:EUR_USD:5s", ADDR);
        assert_eq!(lines.len(), 1, "{lines:?}");
    }

    /// The `cancel --json` document carries the series, the address and both lists.
    #[test]
    fn the_cancel_document_carries_the_series_and_both_lists() {
        let done = BackfillCancelDone {
            flagged: vec![row(4, true, true)],
            unstoppable: vec![row(5, false, false)],
        };
        let doc = cancel_json(&done, "oanda:EUR_USD:5s", ADDR);
        assert_eq!(doc["addr"], ADDR);
        assert_eq!(doc["spec"], "oanda:EUR_USD:5s");
        assert_eq!(doc["flagged"][0]["id"], 4);
        assert_eq!(doc["flagged"][0]["cancelled"], true);
        assert_eq!(doc["unstoppable"][0]["id"], 5);
        assert_eq!(doc["unstoppable"][0]["stoppable"], false);
    }

    /// Every `vike-cli …` line these answers tell an operator to type PARSES — held to the real
    /// grammar through `crate::cmd::accepts`, never to a second copy of its spelling. The `running`
    /// table's hint carries a `SERIES` placeholder, filled with a row's own spec, which is the
    /// pasting the hint asks for.
    #[test]
    fn every_command_these_answers_tell_an_operator_to_type_parses() {
        let backticked = |line: &str| -> Vec<String> {
            line.split('`').skip(1).step_by(2).map(String::from).collect()
        };
        let mut commands: Vec<String> = Vec::new();
        let rows = [row(4, true, false)];
        for line in running_lines(&rows, ADDR) {
            commands.extend(
                backticked(&line).into_iter().map(|c| c.replace("SERIES", &spec_of(&rows[0]))),
            );
        }
        for line in cancel_lines(&BackfillCancelDone::default(), "oanda:EUR_USD:5s", ADDR)
            .into_iter()
            .chain(cancel_lines(
                &BackfillCancelDone { flagged: vec![row(4, true, true)], unstoppable: Vec::new() },
                "oanda:EUR_USD:5s",
                ADDR,
            ))
        {
            commands.extend(backticked(&line));
        }
        let commands: Vec<String> =
            commands.into_iter().filter(|c| c.starts_with("vike-cli ")).collect();
        assert!(commands.len() >= 3, "the hints were not found: {commands:?}");
        for command in &commands {
            let argv: Vec<&str> = command.split_whitespace().collect();
            crate::cmd::accepts(&argv[1..]).unwrap_or_else(|e| {
                panic!("this answer tells an operator to run `{command}`: {e}")
            });
        }
    }
}
