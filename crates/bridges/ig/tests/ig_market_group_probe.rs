//! Live demo PROBE, READ-ONLY: is IG's `MARKET:{epic}` Lightstreamer item group still SERVED, does
//! the `PRICE:{epic}` group IG's documentation points at exist beside it, and does the `MARKET`
//! group carry the aggregated ladder that migration was supposed to buy?
//!
//!     cargo test -p vike-ig --test ig_market_group_probe -- --ignored --nocapture
//!
//! **Why this exists.** IG's Streaming API Reference states that the `MARKET` subscription (and its
//! alias `L1`) "reaches end of life on 1 May 2026 and will be decommissioned on 8 May 2026",
//! directing users to `PRICE`.
//! `docs/decisions/0045-order-level-replay-is-a-one-venue-capability-not-a-platform-one.md`
//! records that as a side finding and says, in the same breath, that it is the VENDOR'S STATEMENT
//! and not a measurement — while `crates/bridges/ig/src/market_data.rs`'s `market_item` still
//! builds `MARKET:{epic}` and the live feed still runs on it. This probe is how that question is
//! ASKED OF THE VENUE rather than of a document, and how it is re-asked when the answer changes.
//!
//! **READ-ONLY.** A market-data subscription places no order and touches no account state. Nothing
//! here writes, and nothing here is on any production path. Double-gated like every other
//! `*_smoke.rs`: network + `IG_DEMO_*` credentials in the credential store, self-skipping (a
//! printed note, then an early `return`) when the creds are absent or the demo login fails.
//!
//! **What it prints, and why it prints it raw.** One TLCP session per case, each dialling the same
//! URL production dials (`crates/bridges/ig/src/market_feed.rs`'s `ls_ws_url`) and sending the same
//! request bytes production sends (`crates/bridges/ig/src/lightstreamer.rs`'s
//! `create_session_request` / `subscribe_request`). Every notification line the server answers with
//! is printed VERBATIM — IG demo quotes are not secret, and a paraphrased frame is not evidence.
//! The one redaction is the `CONOK` session id, which is a live session handle. Updates are ALSO
//! printed decoded field-by-field against the schema asked for, because "the group answered" and
//! "the field carries a value" are different claims, and the ladder question is the second one.
//!
//! **The control case is load-bearing.** A `SUBOK` for a schema naming a field that cannot exist
//! would make every other `SUBOK` here worthless as evidence about FIELDS. So one case asks
//! `MARKET:{epic}` for a deliberately impossible field name: if the server refuses THAT, a `SUBOK`
//! elsewhere means the named fields are real.
//!
//! **TWO AXES, TWO CONTROLS — and the bisect exists because the first run had only one of them.**
//! A refusal here can be about the ITEM GROUP (`REQERR,..,21,Invalid group`) or about the SCHEMA
//! (`REQERR,..,23,Invalid schema`), and the 2026-09-06 run could attribute neither with confidence:
//!
//! * **The schema axis is ALL-OR-NOTHING.** The impossible-field control came back `Invalid schema`
//!   for a request whose other two fields were live in the same run, so ONE bad name poisons an
//!   otherwise-valid ask. A refused MULTI-field request therefore localises nothing — it says at
//!   least one name is bad, never that none of them exist. The cure is a BISECT: ask each ladder
//!   name as the ONLY field in its schema, with a single-field POSITIVE control (`BID` alone)
//!   beside them, so an all-refused result cannot be read as "single-field asks are rejected for
//!   some unrelated reason". A `SUBOK` for a lone ladder name would mean that field EXISTS on
//!   `MARKET`.
//! * **The item axis had NO control at all.** Every case used one real epic, so an `Invalid group`
//!   was a verdict about the group+item PAIR, not about the group. The cure is two more asks: a
//!   KNOWN-GOOD group with an impossible item, and an impossible group with the known-good item.
//!   If both answer `21`, the code is ambiguous and no `21` in this file attributes to the group;
//!   if only the second does, `21` really is a verdict about the group.
//!
//! ⚠ A missing QUOTE is not a missing GROUP. IG's demo gateway serves a snapshot on subscribe
//! (`LS_snapshot=true`), but FX is shut at the weekend, so absence of live movement says nothing.
//! The discriminating evidence is the SUBSCRIPTION ACK: `SUBOK` (served) versus `REQERR` (refused,
//! with the server's own wording) versus silence.
//!
//! ⚠ This probe dials with a bare `tungstenite::connect` — no connect bound — which a feed-path
//! file may never do (`crates/vike-ops/tests/feed_stop_windows_gate.rs` gates that). It is
//! legitimate here and only here: no stop flag exists to be ignored, a human is watching, and the
//! whole run is seconds long.

