use super::decode_entities;

#[test]
fn numeric_then_named_references_decode_and_amp_goes_last() {
    assert_eq!(
        decode_entities("it&#8217;s &#x2014; &lt;b&gt; &quot;q&quot;&nbsp;&apos;"),
        "it\u{2019}s \u{2014} <b> \"q\" '"
    );
    assert_eq!(decode_entities("&amp;lt;"), "&lt;", "a decoded &amp; never re-creates a reference");
    assert_eq!(
        decode_entities("&#xZZ; &#; 5 & 6"),
        "&#xZZ; &#; 5 & 6",
        "malformed stays as written"
    );
}
