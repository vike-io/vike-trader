//! The RT gap's source gate: what the types cannot see, read out of the harness's own source.

/// The harness this gate reads, at compile time: it sits two directories up in the same crate's
/// `tests/`, ships wherever this file ships, and a rename breaks the build rather than the gate.
#[cfg(test)]
const HARNESS: &str = include_str!("../../runtime_latency.rs");

/// The bounds that make a `for` loop a SAMPLE loop, i.e. the old direct pattern of a measured
/// window: `for i in 0..N`, `0..N as u64`, `0..SNAPSHOT_BUILDS as u32`. Every harness in the file
/// names its sample count through one of these.
#[cfg(test)]
const SAMPLE_COUNTS: [&str; 3] = ["N", "HOP_SAMPLES", "SNAPSHOT_BUILDS"];

/// `src` with every comment and the CONTENTS of every string and char literal blanked to spaces
/// (same length, same newlines), so prose and messages can neither satisfy nor trip a rule. Raw
/// strings are not special-cased: the harness has none, and a lexer that went wrong on one would
/// trip the non-vacuity asserts below rather than pass quietly.
#[cfg(test)]
fn code_only(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, at: usize| {
        if out[at] != b'\n' {
            out[at] = b' ';
        }
    };
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                blank(&mut out, i);
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            while i < b.len() && !b[i..].starts_with(b"*/") {
                blank(&mut out, i);
                i += 1;
            }
            for _ in 0..2 {
                if i < b.len() {
                    blank(&mut out, i);
                    i += 1;
                }
            }
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    blank(&mut out, i);
                    i += 1;
                }
                if i < b.len() {
                    blank(&mut out, i);
                    i += 1;
                }
            }
            i += 1; // the closing quote stays
        } else if b[i] == b'\'' && b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') {
            out[i + 1] = b' ';
            out[i + 2] = b' ';
            i += 4;
        } else if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            out[i + 1] = b' ';
            i += 3;
        } else {
            i += 1;
        }
    }
    // Only ASCII bytes were written, and only over whole comments or literal contents, so no
    // multi-byte character was ever cut in half.
    String::from_utf8(out).expect("blanking keeps the text UTF-8")
}

/// Byte offset to 1-based line number, for the messages.
#[cfg(test)]
fn line_of(code: &str, at: usize) -> usize {
    code[..at].matches('\n').count() + 1
}

/// The byte offset of every `for` loop whose range runs from 0 to a [`SAMPLE_COUNTS`] bound.
#[cfg(test)]
fn sample_loops(code: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for (at, _) in code.match_indices("for ") {
        let at_word_start =
            code[..at].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let Some(brace) = code[at..].find('{') else { continue };
        let head = &code[at..at + brace];
        let Some(range) = head.find(" in ").map(|k| head[k + 4..].trim_start()) else { continue };
        let Some(bound) = range.strip_prefix("0..") else { continue };
        let ident: String =
            bound.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if at_word_start && SAMPLE_COUNTS.contains(&ident.as_str()) {
            out.push(at);
        }
    }
    out
}

/// The span of the body of the function whose signature starts with `sig`, if there is one.
#[cfg(test)]
fn fn_body(code: &str, sig: &str) -> Option<std::ops::Range<usize>> {
    body_from(code, code.find(sig)?)
}