use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;

use vike_bridge_core::credentials::{Environment, load_workspace_dotenv_from};
use vike_bridge_core::user_data::{StreamError, StreamMsg, UserStream};
use vike_bridge_core::ws::{TungsteniteStream, configure_ws_stream};
use vike_ig::lightstreamer::{
    LS_WS_SUBPROTOCOL, LsFrame, MergeItemState, create_session_request, parse_frame,
    subscribe_request,
};
use vike_ig::market_feed::ls_ws_url;
use vike_ig::{IgSession, load_ig_config_from};

/// EUR/USD mini — the epic the market-feed smoke uses, present on the demo gateway.
const EPIC: &str = "CS.D.EURUSD.MINI.IP";
/// An epic in IG's own shape that no market can carry — the ITEM half of the two-axis control.
const IMPOSSIBLE_EPIC: &str = "CS.D.NOTAREALEPIC.MINI.IP";
/// An item-group name no adapter can own — the GROUP half of the same control.
const IMPOSSIBLE_GROUP: &str = "VIKENOTAGROUP";
/// How long each case holds its subscription open after the subscribe is sent.
const WINDOW: Duration = Duration::from_secs(8);
/// The shorter window the one-field bisect and item-axis cases use: their whole answer is the ACK,
/// and there are fifteen of them, so the full window would triple the run for evidence it does not
/// add. Still long enough for the `LS_snapshot=true` update to land after a `SUBOK`.
const ACK_WINDOW: Duration = Duration::from_secs(5);
/// Budget for `create_session` -> `CONOK`.
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(15);
/// Socket read timeout, so the read loop ticks rather than blocking to the window's end.
const READ_TIMEOUT: Duration = Duration::from_millis(400);
/// The subscription id every case uses (one subscription per session).
const SUB_ID: u32 = 1;

/// The four fields production's quote lane asks for (`vike_ig::market_data::QUOTE_SCHEMA`).
const CORE: &[&str] = &["BID", "OFFER", "UPDATE_TIME", "MARKET_STATE"];
/// The 5-level aggregated ladder `PRICE` is documented to carry — the fields whose existence would
/// make IG's `book: false` capability row an understatement.
const LADDER: &[&str] = &[
    "BIDPRICE1",
    "BIDSIZE1",
    "BIDPRICE2",
    "BIDSIZE2",
    "BIDPRICE3",
    "BIDSIZE3",
    "BIDPRICE4",
    "BIDSIZE4",
    "BIDPRICE5",
    "BIDSIZE5",
    "OFRPRICE1",
    "OFRSIZE1",
    "OFRPRICE2",
    "OFRSIZE2",
    "OFRPRICE3",
    "OFRSIZE3",
    "OFRPRICE4",
    "OFRSIZE4",
    "OFRPRICE5",
    "OFRSIZE5",
];
/// The ten bid-side ladder names, asked ONE AT A TIME — experiment A. A schema refusal is
/// all-or-nothing, so this is the only shape of ask whose answer is about a NAMED field.
const LADDER_BISECT: &[&str] = &[
    "BIDPRICE1",
    "BIDPRICE2",
    "BIDPRICE3",
    "BIDPRICE4",
    "BIDPRICE5",
    "BIDSIZE1",
    "BIDSIZE2",
    "BIDSIZE3",
    "BIDSIZE4",
    "BIDSIZE5",
];
/// A field name no adapter can own — the control that says whether a `SUBOK` is evidence about
/// FIELDS at all, or only about the group.
const IMPOSSIBLE_FIELD: &[&str] = &["BID", "OFFER", "VIKE_NOT_A_REAL_FIELD"];

