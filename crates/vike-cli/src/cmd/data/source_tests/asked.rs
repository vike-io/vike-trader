//! `show VENUE --addr A`: a datahub's answer, an older datahub's fallback, and a real dial.
use super::*;

// ── `show VENUE --addr A`: the history-channels read, the owner's Q3 ─────────────────────────────

/// A served answer: the client's own compiled rows standing in for a server's, with an overlay
/// planted on OANDA's credentialed row and on what the store holds.
fn served(addr: &str) -> Asked {
    let mut report = compiled_report(today());
    let oanda = report.venues.iter_mut().find(|v| v.venue == "oanda").expect("oanda");
    oanda.channels[0].mounted = Some(true);
    oanda.channels[0].credential = CredentialPresence::Absent;
    oanda.held = vec![HeldKind {
        kind: "bar".to_string(),
        series: 2,
        rows: 1_234,
        first_ts: vike_model::time::days_from_civil(2015, 1, 1) * vike_model::MS_PER_DAY,
        last_ts: today(),
    }];
    Asked { addr: addr.to_string(), report, served: true }
}

/// **The served table**: the server's rows, its overlay on the credentialed row, what its store
/// holds — and an ending that names the datahub that answered, never the sentence saying no server
/// was asked, which would be false here.
#[test]
fn an_addr_answer_prints_the_servers_rows_and_overlay_and_names_who_answered() {
    let asked = served("127.0.0.1:7878");
    let text = asked_lines(&row_of("oanda"), &asked).join("\n");
    assert!(text.contains("as the datahub at 127.0.0.1:7878 declares it"), "{text}");
    assert!(text.contains("v20 REST candles"), "{text}");
    assert!(text.contains("since 2005-01-03"), "the server's depth cell: {text}");
    assert!(text.contains("mounted:") && text.contains("yes"), "{text}");
    assert!(text.contains(CredentialPresence::Absent.phrase()), "{text}");
    assert!(text.contains("held — what the store at 127.0.0.1:7878 holds for it:"), "{text}");
    assert!(text.contains("2 series, 1234 rows, 2015-01-01 .. 2026-09-30"), "{text}");
    assert!(!text.contains(NOT_VERIFIED), "a server WAS asked: {text}");
    assert!(!text.contains(HISTORY_NOTE), "the rows are the SERVER's table, not this binary's");
    assert!(text.trim_end().ends_with(&asked_closer(&asked)), "{text}");
    assert!(text.contains("nothing was read from a vendor"), "{text}");

    let doc: serde_json::Value =
        serde_json::from_str(&asked_json(&row_of("oanda"), &asked)).expect("one document");
    assert_eq!(doc["verified_against_the_vendor"], false, "a datahub is not a vendor");
    assert_eq!(doc["datahub"]["served"], true);
    assert_eq!(doc["datahub"]["addr"], "127.0.0.1:7878");
    assert_eq!(doc["channels"][0]["credential"], "Absent");
    assert_eq!(doc["held"][0]["rows"], 1234);
}

/// **An older datahub**: this binary's own table, under the caption, with the overlay marked not
/// known — and no store line it never asked for.
#[test]
fn an_older_datahub_is_answered_from_this_binarys_table_and_says_so() {
    let asked = Asked {
        addr: "127.0.0.1:7878".to_string(),
        report: compiled_report(today()),
        served: false,
    };
    let text = asked_lines(&row_of("oanda"), &asked).join("\n");
    assert!(text.contains(COMPILED_TABLE_CAPTION), "{text}");
    assert!(!text.contains("mounted:"), "nothing is known to be mounted: {text}");
    assert!(text.contains(CredentialPresence::NotChecked.phrase()), "{text}");
    assert!(text.contains("held: not known"), "{text}");
    let doc: serde_json::Value =
        serde_json::from_str(&asked_json(&row_of("oanda"), &asked)).expect("one document");
    assert_eq!(doc["datahub"]["served"], false);
    assert_eq!(doc["datahub"]["caption"], COMPILED_TABLE_CAPTION);
    assert!(doc["held"].is_null(), "no store was asked: {}", doc["held"]);
}

/// **The verb's second consumer, driven headless** (the design's Q3): a REAL datahub with no
/// collector table answers, every built row reading not mounted — and a server that does not
/// advertise the read is answered from this binary's table with nothing sent to it.
#[test]
fn the_cli_asks_a_real_datahub_and_falls_back_on_an_older_one() {
    use std::net::TcpListener;
    use std::sync::Arc;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr").to_string();
    let store: Arc<dyn vike_data::HistStore + Send + Sync> =
        Arc::new(vike_data::MemHistStore::new());
    std::thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    let asked = ask_the_datahub(&addr, None, today()).expect("the datahub answers");
    assert!(asked.served, "a current datahub serves the read");
    let text = asked_lines(&row_of("oanda"), &asked).join("\n");
    assert!(
        text.contains("mounted:") && text.contains("no — that datahub mounts no lane"),
        "{text}"
    );
    assert!(text.contains("held: nothing"), "{text}");

    // An OLDER datahub: answers `Hello` without the capability and must receive nothing else.
    let old = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let old_addr = old.local_addr().expect("addr").to_string();
    let (tx, rx) = std::sync::mpsc::channel::<usize>();
    std::thread::spawn(move || {
        use vike_datahub_client::{
            proto::{Request, Response, write_frame},
            read_frame,
        };
        let (mut s, _) = old.accept().expect("accept");
        let _ = read_frame::<_, Request>(&mut s).expect("Hello");
        write_frame(
            &mut s,
            &Response::Welcome {
                proto_version: vike_datahub_client::PROTO_VERSION,
                features: vec!["load_bars".to_string()],
                nonce: None,
            },
        )
        .expect("Welcome");
        let mut after = 0;
        while read_frame::<_, Request>(&mut s).is_ok() {
            after += 1;
        }
        let _ = tx.send(after);
    });
    let asked =
        ask_the_datahub(&old_addr, None, today()).expect("an older datahub is not an error");
    assert!(!asked.served);
    assert_eq!(rx.recv().expect("the fake reports"), 0, "nothing was sent after the handshake");

    // And an unreachable one is the CONNECT rung, not a fallback.
    let closed = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr");
    let err = ask_the_datahub(&closed.to_string(), None, today()).expect_err("nothing listens");
    assert_eq!(err.exit, crate::exit::Exit::Connect);
}
