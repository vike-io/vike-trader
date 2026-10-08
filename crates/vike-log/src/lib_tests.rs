use super::*;
use std::path::{Path, PathBuf};

/// The whole ladder, every ADJACENT pair asserted: a test of only the extremes passes with the
/// middle two swapped, and those are the pair with a real argument behind their order.
#[test]
fn dir_precedence_env_over_cfg_over_project_over_exe() {
    let exe = Path::new("/opt/vike");
    let cfg = PathBuf::from("/var/cfg");
    let project = PathBuf::from("/proj/settings/state/logs");

    // env wins over everything
    assert_eq!(
        resolve_log_dir(Some("/env/logs"), Some(&cfg), Some(&project), exe),
        PathBuf::from("/env/logs")
    );
    // cfg (a config file's log_dir / a --log-dir flag) wins over the project default
    assert_eq!(resolve_log_dir(None, Some(&cfg), Some(&project), exe), cfg);
    // the project default wins over the exe fallback
    assert_eq!(resolve_log_dir(None, None, Some(&project), exe), project);
    // last resort: <exe_dir>/logs, for a binary with no project above it
    assert_eq!(resolve_log_dir(None, None, None, exe), PathBuf::from("/opt/vike/logs"));
    // …and the env var still wins with nothing else set at all.
    assert_eq!(resolve_log_dir(Some("/env/logs"), None, None, exe), PathBuf::from("/env/logs"));
}

/// `LogConfig::default()` names no directory itself: a `Default` that resolved a path would make
/// every `..Default::default()` touch the filesystem.
#[test]
fn the_default_config_resolves_no_directory_of_its_own() {
    let cfg = LogConfig::default();
    assert!(cfg.dir.is_none());
    assert!(cfg.project_dir.is_none());
    assert_eq!(
        resolve_log_dir(None, cfg.dir.as_deref(), cfg.project_dir.as_deref(), Path::new("/x")),
        PathBuf::from("/x/logs")
    );
}