/// Which question a case belongs to, so the transcript can render one table per axis.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Question {
    /// The original six: which item groups are served.
    Groups,
    /// Experiment A: the ladder field names, one at a time, with a single-field positive control.
    LadderBisect,
    /// Experiment B: what an `Invalid group` is actually a verdict about.
    ItemAxis,
}

/// One question to put to the venue.
struct Case {
    label: String,
    question: Question,
    group: String,
    schema: Vec<&'static str>,
    window: Duration,
    /// Why this case is asked — printed with its verdict so the transcript reads on its own.
    why: &'static str,
}

/// What the venue answered.
#[derive(Debug, Default)]
struct Answer {
    subok: Option<String>,
    reqerr: Option<String>,
    /// The numeric code of the `REQERR`, kept apart so the per-axis tables can key on it.
    reqerr_code: Option<String>,
    fatal: Option<String>,
    updates: usize,
    /// Fields that arrived with a non-null value at least once, in schema order.
    populated: Vec<String>,
}

impl Answer {
    fn verdict(&self) -> String {
        if let Some(e) = &self.fatal {
            return format!("SESSION FAILED: {e}");
        }
        if let Some(e) = &self.reqerr {
            return format!("REFUSED: {e}");
        }
        match &self.subok {
            Some(ok) => format!(
                "SERVED ({ok}); {} update(s); populated fields: [{}]",
                self.updates,
                self.populated.join(", ")
            ),
            None => format!("NO ACK ({} update(s))", self.updates),
        }
    }

    /// The one-word shape of the answer, for the per-axis tables. It says what came back and
    /// nothing about what it MEANS — the meaning is the reader's job and the decision record's.
    fn shape(&self) -> String {
        if self.fatal.is_some() {
            return "SESSION FAILED".to_string();
        }
        match (&self.reqerr_code, &self.subok) {
            (Some(code), _) => format!("REQERR {code}"),
            (None, Some(_)) => "SUBOK".to_string(),
            (None, None) => "no ack".to_string(),
        }
    }
}

/// Lightstreamer credentials from ONE IG login, reused across every case's session.
struct Auth {
    ws_url: String,
    user: String,
    password: String,
}

fn cfg() -> Option<vike_ig::IgConfig> {
    load_ig_config_from(
        Environment::Demo,
        &load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref()),
    )
}

/// Print a server line verbatim, with the `CONOK` session id redacted (a live session handle).
fn echo(line: &str) {
    if let Some(rest) = line.strip_prefix("CONOK,") {
        let tail = rest.split_once(',').map(|(_, t)| t).unwrap_or("");
        println!("    < CONOK,<session-id redacted>,{tail}");
    } else {
        println!("    < {line}");
    }
}

/// One `recv` tick, folded to the text we care about. `Ok(None)` = nothing to read this tick.
fn tick(stream: &mut TungsteniteStream) -> Result<Option<String>, String> {
    match stream.recv() {
        Ok(StreamMsg::Text(t)) => Ok(Some(t)),
        Ok(StreamMsg::Ping(p)) => {
            let _ = stream.pong(p);
            Ok(None)
        }
        Ok(StreamMsg::Other) => Ok(None),
        Err(StreamError::Timeout) => Ok(None),
        Err(StreamError::Closed(m)) => Err(m),
    }
}