/// The span (`{` to its matching `}`) of the body of the item whose signature starts at `at`.
#[cfg(test)]
fn body_from(code: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let open = at + code[at..].find('{')?;
    let mut depth = 0usize;
    for (k, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open..open + k);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every function in `code` whose body opens a window: (offset of its `fn`, its body span).
#[cfg(test)]
fn window_fns(code: &str) -> Vec<(usize, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    for (at, _) in code.match_indices("fn ") {
        let at_word_start =
            code[..at].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        // A body-less declaration (`fn f();`) has no body of its own to read.
        let has_body = matches!(
            (code[at..].find('{'), code[at..].find(';')),
            (Some(brace), semi) if semi.is_none_or(|semi| brace < semi)
        );
        if !(at_word_start && has_body) {
            continue;
        }
        if let Some(body) =
            body_from(code, at).filter(|b| code[b.clone()].contains(".start_window()"))
        {
            out.push((at, body));
        }
    }
    out
}

/// The text between the `(` at `open` and its matching `)`, if it closes.
#[cfg(test)]
fn call_args(code: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    for (k, c) in code[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&code[open + 1..open + k]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A character that can sit inside a Rust identifier.
#[cfg(test)]
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The `let` statement around the expression `text[at..end]`, when it reads
/// `let [mut] <name> = <lead><expression>;` with a plain identifier for `<name>`: the name, the
/// lead, and the offset just past the `;`. `lead` is whatever precedes the expression inside
/// the initializer (a receiver, for a method call), trimmed. `None` for any other shape: a bare
/// expression statement, a pattern, a type ascription, or an expression that does not end the
/// statement.
#[cfg(test)]
fn let_around(text: &str, at: usize, end: usize) -> Option<(&str, &str, usize)> {
    let stmt = text[..at].rfind([';', '{', '}']).map_or(0, |k| k + 1);
    let rest = text[stmt..at].trim_start().strip_prefix("let ")?.trim_start();
    let rest = rest.strip_prefix("mut ").map_or(rest, str::trim_start);
    let (name, lead) = rest.split_once('=')?;
    let name = name.trim();
    let after = text[end..].trim_start();
    let semi = text.len() - after.len();
    (!name.is_empty() && name.chars().all(is_ident) && after.starts_with(';')).then_some((
        name,
        lead.trim(),
        semi + 1,
    ))
}

/// The offset of the first `let [mut] <name>` at or after `from` in `text` (the binding is shadowed
/// from there on), or `text.len()` if `name` is never bound again.
#[cfg(test)]
fn rebound_at(text: &str, from: usize, name: &str) -> usize {
    text[from..]
        .match_indices("let ")
        .map(|(k, _)| from + k)
        .find(|&at| {
            let word_start = text[..at].chars().next_back().is_none_or(|c| !is_ident(c));
            let rest = text[at + "let ".len()..].trim_start();
            let rest = rest.strip_prefix("mut ").map_or(rest, str::trim_start);
            let binds = rest
                .strip_prefix(name)
                .is_some_and(|after| after.chars().next().is_none_or(|c| !is_ident(c)));
            word_start && binds
        })
        .unwrap_or(text.len())
}

/// Rule 6 for ONE window, the one whose `.start_window()` sits at `start` in the function body
/// `text`: follow it BY BINDING to a report line. `next_start` is where the function's next window
/// opens (or the body's end), so this window's own `.close_window()` must come before it. `Err`
/// says where the chain breaks.
#[cfg(test)]
fn window_reaches_report(text: &str, start: usize, next_start: usize) -> Result<(), String> {
    const CLOSE: &str = ".close_window()";
    const FROM_HOPS: &str = "HopStats::from_hops";
    let close = text[start..next_start]
        .find(CLOSE)
        .map(|k| start + k)
        .ok_or("is not closed in this function before its next window opens")?;
    // The receiver is not checked: position already pairs this close with this window, and the
    // compiler refuses a close on any `OpenWindow` but the one still open.
    let (window, _, after_close) = let_around(text, close, close + CLOSE.len()).ok_or(
        "is closed outside a `let <window> = <open>.close_window();` statement, so the `Window` \
         it returns cannot be followed",
    )?;
    let window_scope = rebound_at(text, after_close, window);
    let mut why = format!(
        "is bound to `{window}`, which is never handed to `HopStats::from_hops(..)` as its last \
         argument"
    );
    for (k, _) in text[after_close..window_scope].match_indices(&format!("{FROM_HOPS}(")) {
        let call = after_close + k;
        let paren = call + FROM_HOPS.len();
        let Some(args) = call_args(text, paren) else { continue };
        if !args.split_whitespace().collect::<String>().ends_with(&format!(",{window}")) {
            continue;
        }
        let call_end = paren + args.len() + 2; // the `(`, the arguments, the `)`
        let (stats, after_call) = match let_around(text, call, call_end) {
            Some((stats, "", after_call)) if stats != "_" => (stats, after_call),
            Some(("_", "", _)) => {
                why = format!(
                    "hands `{window}` to `HopStats::from_hops(..)` but binds the stats to `_`, \
                     which drops them"
                );
                continue;
            }
            _ => {
                why = format!(
                    "hands `{window}` to `HopStats::from_hops(..)`, but the stats it builds are \
                     not bound by `let <stats> = HopStats::from_hops(..);`, so they are dropped"
                );
                continue;
            }
        };
        let reported = format!("{stats}.report(");
        let stats_scope = rebound_at(text, after_call, stats);
        let called = text[after_call..stats_scope].match_indices(&reported).any(|(k, _)| {
            let at = after_call + k;
            text[..at].chars().next_back().is_none_or(|c| !is_ident(c) && c != '.')
        });
        if called {
            return Ok(());
        }
        why = format!("builds `{stats}` from `{window}`, but `{stats}.report(..)` is never called");
    }
    Err(why)
}

/// Everything wrong with how a harness file opens its windows, one line per violation; empty means
/// clean. Six rules:
///   1. every `RtGap::open(` passes the REAL sleep and the REAL clock, verbatim;
///   2. every function that opens a window opens its gap FIRST: its first statement (after any
///      `const` items) is the `RtGap::open(` call, so the gap precedes all setup — `spawn_core`
///      above all — and hop #0 measures what it measured before the gap existed;
///   3. every `.start_window()` is followed by its `.close_window()` before the next one opens;
///   4. every sample loop lies inside a window, and every window holds exactly ONE sample loop
///      (so a window cannot be a decoy around nothing, nor merge two windows into one long one);
///   5. every `spin_loop()` lies inside a window, or inside the `spin_until` helper that windows
///      call;
///   6. EVERY window a function opens — each one, not only the first — is followed BY BINDING to a
///      report: `let <window> = <open>.close_window();`, that `<window>` handed to
///      `HopStats::from_hops(..)` as the last argument, the result bound by
///      `let <stats> = HopStats::from_hops(..);` (not `_`, not left as a bare statement), and
///      `<stats>.report(..)` called, with neither name rebound in between. A `.report(` on any
///      other value — another window's stats included — does not count. That report line is the
///      ONLY place `busy_ns` (and `window_ns`) reach the persisted series, since nothing asserts
///      `busy`, so a window that was measured and never reported would drop the field silently.
#[cfg(test)]
fn window_violations(src: &str) -> Vec<String> {
    let code = code_only(src);
    let mut bad = Vec::new();

    for (_, body) in window_fns(&code) {
        let text = &code[body.clone()];
        let starts: Vec<usize> = text.match_indices(".start_window()").map(|(k, _)| k).collect();
        for (n, &start) in starts.iter().enumerate() {
            let next_start = starts.get(n + 1).copied().unwrap_or(text.len());
            if let Err(why) = window_reaches_report(text, start, next_start) {
                bad.push(format!(
                    "line {}: the window opened here {why}. Bind its `Window` \
                     (`let <window> = <open>.close_window();`), hand it to \
                     `HopStats::from_hops(..)` as the last argument, bind the result \
                     (`let <stats> = HopStats::from_hops(..);`) and call `<stats>.report(..)`: \
                     that line is the only way this window's `window_ns` and `busy_ns` reach the \
                     persisted series",
                    line_of(&code, body.start + start)
                ));
            }
        }
    }

    for (at, body) in window_fns(&code) {
        // Skip the `{`, then any `const NAME: T = ...;` items, to the first real statement.
        let mut rest = code[body.start + 1..body.end].trim_start();
        while rest.starts_with("const ") {
            rest = rest.split_once(';').map_or("", |(_, after)| after).trim_start();
        }
        let first = rest.split_once(';').map_or(rest, |(stmt, _)| stmt);
        if !first.contains("RtGap::open(") {
            bad.push(format!(
                "line {}: this function opens a window, but its first statement is not the \
                 `RtGap::open(..)` call. The gap must be slept at the TOP of the harness, before \
                 any setup (`spawn_core` above all): slept after it, the core thread finishes \
                 starting during the gap and hop #0 stops measuring what it measured before",
                line_of(&code, at)
            ));
        }
    }

    for (at, _) in code.match_indices("RtGap::open(") {
        let args: String =
            code[at..].chars().take_while(|&c| c != ')').filter(|c| !c.is_whitespace()).collect();
        if args != "RtGap::open(std::thread::sleep,Instant::now" {
            bad.push(format!(
                "line {}: `RtGap::open` must be passed `std::thread::sleep, Instant::now`, the real \
                 sleep and clock; a stand-in here would skip the gap the kernel needs",
                line_of(&code, at)
            ));
        }
    }

    let mut marks: Vec<(usize, bool)> = code
        .match_indices(".start_window()")
        .map(|(at, _)| (at, true))
        .chain(code.match_indices(".close_window()").map(|(at, _)| (at, false)))
        .collect();
    marks.sort_unstable();
    let mut spans = Vec::new();
    let mut opened: Option<usize> = None;
    for (at, is_start) in marks {
        match (opened, is_start) {
            (None, true) => opened = Some(at),
            (Some(start), false) => {
                spans.push(start..at);
                opened = None;
            }
            _ => bad.push(format!(
                "line {}: windows must alternate `.start_window()` / `.close_window()`; this one \
                 does not",
                line_of(&code, at)
            )),
        }
    }
    if let Some(start) = opened {
        bad.push(format!("line {}: a window opened here is never closed", line_of(&code, start)));
    }
    let inside = |at: usize| spans.iter().any(|r| r.contains(&at));

    let loops = sample_loops(&code);
    for &at in &loops {
        if !inside(at) {
            bad.push(format!(
                "line {}: a sample loop runs OUTSIDE a window. Open an `RtGap` at the top of the \
                 harness and put this loop, and nothing else, between `start_window()` and \
                 `close_window()`",
                line_of(&code, at)
            ));
        }
    }
    for span in &spans {
        let n = loops.iter().filter(|&&at| span.contains(&at)).count();
        if n != 1 {
            bad.push(format!(
                "line {}: a window must hold exactly ONE sample loop; this one holds {n}",
                line_of(&code, span.start)
            ));
        }
    }

    let helper = fn_body(&code, "fn spin_until(");
    for (at, _) in code.match_indices("spin_loop()") {
        if !inside(at) && !helper.as_ref().is_some_and(|r| r.contains(&at)) {
            bad.push(format!(
                "line {}: a spin OUTSIDE a window. Spinning is what the kernel's RT budget counts, \
                 so it belongs inside a timed window behind a gap",
                line_of(&code, at)
            ));
        }
    }
    bad
}

/// THE GATE: every measured window in the real harness goes through `RtGap`, so a harness added
/// later cannot escape the gap by copying the old direct loop.
#[test]
fn every_measured_loop_in_the_harness_runs_inside_a_window() {
    let bad = window_violations(HARNESS);
    assert!(
        bad.is_empty(),
        "crates/vike-core/tests/runtime_latency.rs has a measured window that breaks a rule of the \
         RT gap (see crates/vike-core/tests/common/rt_gap.rs's module doc for why each exists):\n{}",
        bad.join("\n")
    );
    // Non-vacuity: a gate whose patterns stopped matching the file would pass on anything.
    let code = code_only(HARNESS);
    assert!(
        !sample_loops(&code).is_empty() && code.contains(".start_window()"),
        "the gate found no sample loop or no window in the harness: its patterns no longer match \
         the file, so it is checking nothing"
    );
    assert!(
        fn_body(&code, "fn spin_until(").is_some(),
        "the `spin_until` helper exemption names a function the harness no longer has"
    );
    assert!(
        !window_fns(&code).is_empty(),
        "the open-the-gap-first rule found no function that opens a window: its `fn` walk no \
         longer matches the file, so that rule is checking nothing"
    );
}

/// A miniature harness shaped like the real one, for the planted-violation tests below.
#[cfg(test)]
const COMPLIANT: &str = r#"
fn spin_until(c: &AtomicU64, target: u64) {
    while c.load(Ordering::Acquire) < target {
        std::hint::spin_loop();
    }
}
fn run(label: &str) {
    const N: usize = HOP_SAMPLES;
    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let handle = spawn_core(engine, cfg);
    for i in 0..resting {
        spin_until(&submitted, i);
    }
    let spinning = gap.start_window();
    for i in 0..N as u64 {
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window = spinning.close_window();
    let stats = HopStats::from_hops(label, Arc::try_unwrap(hops).unwrap(), window);
    stats.report(label, "");
    // for i in 0..N { std::hint::spin_loop(); } is prose, not code
    let s = "for i in 0..N { std::hint::spin_loop() } .close_window()";
}
"#;

#[test]
fn the_source_gate_passes_a_compliant_harness_and_ignores_prose() {
    assert_eq!(window_violations(COMPLIANT), Vec::<String>::new());
}

/// The old direct pattern: the loop runs on its own and the window is opened around nothing.
#[test]
fn the_source_gate_refuses_a_sample_loop_outside_its_window() {
    let src = COMPLIANT
        .replace("    let spinning = gap.start_window();\n", "")
        .replace("spinning.close_window()", "gap.start_window().close_window()");
    let bad = window_violations(&src);
    assert!(bad.iter().any(|v| v.contains("sample loop runs OUTSIDE")), "{bad:?}");
    assert!(bad.iter().any(|v| v.contains("holds 0")), "{bad:?}");
    assert!(bad.iter().any(|v| v.contains("spin OUTSIDE")), "{bad:?}");
}

#[test]
fn the_source_gate_refuses_a_spin_outside_a_window() {
    let src = COMPLIANT.replace(
        "    let window = spinning.close_window();\n",
        "    let window = spinning.close_window();\n    while busy() { std::hint::spin_loop(); }\n",
    );
    let bad = window_violations(&src);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("spin OUTSIDE"), "{bad:?}");
}

/// The hole the types and the other rules left open: the gap slept AFTER `spawn_core`, right before
/// the window. Every window is still behind a gap, so only this rule sees that hop #0 changed.
#[test]
fn the_source_gate_refuses_a_gap_opened_after_setup() {
    let open = "    let gap = rt_gap::RtGap::open(std::thread::sleep, Instant::now);\n";
    let spawn = "    let handle = spawn_core(engine, cfg);\n";
    let src = COMPLIANT.replace(&format!("{open}{spawn}"), &format!("{spawn}{open}"));
    assert_ne!(src, COMPLIANT, "the fixture must carry the open-then-spawn pair this test swaps");
    let bad = window_violations(&src);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("first statement"), "{bad:?}");
}

/// A window that is measured and never reported: its `busy_ns` (asserted by nothing) would simply
/// never reach the series. Both halves of rule 6, each on its own.
#[test]
fn the_source_gate_refuses_a_window_that_never_reaches_the_report() {
    let report = "    stats.report(label, \"\");\n";
    let unreported = COMPLIANT.replace(report, "");
    assert_ne!(unreported, COMPLIANT, "the fixture must carry the report call this test deletes");
    let bad = window_violations(&unreported);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("persisted series"), "{bad:?}");

    let elsewhere = COMPLIANT.replace(".unwrap(), window);", ".unwrap(), stale_window);");
    assert_ne!(elsewhere, COMPLIANT, "the fixture must carry the from_hops call this test edits");
    let bad = window_violations(&elsewhere);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("persisted series"), "{bad:?}");
}

