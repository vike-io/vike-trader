use super::*;

/// **The readers' "for now" words are the const**: every `429` or timeout refusal starts with
/// `PROBE_REFUSED_FOR_NOW`, which `config url probes` and `trade health` read as LIMIT, and no other does.
#[test]
fn the_refused_for_now_refusals_start_with_the_readers_words() {
    use UrlProbeRefusalId as Id;
    let for_now = [Id::RateLimited, Id::NoAnswerInTime, Id::ConnectTimedOut, Id::NoTlsAnswer];
    for id in UrlProbeRefusalId::ALL {
        assert_eq!(
            id.template().starts_with(vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW),
            for_now.contains(&id),
            "{}: {}",
            id.word(),
            id.template()
        );
    }
    assert_eq!(
        UrlProbeRefusal::ConnectTimedOut { error: "deadline".into() }.to_string(),
        "rate-limited or temporarily refused: connect timed out (deadline)"
    );
}