/// Open one TLCP session, ask `case`, and report what came back.
fn ask(auth: &Auth, case: &Case) -> Answer {
    let mut answer = Answer::default();
    println!("\n=== {} ===", case.label);
    println!("  why:       {}", case.why);
    println!("  LS_group:  {}", case.group);
    println!("  LS_schema: {}", case.schema.join(" "));

    let mut req = match auth.ws_url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => {
            answer.fatal = Some(format!("bad LS url: {e}"));
            println!("  verdict: {}", answer.verdict());
            return answer;
        }
    };
    req.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_static(LS_WS_SUBPROTOCOL));
    let sock = match tungstenite::connect(req) {
        Ok((s, _resp)) => s,
        Err(e) => {
            answer.fatal = Some(format!("dial failed: {e}"));
            println!("  verdict: {}", answer.verdict());
            return answer;
        }
    };
    configure_ws_stream(&sock, READ_TIMEOUT);
    let mut stream = TungsteniteStream(sock);

    if let Err(e) = stream.send_text(&create_session_request(&auth.user, &auth.password)) {
        answer.fatal = Some(format!("create_session send: {e:?}"));
        println!("  verdict: {}", answer.verdict());
        return answer;
    }

    // Handshake: read until CONOK (or a refusal, or the budget).
    let deadline = Instant::now() + HANDSHAKE_BUDGET;
    let mut connected = false;
    while !connected && answer.fatal.is_none() && Instant::now() < deadline {
        let text = match tick(&mut stream) {
            Ok(Some(t)) => t,
            Ok(None) => continue,
            Err(m) => {
                answer.fatal = Some(format!("closed during handshake: {m}"));
                break;
            }
        };
        for line in text.lines().map(str::trim_end) {
            echo(line);
            match parse_frame(line) {
                Some(LsFrame::Conok { .. }) => connected = true,
                Some(LsFrame::Conerr { code, message }) => {
                    answer.fatal = Some(format!("CONERR,{code},{message}"));
                }
                Some(LsFrame::End { code, message }) => {
                    answer.fatal = Some(format!("END,{code},{message}"));
                }
                _ => {}
            }
        }
    }
    if !connected {
        answer.fatal.get_or_insert_with(|| "no CONOK within handshake budget".into());
        println!("  verdict: {}", answer.verdict());
        return answer;
    }

    let schema = case.schema.join(" ");
    let sub = subscribe_request(1, SUB_ID, "MERGE", &case.group, &schema, true);
    if let Err(e) = stream.send_text(&sub) {
        answer.fatal = Some(format!("subscribe send: {e:?}"));
        println!("  verdict: {}", answer.verdict());
        return answer;
    }
    println!("    > control LS_op=add LS_group={} LS_mode=MERGE LS_snapshot=true", case.group);

    let mut state = MergeItemState::new(case.schema.len());
    let until = Instant::now() + case.window;
    while Instant::now() < until {
        let text = match tick(&mut stream) {
            Ok(Some(t)) => t,
            Ok(None) => continue,
            Err(m) => {
                answer.fatal.get_or_insert(format!("closed: {m}"));
                break;
            }
        };
        for line in text.lines().map(str::trim_end) {
            echo(line);
            match parse_frame(line) {
                Some(LsFrame::Subok { sub_id, num_items, num_fields }) => {
                    answer.subok =
                        Some(format!("SUBOK sub={sub_id} items={num_items} fields={num_fields}"));
                }
                Some(LsFrame::Reqerr { req_id, code, message }) => {
                    answer.reqerr = Some(format!("REQERR,{req_id},{code},{message}"));
                    answer.reqerr_code = Some(code);
                }
                Some(LsFrame::Update(u)) => {
                    answer.updates += 1;
                    state.apply(&u.fields);
                    let decoded: Vec<String> = case
                        .schema
                        .iter()
                        .enumerate()
                        .map(|(i, f)| format!("{f}={}", state.get(i).unwrap_or("<null>")))
                        .collect();
                    println!("      decoded: {}", decoded.join(" "));
                }
                Some(LsFrame::End { code, message }) => {
                    answer.fatal.get_or_insert(format!("END,{code},{message}"));
                }
                Some(LsFrame::Conerr { code, message }) => {
                    answer.fatal.get_or_insert(format!("CONERR,{code},{message}"));
                }
                _ => {}
            }
        }
        if answer.reqerr.is_some() || answer.fatal.is_some() {
            break;
        }
    }
    answer.populated = case
        .schema
        .iter()
        .enumerate()
        .filter(|(i, _)| state.get(*i).is_some())
        .map(|(_, f)| (*f).to_string())
        .collect();

    println!("  verdict: {}", answer.verdict());
    answer
}