/// A SECOND window in the same function, compliant on its own: its own gap, one sample loop, its
/// own `Window`, stats and report. [`two_windows`] appends it after the first window's report.
#[cfg(test)]
const SECOND_WINDOW: &str = r#"    let gap2 = rt_gap::RtGap::open(std::thread::sleep, Instant::now);
    let spinning2 = gap2.start_window();
    for i in 0..N as u64 {
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    let window2 = spinning2.close_window();
    let stats2 = HopStats::from_hops(label, Arc::try_unwrap(hops2).unwrap(), window2);
    stats2.report(label, "");
"#;

/// The first window's report line in [`COMPLIANT`], which the rule-6 fixtures edit.
#[cfg(test)]
const FIRST_REPORT: &str = "    stats.report(label, \"\");\n";

/// [`COMPLIANT`] with [`SECOND_WINDOW`] after its report: one function, two windows, two reports.
#[cfg(test)]
fn two_windows() -> String {
    assert!(COMPLIANT.contains(FIRST_REPORT), "the fixture must carry the first window's report");
    COMPLIANT.replace(FIRST_REPORT, &format!("{FIRST_REPORT}{SECOND_WINDOW}"))
}

/// The 1-based line of the first `needle` in `src`, to pin WHICH window a violation names.
#[cfg(test)]
fn line_in(src: &str, needle: &str) -> usize {
    line_of(src, src.find(needle).expect("the fixture carries the needle"))
}

/// The control for the rule-6 tests below: two windows in one function, each reported through its
/// own bindings, are clean. A gate that went red here would make a correct harness unwritable.
#[test]
fn the_source_gate_passes_a_function_that_reports_each_of_two_windows() {
    assert_eq!(window_violations(&two_windows()), Vec::<String>::new());
}

/// Rule 6 follows the BINDING to the report: a `.report(` on some other value, or on the OTHER
/// window's stats, does not report this window.
#[test]
fn the_source_gate_refuses_a_report_called_on_an_unrelated_value() {
    let unrelated = COMPLIANT.replace(FIRST_REPORT, "    baseline.report(label, \"\");\n");
    assert_ne!(unrelated, COMPLIANT, "the fixture must carry the report call this test edits");
    let bad = window_violations(&unrelated);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("`stats.report(..)` is never called"), "{bad:?}");
    assert!(bad[0].contains("persisted series"), "{bad:?}");

    // The first window "reported" through the second window's stats: both report lines would be
    // the second window's, and the first window's `busy_ns` would never reach the series.
    let src = two_windows();
    let borrowed = src.replacen(FIRST_REPORT, "    stats2.report(label, \"\");\n", 1);
    assert_ne!(borrowed, src, "the fixture must carry the first window's report");
    let bad = window_violations(&borrowed);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("`stats.report(..)` is never called"), "{bad:?}");
}

