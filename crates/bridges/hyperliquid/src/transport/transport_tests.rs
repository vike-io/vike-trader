use super::*;
use std::assert_matches;
use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
// Everything below that names the signing types is `exec`-gated with its half of the
// transport; the `info_weight` schedule test at the bottom is the feeds-plane residue and
// stays feature-less. A default `cargo test` compiles and runs all of it unchanged.
#[cfg(feature = "execution")]
use crate::signing::action::{
    BatchModifyAction, CancelAction, CancelByCloidAction, CancelCloidWire, CancelWire, LimitParams,
    ModifyWire, OrderKind, OrderWire,
};

/// The official Rust SDK's test wallet (shared with `tests/signing_vectors.rs`) — a valid
/// secp256k1 key, so the `Signer` constructs and signing is exercised end-to-end.
#[cfg(feature = "execution")]
const KEY: &str = "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e";
#[cfg(feature = "execution")]
const NONCE: u64 = 1583838;

#[cfg(feature = "execution")]
fn signer() -> Signer {
    Signer::from_private_key(KEY, VenueNetwork::Mainnet).expect("SDK test key is valid")
}

/// **An `/exchange` write answered 5xx is unknown, whatever the status**, through the REAL post: this
/// venue states no code of its own, and a bare 500 or a Cloudflare 520 was a terminal reject of an order
/// that may be live. A 4xx keeps its status, and so does the `/info` door (its bounded retry reads it).
#[cfg(feature = "execution")]
#[test]
fn an_exchange_write_answered_any_5xx_is_unknown_and_a_read_keeps_its_status() {
    use std::io::{Read, Write};
    let answering = |status: u16| {
        let reply =
            format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let _ = socket.read(&mut [0u8; 2048]);
                let _ = socket.write_all(reply.as_bytes());
            }
        });
        format!("http://{addr}")
    };
    let t = crate::test_support::seed_transport(VenueNetwork::Testnet);
    for status in [500, 501, 520, 521, 522, 523, 525, 526, 527, 530] {
        let err = t.post(&answering(status), b"{}").unwrap_err();
        assert_eq!(err.code, E_TIMEOUT_AMBIGUOUS, "{status}");
        assert!(err.kind().must_requery(), "{status}: no terminal reject");
    }
    assert_eq!(t.post(&answering(400), b"{}").unwrap_err().code, 400);
    assert_eq!(t.post_with(&answering(500), b"{}", false).unwrap_err().code, 500);
}

/// **A plain-text refusal is this venue's words, and the request's signature is never in them**: hyperliquid
/// states its refusals as one plain line (declared on [`REPLY`]), so a person reads it as sent, and a line that
/// echoes the request's own signature half shows it redacted (`vike_bridge_core::transport::redact_sent_values`).
#[cfg(feature = "execution")]
#[test]
fn a_plain_text_refusal_is_shown_and_an_echoed_signature_is_redacted() {
    let t = crate::test_support::seed_transport(VenueNetwork::Testnet);
    let body = exchange_body(&order_action(), &signer(), NONCE, None);
    let payload = serde_json::to_vec(&body).expect("a JSON body");
    let (url, _) = crate::test_support::info_server(|request| {
        let r = serde_json::from_str::<serde_json::Value>(request)
            .ok()
            .and_then(|v| v["signature"]["r"].as_str().map(str::to_string))
            .unwrap_or_default();
        let line = format!("User or API Wallet does not exist; r={r}");
        let reply = format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{line}",
            line.len()
        );
        Box::leak(reply.into_boxed_str())
    });
    let e = t.post(&url, &payload).unwrap_err();
    assert_eq!((e.code, e.msg.as_str()), (400, "User or API Wallet does not exist; r=[redacted]"));
}

/// A single-order `order` action (asset 1, buy 3.5 @ 2000, Ioc) — the shape exec submits.
#[cfg(feature = "execution")]
fn order_action() -> Action {
    crate::test_support::sdk_order("2000", "Ioc", None)
}

#[cfg(feature = "execution")]
#[test]
fn exchange_body_has_action_nonce_signature_and_omits_vault() {
    let body = exchange_body(&order_action(), &signer(), NONCE, None);

    // action serializes inline as the tagged order envelope.
    assert_eq!(body["action"]["type"], "order");
    assert_eq!(body["action"]["orders"][0]["a"], 1);
    assert_eq!(body["action"]["orders"][0]["p"], "2000");

    // nonce is the numeric u64 we passed.
    assert_eq!(body["nonce"].as_u64(), Some(NONCE));

    // signature is the `{r,s,v}` object: r/s are 0x + 64 hex, v ∈ {27,28}.
    let sig = &body["signature"];
    let r = sig["r"].as_str().expect("r present");
    let s = sig["s"].as_str().expect("s present");
    assert!(r.starts_with("0x") && r.len() == 66, "malformed r: {r}");
    assert!(s.starts_with("0x") && s.len() == 66, "malformed s: {s}");
    assert_matches!(sig["v"].as_u64(), Some(27) | Some(28), "v: {}", sig["v"]);

    // no vault → the key is ABSENT (not null), so the venue's hash marker matches.
    assert!(body.get("vaultAddress").is_none(), "vaultAddress must be omitted when absent");
}