/// The six original group questions — which item groups IG still serves, and the FIELD-axis
/// control that says whether a `SUBOK` is evidence about fields at all. Unchanged since the
/// 2026-09-06 run, so every later run reproduces that measurement before extending it.
fn group_cases() -> Vec<Case> {
    vec![
        Case {
            label: "MARKET (what production sends today)".into(),
            question: Question::Groups,
            group: format!("MARKET:{EPIC}"),
            schema: CORE.to_vec(),
            window: WINDOW,
            why: "vendor docs say end-of-life 2026-05-01, decommissioned 2026-05-08",
        },
        Case {
            label: "L1 (the documented alias of MARKET)".into(),
            question: Question::Groups,
            group: format!("L1:{EPIC}"),
            schema: CORE.to_vec(),
            window: WINDOW,
            why: "the same notice names L1 alongside MARKET",
        },
        Case {
            label: "CONTROL: MARKET with an impossible field".into(),
            question: Question::Groups,
            group: format!("MARKET:{EPIC}"),
            schema: IMPOSSIBLE_FIELD.to_vec(),
            window: WINDOW,
            why: "does a SUBOK mean the FIELDS exist, or only the group? a refusal here says it does",
        },
        Case {
            label: "PRICE core (the replacement IG points at)".into(),
            question: Question::Groups,
            group: format!("PRICE:{EPIC}"),
            schema: CORE.to_vec(),
            window: WINDOW,
            why: "does the successor group exist, carrying the four fields we already consume?",
        },
        Case {
            label: "PRICE ladder (the 5-level aggregated book)".into(),
            question: Question::Groups,
            group: format!("PRICE:{EPIC}"),
            schema: LADDER.to_vec(),
            window: WINDOW,
            why: "would migrating buy depth IG's `book: false` capability row denies?",
        },
        Case {
            label: "MARKET ladder (the same depth asked of the group that IS served)".into(),
            question: Question::Groups,
            group: format!("MARKET:{EPIC}"),
            schema: LADDER.to_vec(),
            window: WINDOW,
            why: "a group-level refusal of PRICE leaves the LADDER question open — ask it here, \
                  where the group is known to exist, so the answer is about the FIELDS",
        },
    ]
}

/// Experiment A — the ladder BISECT. A single-field POSITIVE control first, then each bid-side
/// ladder name as the ONLY field in its schema, so a refusal is about that one name.
fn ladder_bisect_cases() -> Vec<Case> {
    let mut cases = vec![Case {
        label: "POSITIVE CONTROL: MARKET with BID alone".into(),
        question: Question::LadderBisect,
        group: format!("MARKET:{EPIC}"),
        schema: vec!["BID"],
        window: ACK_WINDOW,
        why: "a one-field ask naming a field known to be live — without it, an all-refused bisect \
              cannot be told apart from one-field asks being rejected for some other reason",
    }];
    for field in LADDER_BISECT {
        cases.push(Case {
            label: format!("BISECT: MARKET with {field} alone"),
            question: Question::LadderBisect,
            group: format!("MARKET:{EPIC}"),
            schema: vec![field],
            window: ACK_WINDOW,
            why: "one name, one schema: the only ask whose refusal is a verdict on THIS field",
        });
    }
    cases
}

/// Experiment B — the ITEM axis. What is an `Invalid group` a verdict about: the group, the item,
/// or the pair? Plus two alternative `PRICE` item shapes, which are only worth reading if the two
/// controls show that code 21 is ambiguous.
fn item_axis_cases() -> Vec<Case> {
    vec![
        Case {
            label: "ITEM CONTROL: known-good group, impossible item".into(),
            question: Question::ItemAxis,
            group: format!("MARKET:{IMPOSSIBLE_EPIC}"),
            schema: CORE.to_vec(),
            window: ACK_WINDOW,
            why: "if MARKET with a bogus epic also answers 21, then 21 is AMBIGUOUS and no 21 in \
                  this file can be attributed to the group",
        },
        Case {
            label: "GROUP CONTROL: impossible group, known-good item".into(),
            question: Question::ItemAxis,
            group: format!("{IMPOSSIBLE_GROUP}:{EPIC}"),
            schema: CORE.to_vec(),
            window: ACK_WINDOW,
            why: "the other half of the same control — what a group that certainly does not exist \
                  answers, against an epic that certainly does",
        },
        Case {
            label: "PRICE, alternative item shape: suffixed".into(),
            question: Question::ItemAxis,
            group: format!("PRICE:{EPIC}:TICK"),
            schema: CORE.to_vec(),
            window: ACK_WINDOW,
            why: "CHART takes a suffixed item (CHART:{epic}:{scale}); if PRICE exists but takes a \
                  different item syntax, the bare form's refusal would look identical",
        },
        Case {
            label: "PRICE, alternative item shape: bare epic, no group prefix".into(),
            question: Question::ItemAxis,
            group: EPIC.to_string(),
            schema: CORE.to_vec(),
            window: ACK_WINDOW,
            why: "the plain Lightstreamer item-name form, with no group prefix at all",
        },
    ]
}

