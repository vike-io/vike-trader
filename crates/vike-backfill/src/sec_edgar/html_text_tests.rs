use super::html_to_text;

#[test]
fn blocks_are_lines_cells_are_spaced_and_hidden_parts_go() {
    let html = r#"<html><head><title>Form 10-K</title><style>p{color:red}</style></head>
<body><div style="display:none"><ix:header><ix:hidden><ix:nonNumeric name="dei:AmendmentFlag">false</ix:nonNumeric></ix:hidden></ix:header></div>
<!-- a comment <p>not text</p> -->
<p style="font-weight:bold">Item&#160;1A.&nbsp;&nbsp;Risk <b>Factors</b></p>
<table><tr><td>Revenue</td><td>$&#160;383,285</td></tr></table>
<p>It&#8217;s &amp; more<br/>next line</p><script>var x = "<p>";</script></body></html>"#;
    assert_eq!(
        html_to_text(html),
        "Item 1A. Risk Factors\nRevenue $ 383,285\nIt\u{2019}s & more\nnext line",
        "the head's title is the file's, not the filing's words"
    );
}

#[test]
fn plain_text_keeps_its_lines_and_an_unclosed_tag_ends_the_text() {
    assert_eq!(html_to_text("  ITEM 7.   MD&amp;A \n\n\n  body  "), "ITEM 7. MD&A\nbody");
    assert_eq!(html_to_text("before <p never closed"), "before");
}