#[cfg(feature = "execution")]
#[test]
fn exchange_body_includes_lowercased_vault_address_when_present() {
    let vault = [0xABu8; 20];
    let body = exchange_body(&order_action(), &signer(), NONCE, Some(vault));
    assert_eq!(
        body["vaultAddress"].as_str(),
        Some("0xabababababababababababababababababababab"),
        "vaultAddress is 0x + lowercased 20-byte hex"
    );
}

#[cfg(feature = "execution")]
#[test]
fn signature_is_deterministic_for_a_fixed_action_and_nonce() {
    // RFC-6979 ECDSA ⇒ the same body is produced twice (guards against nondeterministic assembly).
    let a = exchange_body(&order_action(), &signer(), NONCE, None);
    let b = exchange_body(&order_action(), &signer(), NONCE, None);
    assert_eq!(a, b);
}

/// Answers one request with `reply`, on a loopback port: no venue is called.
fn serve(reply: &'static str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// A 2xx whose body cannot be read: on a write the venue took the request and what became of it is
/// unknown (`code: 0` is "it never left this host", a terminal reject of the whole batch); a read has no
/// outcome, so it keeps `code: 0`.
#[test]
fn an_unreadable_2xx_reply_is_ambiguous_for_a_write_only() {
    let html = "HTTP/1.1 200 OK\r\ncontent-length: 6\r\nconnection: close\r\n\r\n<html>";
    let empty = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    let t = crate::test_support::seed_transport(VenueNetwork::Testnet);
    for reply in [html, empty] {
        let e = t.post_with(&serve(reply), b"{}", true).unwrap_err();
        assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "{e}");
        assert!(e.kind().must_requery(), "{e}");
        assert!(e.msg.contains("bad json"), "{e}");
    }
    let e = t.post_with(&serve(html), b"{}", false).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
}

/// **B-3: a 3xx on an `/exchange` write is outcome unknown.** No agent follows a redirect, so the 3xx is
/// the answer and says nothing about the order: a write re-queries instead of a terminal reject, before
/// its body is read. A read keeps the status as its code.
#[test]
fn a_redirected_write_is_unknown_and_a_redirected_read_keeps_its_status() {
    let moved = "HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:9/\r\ncontent-length: 0\r\n\
                 connection: close\r\n\r\n";
    let t = crate::test_support::seed_transport(VenueNetwork::Testnet);
    let e = t.post_with(&serve(moved), b"{}", true).unwrap_err();
    assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "{e}");
    assert!(e.kind().must_requery(), "{e}");
    assert_eq!(e.msg, "network error: redirect http 302, outcome unknown");
    assert_eq!(t.post_with(&serve(moved), b"{}", false).unwrap_err().code, 302);
}

#[test]
fn info_weight_schedule_matches_research_9() {
    for t in ["l2Book", "clearinghouseState", "orderStatus", "spotClearinghouseState", "allMids"] {
        assert_eq!(info_weight(t), 2, "{t} is a light read");
    }
    assert_eq!(info_weight("userRole"), 60);
    assert_eq!(info_weight("meta"), 20);
    assert_eq!(info_weight("spotMeta"), 20);
    assert_eq!(info_weight(""), 20, "an absent type defaults to the heavy bucket");
}

#[cfg(feature = "execution")]
#[test]
fn exchange_weight_is_one_plus_batch_over_forty() {
    assert_eq!(exchange_weight(0), 1);
    assert_eq!(exchange_weight(1), 1);
    assert_eq!(exchange_weight(39), 1);
    assert_eq!(exchange_weight(40), 2);
    assert_eq!(exchange_weight(79), 2);
    assert_eq!(exchange_weight(80), 3);
}