/// Ask IG which market-data item groups it still serves, which ladder field names it knows, and
/// what a group refusal is actually a verdict about — printing every frame it answers with.
///
/// Asserts NOTHING about the answer — the answer is the point, and a probe that goes red when the
/// venue changes is a probe that gets muted. The one thing it insists on is that the SESSION came
/// up for the production `MARKET:{epic}` case, because a transport failure there means the run
/// measured nothing at all and must not be read as "the group is gone".
#[test]
#[ignore = "live demo; needs IG_DEMO_* creds — READ-ONLY (no order), run manually (see module doc)"]
fn ig_market_group_probe() {
    let Some(c) = cfg() else {
        eprintln!("SKIP: IG_DEMO creds absent");
        return;
    };
    let session = match IgSession::login(&c) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "skip: IG demo login failed (creds present but session not established): {e}"
            );
            return;
        }
    };
    if session.lightstreamer_endpoint.is_empty() {
        eprintln!("skip: login returned no lightstreamerEndpoint");
        return;
    }
    let auth = Auth {
        ws_url: ls_ws_url(&session.lightstreamer_endpoint),
        user: session.account_id.clone(),
        password: session.ls_password(),
    };
    println!("probing {} (epic {EPIC})", auth.ws_url);

    let mut cases = group_cases();
    cases.extend(item_axis_cases());
    cases.extend(ladder_bisect_cases());

    let mut verdicts = Vec::new();
    for case in &cases {
        let answer = ask(&auth, case);
        verdicts.push((case, answer));
    }

    let table = |question: Question, title: &str| {
        println!("\n---- {title} ----");
        for (case, a) in verdicts.iter().filter(|(c, _)| c.question == question) {
            println!("  {:<14}  {:<58}  {}", a.shape(), case.label, a.verdict());
        }
    };

    println!("\n================ SUMMARY ================");
    table(Question::Groups, "item groups (the original six, unchanged)");
    table(Question::ItemAxis, "experiment B - what is an `Invalid group` a verdict about?");
    table(Question::LadderBisect, "experiment A - the ladder names, one at a time");
    println!("=========================================");

    // ⚠ THE CONTROLS ARE WHAT MAKE THE OTHER TWENTY ROWS READABLE, so a control that never got a
    // session up must FAIL this test rather than print `SESSION FAILED` into the table and exit 0.
    // Without that, experiment A degrades to exactly the shape this probe exists to correct — ten
    // `Invalid schema` rows with no live positive control beside them — and a future re-run would
    // read as a measurement while measuring nothing. The table alone will not stop a reader who is
    // skimming; the exit code will.
    let control = |question: Question, needle: &str, why: &str| {
        let (case, answer) = verdicts
            .iter()
            .filter(|(c, _)| c.question == question)
            .find(|(c, _)| c.label.contains(needle))
            .unwrap_or_else(|| panic!("the {needle} control case is gone from the probe — {why}"));
        assert!(
            answer.fatal.is_none(),
            "the CONTROL case `{}` never got a TLCP session up ({}), so {why} — this run measured \
             NOTHING there. Re-run before reading any row of that experiment.",
            case.label,
            answer.fatal.as_deref().unwrap_or("")
        );
    };

    let (_, market) = &verdicts[0];
    assert!(
        market.fatal.is_none(),
        "the MARKET case never got a TLCP session up ({}), so this run measured NOTHING about \
         whether the group is served — do not read it as a decommissioning",
        market.fatal.as_deref().unwrap_or("")
    );
    control(
        Question::LadderBisect,
        "POSITIVE CONTROL",
        "a single-field ask is only evidence about the NAME if a known-good single-field ask acks \
         in the same run",
    );
    control(
        Question::ItemAxis,
        "impossible item",
        "an `Invalid group` is only a verdict about the GROUP if a bad ITEM under a good group \
         answers differently",
    );
}