/// The BASE of a composed directive: everything except the pins this crate appends. Precedence
/// is a property of the base; each pin family has its own tests.
///
/// ⚠ Every family this crate appends must be stripped here, or the PRECEDENCE tests fail with a
/// diff that looks like a precedence bug and is not.
///
/// ⚠ The caller-supplied family ([`file_level_directive_with_pins`]) is out of scope: it is DATA
/// ([`LogConfig::file_target_pins`]), not a constant. A precedence test that feeds it pins must
/// strip its OWN; do not add a fourth constant here, which would be the coupling that family
/// exists to avoid.
fn base_of(directive: &str) -> String {
    directive
        .split(',')
        .filter(|d| {
            let d = d.trim();
            !CREDENTIAL_BEARING_TARGETS.iter().any(|t| d.starts_with(t))
                && !NOISY_TARGETS.iter().any(|(t, _)| d.starts_with(t))
                && !HIGH_VOLUME_TARGETS.iter().any(|(t, _)| d.starts_with(t))
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn filter_precedence_rust_log_over_vike_log_over_default() {
    assert_eq!(base_of(&filter_directive(Some("debug"), Some("warn"), "info")), "debug");
    assert_eq!(base_of(&filter_directive(None, Some("warn"), "info")), "warn");
    assert_eq!(base_of(&filter_directive(None, None, "info")), "info");
}

/// The file layer's own precedence: the console knobs never reach it.
#[test]
fn file_level_precedence_env_over_cfg() {
    assert_eq!(base_of(&file_level_directive(Some("warn"), "trace")), "warn");
    assert_eq!(base_of(&file_level_directive(Some("off"), "trace")), "off");
    assert_eq!(base_of(&file_level_directive(None, "trace")), "trace");
    // An empty or whitespace-only env value is NOT a directive — it would widen the filter to
    // the `EnvFilter` default rather than narrow it, so it falls through to the config.
    assert_eq!(base_of(&file_level_directive(Some(""), "trace")), "trace");
    assert_eq!(base_of(&file_level_directive(Some("  "), "trace")), "trace");
    // Surrounding whitespace is trimmed rather than passed into `EnvFilter`.
    assert_eq!(base_of(&file_level_directive(Some(" warn "), "trace")), "warn");
}

/// A caller-supplied pin RAISES one target above the global file level (the audit trail under a
/// daemon unit's `VIKE_LOG_FILE_LEVEL=warn`); the global level still leads.
#[test]
fn a_pinned_target_survives_a_lower_global_file_level() {
    let directive = file_level_directive_with_pins(
        Some("warn"),
        "trace",
        &[("vike_tradehub::audit".to_string(), "info".to_string())],
    );
    assert!(directive.starts_with("warn"), "the global level still leads: {directive}");
    assert!(
        directive.contains("vike_tradehub::audit=info"),
        "the pinned target must be raised: {directive}"
    );
}

/// A pin may only ever RAISE: at `trace` the same pin would quieten the target.
#[test]
fn a_pin_never_lowers_a_target_below_the_global_level() {
    let directive = file_level_directive_with_pins(
        None,
        "trace",
        &[("vike_tradehub::audit".to_string(), "info".to_string())],
    );
    assert!(!directive.contains("vike_tradehub::audit=info"), "got: {directive}");
}

/// An EXPLICIT mention of the target wins, and a directive with no bare global level is left
/// ALONE.
#[test]
fn a_pin_yields_to_an_explicit_mention_and_to_an_unreadable_global() {
    let pins = [("vike_tradehub::audit".to_string(), "info".to_string())];

    let explicit =
        file_level_directive_with_pins(Some("warn,vike_tradehub::audit=error"), "trace", &pins);
    assert!(
        !explicit.contains("vike_tradehub::audit=info"),
        "a typed-out directive for the target wins: {explicit}"
    );

    // No bare level = nothing to compare a pin against.
    let targets_only = file_level_directive_with_pins(Some("ureq=trace"), "trace", &pins);
    assert!(
        !targets_only.contains("vike_tradehub::audit=info"),
        "an unreadable global level is left alone: {targets_only}"
    );
}

/// `off` is not an exemption ([`file_level_directive_with_pins`]): the pin holds there too.
#[test]
fn a_pin_holds_even_when_the_global_file_level_is_off() {
    let directive = file_level_directive_with_pins(
        Some("off"),
        "trace",
        &[("vike_tradehub::audit".to_string(), "info".to_string())],
    );
    assert!(directive.starts_with("off"), "the global level still leads: {directive}");
    assert!(directive.contains("vike_tradehub::audit=info"), "got: {directive}");
}

/// No pins = the unpinned directive, byte for byte.
#[test]
fn no_pins_is_byte_identical_to_the_unpinned_directive() {
    for (env, cfg) in [(Some("warn"), "trace"), (None, "trace"), (Some("off"), "info")] {
        assert_eq!(
            file_level_directive_with_pins(env, cfg, &[]),
            file_level_directive(env, cfg),
            "an empty pin list must change nothing ({env:?}, {cfg})"
        );
    }
}

/// Both layers pin the credential-bearing targets at the two levels where they LEAK: `debug` and
/// `trace`.
#[test]
fn credential_bearing_targets_are_pinned_at_the_levels_that_leak() {
    for base in ["trace", "debug"] {
        let file = file_level_directive(None, base);
        let console = filter_directive(None, None, base);
        for t in CREDENTIAL_BEARING_TARGETS {
            assert!(
                file.contains(&format!("{t}=info")),
                "file directive for base {base:?} must pin {t}, got {file:?}"
            );
            assert!(
                console.contains(&format!("{t}=info")),
                "console directive for base {base:?} must pin {t}, got {console:?}"
            );
        }
    }
    // ...and an operator's own `VIKE_LOG_FILE_LEVEL=trace` is pinned just the same.
    let d = file_level_directive(Some("trace"), "warn");
    assert!(d.contains("ureq=info") && d.contains("tungstenite=info"), "got {d:?}");
}

/// [`NOISY_TARGETS`] are pinned on the console at every base that admits their WARN chatter.
#[test]
fn noisy_third_party_targets_are_pinned_on_the_console() {
    for base in ["warn", "info", "debug", "trace"] {
        let console = filter_directive(None, None, base);
        for (target, level) in NOISY_TARGETS {
            assert!(
                console.contains(&format!("{target}={level}")),
                "console directive for base {base:?} must pin {target}, got {console:?}"
            );
        }
    }
}

/// The NOISE pins are console-only; the file keeps the chatter as forensics. ⚠ The file still
/// carries [`HIGH_VOLUME_TARGETS`]'s `wgpu_hal` CRATE pin: this asserts only that the console's
/// `wgpu_hal::vulkan::instance` MODULE pin is absent.
#[test]
fn the_noise_pins_do_not_touch_the_file_layer() {
    for base in ["warn", "info", "debug", "trace"] {
        let file = file_level_directive(None, base);
        for (target, _) in NOISY_TARGETS {
            assert!(!file.contains(target), "file directive must not pin {target}: {file:?}");
        }
    }
}

/// An EXPLICIT mention of a noisy target wins.
#[test]
fn an_explicitly_named_noisy_target_is_not_pinned_over() {
    let d = filter_directive(Some("info,wgpu_hal::vulkan::instance=trace"), None, "info");
    assert!(d.contains("wgpu_hal::vulkan::instance=trace"), "{d:?}");
    assert!(!d.contains("wgpu_hal::vulkan::instance=error"), "no second directive: {d:?}");
}

/// ⚠ A pin may only ever NARROW: at a base that already excludes the leak, `ureq=info` would
/// RAISE the target (`off,ureq=info` writes into a file the operator turned OFF).
#[test]
fn a_pin_never_raises_a_target_above_the_base_level() {
    for base in ["off", "error", "warn", "info"] {
        let file = file_level_directive(None, base);
        let console = filter_directive(None, None, base);
        assert_eq!(file, base, "base {base:?} already excludes the leak — no pin may be added");
        // The console's NOISE pins obey the same rule. Compared EXACTLY, so an unexpected
        // directive of any kind still fails here.
        assert_eq!(console, expected_console(base), "same for the console filter at base {base:?}");
    }
    // the same via the env knob, which is how an operator actually silences the file
    assert_eq!(file_level_directive(Some("off"), "trace"), "off");
    assert_eq!(file_level_directive(Some("warn"), "trace"), "warn");
}

/// The render loop ([`HIGH_VOLUME_TARGETS`]) is pinned OUT of the file at `debug` and `trace`.
#[test]
fn the_render_loop_is_pinned_out_of_the_file_at_debug_and_trace() {
    for base in ["debug", "trace"] {
        let file = file_level_directive(None, base);
        for (target, level) in HIGH_VOLUME_TARGETS {
            assert!(
                file.contains(&format!("{target}={level}")),
                "file directive for base {base:?} must pin {target}, got {file:?}"
            );
        }
    }
}

/// An EXPLICIT mention of a high-volume target wins.
#[test]
fn an_explicitly_named_high_volume_target_is_not_pinned_over() {
    let d = file_level_directive(Some("trace,wgpu_core=trace"), "trace");
    assert!(d.contains("wgpu_core=trace"), "{d:?}");
    assert!(!d.contains("wgpu_core=info"), "no second directive for it: {d:?}");
    // ...and the family's other members are still pinned, so opting one in does not opt all in.
    assert!(d.contains("naga=info"), "{d:?}");
}

/// The volume pins are FILE-only, the mirror of [`the_noise_pins_do_not_touch_the_file_layer`].
#[test]
fn the_volume_pins_do_not_touch_the_console() {
    for base in ["warn", "info", "debug", "trace"] {
        let console = filter_directive(None, None, base);
        for (target, level) in HIGH_VOLUME_TARGETS {
            assert!(
                !console.contains(&format!("{target}={level}")),
                "console directive for base {base:?} must not carry the volume pin {target}, \
                     got {console:?}"
            );
        }
    }
}

/// `base` plus exactly those [`NOISY_TARGETS`] pins that NARROW it — derived from the rule
/// rather than from the implementation, so the two have to agree.
fn expected_console(base: &str) -> String {
    let mut out = base.to_string();
    for (target, level) in NOISY_TARGETS {
        if level_rank(base) > level_rank(level) {
            out.push_str(&format!(",{target}={level}"));
        }
    }
    out
}

/// An EXPLICIT mention of a credential-bearing target wins.
#[test]
fn an_explicit_target_directive_is_not_overridden_by_the_pin() {
    let d = file_level_directive(Some("trace,ureq=trace"), "warn");
    assert!(d.contains("ureq=trace"), "the operator's own ureq directive must survive: {d:?}");
    assert!(
        !d.contains("ureq=info"),
        "and must not be shadowed by a second, conflicting ureq directive: {d:?}"
    );
    // the target they did NOT name is still pinned
    assert!(d.contains("tungstenite=info"), "got {d:?}");
}

/// In-memory `MakeWriter` that captures everything written to it, so a test can inspect the
/// bytes a layer produced without touching the real stderr/global subscriber.
#[derive(Clone, Default)]
struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

/// **A live reload changes what the layer emits**, and the [`CREDENTIAL_BEARING_TARGETS`] pins
/// SURVIVE it: after [`LogReloadHandles::reload_console_level`] raises `info` to `debug`, a plain
/// `debug!` is captured while a `ureq`-target `debug!` is still filtered. A reload API that took a
/// raw directive would fail this.
#[test]
fn a_console_reload_applies_live_and_keeps_the_credential_pins() {
    let buf = SharedBuf::default();
    let make_writer = {
        let buf = buf.clone();
        move || buf.clone()
    };
    let (filter, handle): (reload::Layer<EnvFilter, Registry>, _) =
        reload::Layer::new(EnvFilter::new("info"));
    let layer = console_layer(ConsoleFormat::Json, false, filter, make_writer);
    let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![layer];
    let subscriber = tracing_subscriber::registry().with(layers);
    let handles = LogReloadHandles { console: handle, file: None, file_pins: Vec::new() };
    crate::capture::scoped(subscriber, || {
        tracing::debug!(marker = "before_reload", "filtered at the boot level");
        handles.reload_console_level(None, None, "debug").expect("the console filter must reload");
        tracing::debug!(marker = "after_reload", "captured at the reloaded level");
        tracing::debug!(target: "ureq::run", marker = "pinned_target", "full-URL leak shape");
    });

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
    assert!(!out.contains("before_reload"), "the boot-level filter must hold first: {out}");
    assert!(out.contains("after_reload"), "the reloaded level must apply live: {out}");
    assert!(
        !out.contains("pinned_target"),
        "the ureq=info credential pin must survive a hot reload to debug: {out}"
    );
}

/// The file half: with NO file layer [`LogReloadHandles::reload_file_level`] answers `Err`
/// (restart-required); with one, a reload from `trace` to `off` applies live.
#[test]
fn a_file_reload_is_refused_without_a_file_layer_and_applies_with_one() {
    let none =
        LogReloadHandles { console: reload_probe_handle(), file: None, file_pins: Vec::new() };
    let err = none.reload_file_level(None, "warn").expect_err("no file layer ⇒ Err");
    assert!(err.contains("no reloadable FILE layer"), "{err}");

    let buf = SharedBuf::default();
    let make_writer = {
        let buf = buf.clone();
        move || buf.clone()
    };
    let (filter, handle): (reload::Layer<EnvFilter, Registry>, _) =
        reload::Layer::new(EnvFilter::new(file_level_directive(None, "trace")));
    let layer: Box<dyn Layer<Registry> + Send + Sync> =
        fmt::layer().json().with_writer(make_writer).with_filter(filter).boxed();
    let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![layer];
    let subscriber = tracing_subscriber::registry().with(layers);
    let handles = LogReloadHandles {
        console: reload_probe_handle(),
        file: Some(handle),
        file_pins: Vec::new(),
    };
    crate::capture::scoped(subscriber, || {
        tracing::info!(marker = "file_before_reload", "captured at trace");
        handles.reload_file_level(Some("off"), "trace").expect("the file filter must reload");
        tracing::info!(marker = "file_after_off", "must NOT be captured");
    });

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
    assert!(out.contains("file_before_reload"), "{out}");
    assert!(!out.contains("file_after_off"), "`off` must apply live to the file layer: {out}");
}

/// A hot reload recomposes the pins the boot composed. `crates/vike-tradehub/src/hot_reload.rs`'s
/// `LogLevelApplier` answers a `SetSetting` on `preferences.log_file_level` through
/// [`LogReloadHandles::reload_file_level`]; composing there without the pins would drop the audit
/// pin for the life of the process. Asserted by behaviour, not spelling: after a reload to
/// `warn`, an `info` line on the PINNED target is written and one on any other target is not.
#[test]
fn a_file_reload_recomposes_the_pins_the_boot_composed() {
    let buf = SharedBuf::default();
    let make_writer = {
        let buf = buf.clone();
        move || buf.clone()
    };
    let (filter, handle): (reload::Layer<EnvFilter, Registry>, _) =
        reload::Layer::new(EnvFilter::new(file_level_directive(None, "trace")));
    let layer: Box<dyn Layer<Registry> + Send + Sync> =
        fmt::layer().json().with_writer(make_writer).with_filter(filter).boxed();
    let layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![layer];
    let subscriber = tracing_subscriber::registry().with(layers);
    // A probe target: the property under test is the composition, not one caller.
    let handles = LogReloadHandles {
        console: reload_probe_handle(),
        file: Some(handle),
        file_pins: vec![("pin_reload_probe::audit".to_string(), "info".to_string())],
    };
    crate::capture::scoped(subscriber, || {
        handles.reload_file_level(Some("warn"), "trace").expect("the file filter must reload");
        tracing::info!(
            target: "pin_reload_probe::audit",
            marker = "pinned_after_reload",
            "the pinned target survives an operator turning the file down"
        );
        tracing::info!(
            target: "pin_reload_probe::other",
            marker = "unpinned_after_reload",
            "every other target still obeys the reloaded global level"
        );
    });

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
    assert!(
        out.contains("pinned_after_reload"),
        "a hot reload dropped the pin — a SetSetting on preferences.log_file_level would take \
             the audit trail off disk for the life of the process: {out}"
    );
    assert!(
        !out.contains("unpinned_after_reload"),
        "the reloaded global level must still hold for every unpinned target: {out}"
    );
}

/// A detached console handle for tests that only exercise the FILE half.
fn reload_probe_handle() -> ReloadableFilterHandle {
    let (_layer, handle): (reload::Layer<EnvFilter, Registry>, _) =
        reload::Layer::new(EnvFilter::new("info"));
    handle
}

#[test]
fn console_json_format_emits_json() {
    let buf = SharedBuf::default();
    let make_writer = {
        let buf = buf.clone();
        move || buf.clone()
    };
    let layer = console_layer(ConsoleFormat::Json, false, EnvFilter::new("trace"), make_writer);
    let subscriber = tracing_subscriber::registry().with(layer);
    crate::capture::scoped(subscriber, || {
        tracing::info!(marker = "console_json_test", "hello from the console json test");
    });

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
    assert!(out.trim_start().starts_with('{'), "expected JSON console output, got: {out}");
    assert!(
        out.contains("console_json_test"),
        "expected the event field in the output, got: {out}"
    );
}

#[test]
fn console_pretty_format_does_not_emit_json() {
    let buf = SharedBuf::default();
    let make_writer = {
        let buf = buf.clone();
        move || buf.clone()
    };
    let layer = console_layer(ConsoleFormat::Pretty, true, EnvFilter::new("trace"), make_writer);
    let subscriber = tracing_subscriber::registry().with(layer);
    crate::capture::scoped(subscriber, || {
        tracing::info!(marker = "console_pretty_test", "hello from the console pretty test");
    });

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
    assert!(!out.trim_start().starts_with('{'), "expected non-JSON pretty output, got: {out}");
    assert!(
        out.contains("console_pretty_test"),
        "expected the event field in the output, got: {out}"
    );
}

/// END-TO-END: a credential-shaped URL logged by the transport stack cannot reach the FILE
/// layer at the DEFAULT level. Builds the real JSON file layer over the real composed directive
/// and emits the records `ureq` and `tungstenite` emit (same targets, levels and leak shapes as
/// [`CREDENTIAL_BEARING_TARGETS`] describes); fails if the pins are dropped, set at `debug`, or no
/// longer composed into the file filter.
#[test]
fn a_credential_shaped_url_never_reaches_the_file_layer_at_the_default_level() {
    const BOT_TOKEN: &str = "1234567890:AAHsupersecrettelegramtokenvalue";
    const LISTEN_KEY: &str = "pqia91ma19a5supersecretlistenkeyvalue";
    const VENDOR_KEY: &str = "supersecretvendorapikeyvalue";

    let buf = SharedBuf::default();
    let make_writer = {
        let buf = buf.clone();
        move || buf.clone()
    };
    // EXACTLY what `init` builds for the file layer on a default `LogConfig` (file_level
    // "trace", no `VIKE_LOG_FILE_LEVEL` set).
    let directive = file_level_directive(None, &LogConfig::default().file_level);
    let layer = fmt::layer().json().with_writer(make_writer).with_filter(EnvFilter::new(directive));
    let subscriber = tracing_subscriber::registry().with(layer);

    crate::capture::scoped(subscriber, || {
        // `ureq::run`'s `debug!("{method} {DebugUri}")`, with the query it prints at trace.
        tracing::debug!(
            target: "ureq::run",
            "POST https://api.telegram.org/bot{BOT_TOKEN}/sendMessage",
        );
        tracing::debug!(
            target: "ureq::run",
            "GET https://finnhub.io/api/v1/quote?symbol=AAPL&token={VENDOR_KEY}",
        );
        // `tungstenite::client::connect_to_some`'s ungated `debug!("Trying to contact {uri} …")`
        // — the reason the pin is `info` and not `debug`.
        tracing::debug!(
            target: "tungstenite::client",
            "Trying to contact wss://stream.binance.com:9443/ws/{LISTEN_KEY} at 1.2.3.4:9443...",
        );
        // `tungstenite::handshake::client`'s `trace!("Request: {:?}")`: the whole raw request.
        tracing::trace!(
            target: "tungstenite::handshake::client",
            "Request: \"GET /ws/{LISTEN_KEY} HTTP/1.1\\r\\nAuthorization: Bearer {VENDOR_KEY}\\r\\n\"",
        );
        // ...and OUR OWN crates must still get full trace into the file: asserted, not assumed.
        tracing::trace!(target: "vike_exec::oms", marker = "our_own_trace_survives", "hop");
    });

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).expect("utf8 output");
    for (what, secret) in [
        ("telegram bot token", BOT_TOKEN),
        ("binance listenKey", LISTEN_KEY),
        ("vendor api key", VENDOR_KEY),
    ] {
        assert!(
            !out.contains(secret),
            "the {what} reached the trace FILE layer at the default level — \
                 every app-level redaction is bypassed below this point. Output: {out}"
        );
    }
    assert!(
        out.contains("our_own_trace_survives"),
        "our own crates must keep trace-level file logging — a blanket level drop would have \
             been the cheaper fix and this is what it would have cost. Output: {out}"
    );
}