/// EVERY window in a function is checked, not only the first: a second window that never reports
/// is red, whether its stats are built and left unreported or it never reaches `from_hops` at all.
/// The violation names the SECOND window's line.
#[test]
fn the_source_gate_checks_every_window_in_a_function_not_only_the_first() {
    let src = two_windows();
    let second = format!("line {}:", line_in(&src, "gap2.start_window()"));

    let unreported = src.replace("    stats2.report(label, \"\");\n", "");
    assert_ne!(unreported, src, "the fixture must carry the second window's report");
    let bad = window_violations(&unreported);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&second), "the violation must name the second window: {bad:?}");
    assert!(bad[0].contains("`stats2.report(..)` is never called"), "{bad:?}");

    let stats2 =
        "    let stats2 = HopStats::from_hops(label, Arc::try_unwrap(hops2).unwrap(), window2);\n";
    let never_handed = unreported.replace(stats2, "");
    assert_ne!(never_handed, unreported, "the fixture must carry the second window's from_hops");
    let bad = window_violations(&never_handed);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&second), "the violation must name the second window: {bad:?}");
    assert!(bad[0].contains("`window2`, which is never handed"), "{bad:?}");
}

/// A window whose stats are built and then DROPPED — bound to `_`, or never bound at all — is red
/// even when another window in the same function supplies a `.report(` for a text match to find.
#[test]
fn the_source_gate_refuses_a_window_whose_stats_are_dropped() {
    let src = two_windows();
    let first = format!("line {}:", line_in(&src, "gap.start_window()"));
    let bound = "    let stats = HopStats::from_hops(";
    assert!(src.contains(bound), "the fixture must carry the first window's from_hops binding");
    let unreported = src.replace(FIRST_REPORT, "");

    let to_underscore = unreported.replace(bound, "    let _ = HopStats::from_hops(");
    let bad = window_violations(&to_underscore);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&first), "the violation must name the first window: {bad:?}");
    assert!(bad[0].contains("binds the stats to `_`"), "{bad:?}");

    let unbound = unreported.replace(bound, "    HopStats::from_hops(");
    let bad = window_violations(&unbound);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].starts_with(&first), "the violation must name the first window: {bad:?}");
    assert!(bad[0].contains("not bound"), "{bad:?}");
}

#[test]
fn the_source_gate_refuses_a_gap_opened_with_a_stand_in_sleep() {
    let src = COMPLIANT.replace("std::thread::sleep", "|_| {}");
    let bad = window_violations(&src);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("the real"), "{bad:?}");
}
