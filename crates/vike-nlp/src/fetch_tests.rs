use super::*;
use crate::pins::ModelTask;

/// sha256 of the three bytes `abc` (FIPS 180-2's first example).
const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

/// A model whose one file is the bytes `abc`, pinned at an address no test ever dials.
const ABC_MODEL: PinnedModel = PinnedModel {
    name: "abc",
    repository: "example/abc",
    revision: "0000000000000000000000000000000000000000",
    license: "none",
    task: ModelTask::Embedding,
    max_tokens: 8,
    files: &[PinnedFile { path: "abc.txt", sha256: ABC_SHA256, size: 3 }],
};

fn scratch(tag: &str) -> vike_paths::scratch::ScratchDir {
    vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), &format!("vike_nlp_{tag}"))
        .expect("scratch dir")
}

#[test]
fn bytes_are_accepted_only_at_the_pinned_size_and_digest() {
    let file = ABC_MODEL.files[0];
    assert!(check_bytes("x", b"abc", &file).is_ok());
    let longer = check_bytes("x", b"abcd", &file).unwrap_err().to_string();
    assert!(longer.contains("size 3") && longer.contains("size 4"), "{longer}");
    let swapped = check_bytes("x", b"abd", &file).unwrap_err().to_string();
    assert!(swapped.contains(ABC_SHA256), "the pin is named: {swapped}");
}

#[test]
fn a_missing_file_says_fetch_and_a_changed_file_is_refused() {
    let dir = scratch("verify");
    let missing = read_verified(&ABC_MODEL, dir.path(), "abc.txt").unwrap_err();
    assert!(matches!(missing, NlpError::NotFetched { model: "abc", .. }), "{missing}");
    assert!(missing.to_string().contains("vike-nlp fetch"), "{missing}");

    let folder = ABC_MODEL.folder_in(dir.path());
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("abc.txt"), b"abd").unwrap();
    assert!(matches!(
        read_verified(&ABC_MODEL, dir.path(), "abc.txt"),
        Err(NlpError::Digest { .. })
    ));
    std::fs::write(folder.join("abc.txt"), b"abc").unwrap();
    assert_eq!(read_verified(&ABC_MODEL, dir.path(), "abc.txt").unwrap(), b"abc");
    assert!(read_verified(&ABC_MODEL, dir.path(), "other.txt").is_err(), "not a pinned file");
}

#[test]
fn a_verified_folder_is_kept_without_a_download() {
    // The pinned address is never dialled: every file already matches, so fetch only verifies.
    let dir = scratch("keep");
    let folder = ABC_MODEL.folder_in(dir.path());
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("abc.txt"), b"abc").unwrap();
    assert_eq!(fetch(&ABC_MODEL, dir.path()).unwrap(), folder);
    assert_eq!(std::fs::read(folder.join("abc.txt")).unwrap(), b"abc");
}

/// **A download that goes silent fails at the idle bound**, instead of waiting for a peer that sent one byte and
/// then nothing (here the server holds the socket open for ten seconds).
#[test]
fn a_download_that_goes_silent_fails_at_the_idle_bound() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/abc.txt", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let Ok((mut s, _)) = listener.accept() else { return };
        let _ = s.read(&mut [0u8; 4096]);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\na");
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(10));
    });
    let started = std::time::Instant::now();
    let err = download(&download_agent(Duration::from_millis(300)), &url, 3).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}: {err}", started.elapsed());
}

/// The download agent's bounds, as fields: a per-read idle bound of the shared `DEFAULT_TIMEOUT` and NO total
/// ceiling on the call or the body (the 438 MB weights must not be cut off), the connect bound, and the network
/// layer's stated redirect limit rather than ureq's ten.
#[test]
fn the_download_agent_states_its_bounds() {
    let agent = download_agent(vike_transport::http::DEFAULT_TIMEOUT);
    let config = agent.config();
    assert_eq!(config.timeout_per_read(), Some(vike_transport::http::DEFAULT_TIMEOUT));
    assert_eq!(config.timeouts().global, None);
    assert_eq!(config.timeouts().recv_body, None);
    assert_eq!(config.timeouts().connect, Some(CONNECT_TIMEOUT));
    assert_eq!(config.max_redirects(), vike_transport::http::PUBLIC_GET_MAX_REDIRECTS);
    assert!(config.http_status_as_error(), "a 4xx/5xx is the error it always was");
}
