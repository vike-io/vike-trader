//! `list_sdks` and `read_sdk_file`'s negotiation and round trip, against the fake peer: refused
//! client-side with nothing sent by a server that predates the versioned strategy SDK, and SENT, with the
//! answer read back typed, by one that advertises `sdk_source`. The real server's half (what it lists and
//! what paths it refuses) is `crates/vike-backtesthub/tests/sdk_source.rs`'s.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{FEATURE_SDK_SOURCE, FEATURE_STUDY_FILES, Request, Response};
use vike_backtesthub_client::sdk_source::{WireSdk, WireSdkEntry, WireSdkFile, WireSdkListing};
use vike_backtesthub_client::test_support::spawn_fake_peer;

/// A server that does not advertise the capability is refused CLIENT-side, both verbs, by name, with
/// nothing sent.
#[test]
fn a_server_before_the_sdk_is_refused_without_sending() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_STUDY_FILES.to_string()], |_| None);
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let refusals = [
        client.list_sdks().map(|_| ()).expect_err("the list needs sdk_source"),
        client.read_sdk_file("sdk-1", "crates").map(|_| ()).expect_err("so does a file"),
    ];
    for refusal in refusals {
        assert!(
            refusal.contains(FEATURE_SDK_SOURCE),
            "the refusal names the capability: {refusal}"
        );
        assert!(refusal.contains("Nothing was sent"), "{refusal}");
    }
    drop(client);
    assert!(rx.recv().expect("count").is_empty(), "nothing after the handshake");
    handle.join().expect("join");
}

/// A server that advertises it is SENT each verb, with its arguments as given, and its answer reads back
/// typed; its refusal is the server's own words.
#[test]
fn a_server_that_advertises_the_sdk_answers_both_verbs() {
    let listing = WireSdkListing {
        root: "/srv/vike/vendor/vike-strategy-source".to_string(),
        sdks: vec![WireSdk {
            name: "sdk-3".to_string(),
            number: Some(3),
            commit: "a3ca741107".to_string(),
            abi_version: Some(14),
            usable: true,
            why_unusable: String::new(),
        }],
    };
    let folder = WireSdkFile {
        sdk: "sdk-3".to_string(),
        path: "crates".to_string(),
        folder: true,
        text: String::new(),
        entries: vec![WireSdkEntry { name: "vike-tactics".to_string(), folder: true }],
    };
    let (listed, read) = (listing.clone(), folder.clone());
    let (addr, rx, handle) =
        spawn_fake_peer(vec![FEATURE_SDK_SOURCE.to_string()], move |request| match request {
            Request::ListSdks => Some(Response::Sdks(listed.clone())),
            Request::ReadSdkFile { path, .. } if path == "crates" => {
                Some(Response::SdkFile(read.clone()))
            }
            Request::ReadSdkFile { .. } => Some(Response::Error("no such file".to_string())),
            _ => None,
        });
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    assert_eq!(client.list_sdks().expect("listed"), listing);
    assert_eq!(client.read_sdk_file("sdk-3", "crates").expect("read"), folder);
    assert_eq!(
        client.read_sdk_file("sdk-3", "crates/../SDK").expect_err("refused"),
        "no such file"
    );
    drop(client);
    let sent = rx.recv().expect("count");
    assert_eq!(sent.len(), 3, "every call was sent: {sent:?}");
    assert!(matches!(
        &sent[2],
        Request::ReadSdkFile { sdk, path } if sdk == "sdk-3" && path == "crates/../SDK"
    ));
    handle.join().expect("join");
}