#[cfg(feature = "execution")]
#[test]
fn action_batch_len_reads_each_variant() {
    let order = OrderWire {
        asset: 1,
        is_buy: true,
        limit_px: "2000".to_string(),
        sz: "3.5".to_string(),
        reduce_only: false,
        order_type: OrderKind::Limit(LimitParams { tif: "Gtc".to_string() }),
        cloid: None,
    };
    assert_eq!(action_batch_len(&order_action()), 1);
    assert_eq!(
        action_batch_len(&Action::Cancel(CancelAction {
            cancels: vec![CancelWire { asset: 1, oid: 1 }, CancelWire { asset: 1, oid: 2 }],
        })),
        2
    );
    assert_eq!(
        action_batch_len(&Action::CancelByCloid(CancelByCloidAction {
            cancels: vec![CancelCloidWire { asset: 1, cloid: "0x00".to_string() }],
        })),
        1
    );
    assert_eq!(action_batch_len(&Action::Modify(ModifyWire { oid: 1, order: order.clone() })), 1);
    assert_eq!(
        action_batch_len(&Action::BatchModify(BatchModifyAction {
            modifies: vec![
                ModifyWire { oid: 1, order: order.clone() },
                ModifyWire { oid: 2, order },
            ],
        })),
        2
    );
}

/// `/info` answered by a loopback venue with `replies` in turn (the last repeating): what it returned, when
/// each post arrived, and its retry lines as `<level> <request_type> attempt=<n> <error>`.
#[cfg(feature = "execution")]
fn info_against(
    replies: &'static [&'static str],
) -> (Result<serde_json::Value, VenueApiError>, Vec<std::time::Instant>, Vec<String>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let arrived = Arc::new(Mutex::new(Vec::new()));
    let (at, next) = (Arc::clone(&arrived), AtomicUsize::new(0));
    let (url, _) = crate::test_support::info_server(move |_| {
        at.lock().unwrap().push(std::time::Instant::now());
        replies[next.fetch_add(1, Ordering::SeqCst).min(replies.len() - 1)]
    });
    let transport = HyperliquidTransport::new(VenueNetwork::Testnet, &url);
    let (got, lines) =
        vike_log::capture::captured(|| transport.info(&serde_json::json!({ "type": "allMids" })));
    let field = |line: &vike_log::capture::CapturedEvent, name: &str| {
        line.field(name).unwrap_or_default().to_string()
    };
    let retries = lines
        .iter()
        .filter(|line| line.message == "info request transient failure; retrying")
        .map(|line| {
            let (kind, attempt) = (field(line, "request_type"), field(line, "attempt"));
            format!("{} {kind} attempt={attempt} {}", line.level, field(line, "error"))
        })
        .collect();
    let arrived = arrived.lock().unwrap().clone();
    (got, arrived, retries)
}

/// **`/info`'s bounded retry, pinned before its ladder moved to the shared ladder (`vike_utility::retry`)**: a throttle or a
/// 5xx is posted again 200 ms, then 400 ms, later (three posts in all), each retry after ONE `warn` naming its
/// attempt and the failure; the third failure is handed back with no line of its own. Any other refusal, and
/// an answer, end it at once.
#[cfg(feature = "execution")]
#[test]
fn an_info_read_retries_a_throttle_or_a_5xx_twice_200_then_400_ms_apart() {
    const THROTTLED: &str =
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 4\r\nconnection: close\r\n\r\nslow";
    const DOWN: &str =
        "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nconnection: close\r\n\r\ndown";
    const REFUSED: &str =
        "HTTP/1.1 400 Bad Request\r\ncontent-length: 3\r\nconnection: close\r\n\r\nbad";
    const ANSWERED: &str =
        "HTTP/1.1 200 OK\r\ncontent-length: 8\r\nconnection: close\r\n\r\n{\"ok\":1}";
    let ms = std::time::Duration::from_millis;
    let warn =
        |attempt: &str, error: &VenueApiError| format!("WARN allMids attempt={attempt} {error}");

    let (got, arrived, retries) = info_against(&[THROTTLED]);
    let last = got.expect_err("three throttles");
    assert_eq!(last.code, 429, "{last}");
    assert_eq!(arrived.len(), 3, "one post and two retries");
    assert!(
        arrived[1] - arrived[0] >= ms(200) && arrived[2] - arrived[1] >= ms(400),
        "{arrived:?}"
    );
    assert_eq!(retries, [warn("1", &last), warn("2", &last)]);

    let (got, arrived, retries) = info_against(&[DOWN, ANSWERED]);
    assert_eq!(got.expect("answered on the retry"), serde_json::json!({"ok": 1}));
    assert_eq!(arrived.len(), 2);
    assert!(arrived[1] - arrived[0] >= ms(200), "{arrived:?}");
    assert_eq!(retries.len(), 1, "{retries:?}");
    assert!(retries[0].starts_with("WARN allMids attempt=1 venue error 503"), "{retries:?}");

    let (got, arrived, retries) = info_against(&[REFUSED]);
    assert_eq!(got.expect_err("refused").code, 400);
    assert_eq!((arrived.len(), retries.len()), (1, 0), "a refusal is never asked again");
}
