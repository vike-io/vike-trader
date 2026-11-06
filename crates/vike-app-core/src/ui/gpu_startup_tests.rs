use super::*;
use std::path::PathBuf;

fn adapter(backend: Backend, kind: AdapterKind, name: &str) -> AdapterFacts {
    AdapterFacts {
        backend,
        kind,
        name: name.to_string(),
        driver: String::new(),
        driver_info: String::new(),
        surface_ok: Some(true),
    }
}

fn kinds(list: &[AdapterKind]) -> Vec<AdapterFacts> {
    list.iter().map(|k| adapter(Backend::Vulkan, *k, &format!("{k:?}"))).collect()
}

// ── the render target contract ───────────────────────────────────────────────────────────

/// The pin the chart pipeline's hard-coded assumptions stand on. `chart_gpu.rs` builds its
/// pipeline with `RENDER_TARGET.sample_count()` and NO depth/stencil state; egui draws into a
/// multisampled target the moment `multisampling` is above 1, and into a pass with a depth
/// attachment the moment either bit count is non-zero. Either would make wgpu reject every candle
/// draw, so changing a field here is a change to that pipeline, not to this constant alone.
#[test]
fn the_render_target_is_single_sampled_with_no_depth_or_stencil() {
    assert_eq!(RENDER_TARGET.multisampling, 0);
    assert_eq!(RENDER_TARGET.depth_buffer, 0);
    assert_eq!(RENDER_TARGET.stencil_buffer, 0);
    assert_eq!(RENDER_TARGET.sample_count(), 1);
    assert!(!RENDER_TARGET.has_depth_or_stencil());
}

/// The sample count is egui-wgpu's own `msaa_samples.max(1)` for every `multisampling` value, so the
/// pipeline built from it agrees with the pass whatever the target is later set to.
#[test]
fn the_sample_count_is_what_egui_wgpu_renders_with() {
    for (multisampling, want) in [(0u16, 1u32), (1, 1), (2, 2), (4, 4), (8, 8)] {
        let t = RenderTarget { multisampling, depth_buffer: 0, stencil_buffer: 0 };
        assert_eq!(t.sample_count(), want, "multisampling = {multisampling}");
        assert_eq!(t.sample_count(), u32::from(multisampling).max(1));
    }
    for (depth_buffer, stencil_buffer, want) in
        [(0u8, 0u8, false), (24, 0, true), (0, 8, true), (24, 8, true)]
    {
        let t = RenderTarget { multisampling: 0, depth_buffer, stencil_buffer };
        assert_eq!(t.has_depth_or_stencil(), want, "{depth_buffer}/{stencil_buffer}");
    }
}

/// A default install has no "configured" attempt that differs from the library's own: the
/// preference's default IS the library's vsync.
#[test]
fn the_vsync_default_is_the_librarys() {
    assert_eq!(LIBRARY_DEFAULT_VSYNC, vike_config::preferences::DEFAULT_VSYNC);
    assert_eq!(LIBRARY_DEFAULT_VSYNC, vike_config::Preferences::default().vsync);
}

// ── the plan ─────────────────────────────────────────────────────────────────────────────

#[test]
fn windows_tries_vulkan_then_dx12_then_gl_and_software_last() {
    assert_eq!(
        plan_rungs(true, HostOs::Windows),
        [
            Rung::Configured,
            Rung::OnlyBackend(Backend::Vulkan),
            Rung::OnlyBackend(Backend::Dx12),
            Rung::OnlyBackend(Backend::Gl),
            Rung::Software,
        ]
    );
}

#[test]
fn linux_has_no_dx12_rung_and_macos_only_metal() {
    assert_eq!(
        plan_rungs(true, HostOs::Linux),
        [
            Rung::Configured,
            Rung::OnlyBackend(Backend::Vulkan),
            Rung::OnlyBackend(Backend::Gl),
            Rung::Software,
        ]
    );
    assert_eq!(
        plan_rungs(true, HostOs::MacOs),
        [Rung::Configured, Rung::OnlyBackend(Backend::Metal), Rung::Software]
    );
}

/// Vsync off is the one stored value that differs from the library's defaults, so only then is
/// there a rung that takes it away.
#[test]
fn the_library_defaults_rung_exists_only_when_vsync_is_off() {
    let off = plan_rungs(false, HostOs::Linux);
    assert_eq!(off[..2], [Rung::Configured, Rung::LibraryDefaults]);
    assert_eq!(off.len(), plan_rungs(true, HostOs::Linux).len() + 1);
    assert!(!plan_rungs(true, HostOs::Linux).contains(&Rung::LibraryDefaults));
}

/// Whatever the inputs: the configured rung is first, software is last and once, the library
/// defaults appear at most once and straight after the configured rung, and no backend repeats.
#[test]
fn every_plan_starts_configured_and_ends_in_software() {
    for os in [HostOs::Windows, HostOs::Linux, HostOs::MacOs] {
        for vsync in [true, false] {
            let plan = plan_rungs(vsync, os);
            assert_eq!(plan.first(), Some(&Rung::Configured), "{os:?}/{vsync}");
            assert_eq!(plan.last(), Some(&Rung::Software), "{os:?}/{vsync}");
            assert_eq!(plan.iter().filter(|r| **r == Rung::Software).count(), 1);
            assert_eq!(plan.iter().filter(|r| **r == Rung::Configured).count(), 1);
            let defaults = plan.iter().position(|r| *r == Rung::LibraryDefaults);
            assert_eq!(defaults, (!vsync).then_some(1), "{os:?}/{vsync}");
            let backends: Vec<Backend> = plan
                .iter()
                .filter_map(|r| match r {
                    Rung::OnlyBackend(b) => Some(*b),
                    _ => None,
                })
                .collect();
            assert_eq!(backends, os.backends(), "{os:?}/{vsync}");
            assert!(!backends.contains(&Backend::Other));
        }
    }
}

#[test]
fn the_host_os_is_one_of_the_three_and_has_a_backend_to_try() {
    assert!(!HostOs::current().backends().is_empty());
}

// ── choosing an adapter ──────────────────────────────────────────────────────────────────

/// "Prefer Integrated over Cpu": with a software adapter listed FIRST, a hardware rung still takes
/// the integrated GPU, and only the software rung takes the CPU one.
#[test]
fn a_hardware_rung_prefers_integrated_over_cpu_and_only_the_software_rung_takes_cpu() {
    let listed = kinds(&[AdapterKind::Cpu, AdapterKind::Integrated]);
    for power in [PowerPref::High, PowerPref::Low, PowerPref::NoPreference] {
        assert_eq!(choose_adapter(Selection::Hardware(power), &listed), Ok(1), "{power:?}");
    }
    assert_eq!(choose_adapter(Selection::Software, &listed), Ok(0));
}

#[test]
fn a_hardware_rung_never_takes_a_cpu_adapter_even_when_it_is_the_only_one() {
    let listed = kinds(&[AdapterKind::Cpu]);
    let err = choose_adapter(Selection::Hardware(PowerPref::High), &listed).unwrap_err();
    assert!(err.contains("no hardware adapter"), "{err}");
    assert!(err.contains("software renderer (CPU)"), "the listing names what was there: {err}");
}

#[test]
fn the_software_rung_takes_only_a_cpu_adapter() {
    let listed = kinds(&[AdapterKind::Discrete, AdapterKind::Integrated, AdapterKind::Virtual]);
    let err = choose_adapter(Selection::Software, &listed).unwrap_err();
    assert!(err.contains("no software adapter"), "{err}");
    let with_cpu = kinds(&[AdapterKind::Discrete, AdapterKind::Cpu, AdapterKind::Cpu]);
    assert_eq!(choose_adapter(Selection::Software, &with_cpu), Ok(1), "the first CPU adapter");
}

/// wgpu-core's order, minus software: high performance puts discrete first, low power integrated,
/// both put an unclassified device above a virtual one, and no preference keeps the listing order.
#[test]
fn the_power_preference_ranks_as_wgpu_core_does() {
    let listed = kinds(&[
        AdapterKind::Virtual,
        AdapterKind::Other,
        AdapterKind::Integrated,
        AdapterKind::Discrete,
    ]);
    assert_eq!(choose_adapter(Selection::Hardware(PowerPref::High), &listed), Ok(3));
    assert_eq!(choose_adapter(Selection::Hardware(PowerPref::Low), &listed), Ok(2));
    assert_eq!(choose_adapter(Selection::Hardware(PowerPref::NoPreference), &listed), Ok(0));
    let no_gpu = kinds(&[AdapterKind::Virtual, AdapterKind::Other]);
    assert_eq!(choose_adapter(Selection::Hardware(PowerPref::High), &no_gpu), Ok(1));
    assert_eq!(choose_adapter(Selection::Hardware(PowerPref::Low), &no_gpu), Ok(1));
}

#[test]
fn a_tie_goes_to_the_adapter_listed_first() {
    let listed = kinds(&[AdapterKind::Discrete, AdapterKind::Discrete]);
    assert_eq!(choose_adapter(Selection::Hardware(PowerPref::High), &listed), Ok(0));
}

/// An adapter that cannot present to the window is passed over, however good it is — and when
/// that leaves nothing, the sentence says so rather than naming an adapter that would then fail.
#[test]
fn an_adapter_that_cannot_present_to_the_window_is_never_chosen() {
    let mut listed = kinds(&[AdapterKind::Discrete, AdapterKind::Integrated]);
    listed[0].surface_ok = Some(false);
    assert_eq!(choose_adapter(Selection::Hardware(PowerPref::High), &listed), Ok(1));
    listed[1].surface_ok = Some(false);
    let err = choose_adapter(Selection::Hardware(PowerPref::High), &listed).unwrap_err();
    assert!(err.contains("cannot present to this window"), "{err}");
    listed[1].surface_ok = None;
    assert_eq!(
        choose_adapter(Selection::Hardware(PowerPref::High), &listed),
        Ok(1),
        "no surface to ask is not a refusal"
    );

    let mut cpu = kinds(&[AdapterKind::Cpu]);
    cpu[0].surface_ok = Some(false);
    assert!(choose_adapter(Selection::Software, &cpu).is_err());
}

#[test]
fn nothing_listed_is_an_error_that_says_so() {
    for selection in [Selection::Hardware(PowerPref::High), Selection::Software] {
        let err = choose_adapter(selection, &[]).unwrap_err();
        assert!(err.contains("wgpu listed: nothing"), "{err}");
    }
}

// ── what a rung changes ──────────────────────────────────────────────────────────────────

/// Only the first rung puts the stored vsync on the surface; every later one runs on the library's
/// own surface settings. Together with `plan_rungs` leaving the library-defaults rung out when
/// vsync is on, that is what makes "skipped if identical" true.
#[test]
fn only_the_configured_rung_applies_the_stored_vsync() {
    let power = PowerPref::High;
    assert!(Rung::Configured.setup(power).apply_stored_vsync);
    for rung in [Rung::LibraryDefaults, Rung::OnlyBackend(Backend::Vulkan), Rung::Software] {
        assert!(!rung.setup(power).apply_stored_vsync, "{rung:?}");
    }
}

/// The first two rungs leave the backends and the adapter to the library (so `WGPU_BACKEND` and
/// `WGPU_POWER_PREF` still steer them); a backend rung names its one backend and keeps the
/// library's power preference for the hardware choice; the software rung opens every backend and
/// takes a CPU adapter only.
#[test]
fn the_backend_and_software_rungs_restrict_the_backends_and_choose_the_adapter() {
    for rung in [Rung::Configured, Rung::LibraryDefaults] {
        let setup = rung.setup(PowerPref::Low);
        assert_eq!(setup.backends, BackendChoice::Library, "{rung:?}");
        assert_eq!(setup.selection, None, "{rung:?}");
    }
    for power in [PowerPref::High, PowerPref::Low, PowerPref::NoPreference] {
        let gl = Rung::OnlyBackend(Backend::Gl).setup(power);
        assert_eq!(gl.backends, BackendChoice::Only(Backend::Gl));
        assert_eq!(gl.selection, Some(Selection::Hardware(power)));
    }
    let software = Rung::Software.setup(PowerPref::Low);
    assert_eq!(software.backends, BackendChoice::Every);
    assert_eq!(software.selection, Some(Selection::Software));
}

// ── the runner ───────────────────────────────────────────────────────────────────────────

fn gpu_error(n: usize) -> Failure<&'static str> {
    Failure::Gpu(format!("error {n}"))
}

/// A fake runner that fails the first `fail` attempts with a graphics error and then succeeds: the
/// rungs are visited in plan order, a success stops the ladder at once, and every earlier failure
/// is kept with its own words.
#[test]
fn the_ladder_visits_rungs_in_order_and_stops_at_the_first_success() {
    let plan = plan_rungs(false, HostOs::Windows);
    for fail in 0..plan.len() {
        let mut visited = Vec::new();
        let end = run_ladder(&plan, |i, rung| {
            visited.push((i, *rung));
            if i < fail { Err(gpu_error(i)) } else { Ok(()) }
        });
        let want_visited: Vec<(usize, Rung)> =
            plan.iter().copied().enumerate().take(fail + 1).collect();
        assert_eq!(visited, want_visited, "fail = {fail}");
        let want_tried: Vec<Attempt> = plan
            .iter()
            .take(fail)
            .enumerate()
            .map(|(i, r)| Attempt { rung: *r, error: format!("error {i}") })
            .collect();
        assert_eq!(end, LadderEnd::Ran { tried: want_tried }, "fail = {fail}");
    }
}

#[test]
fn a_ladder_that_never_succeeds_is_exhausted_with_every_attempt_in_order() {
    let plan = plan_rungs(true, HostOs::Linux);
    let mut calls = 0;
    let end: LadderEnd<&str> = run_ladder(&plan, |i, _| {
        calls += 1;
        Err(gpu_error(i))
    });
    assert_eq!(calls, plan.len());
    let LadderEnd::Exhausted { tried } = end else { panic!("expected Exhausted, got {end:?}") };
    let rungs: Vec<Rung> = tried.iter().map(|a| a.rung).collect();
    assert_eq!(rungs, plan);
    assert_eq!(tried[2].error, "error 2");
}

/// An error that is not "the graphics did not open" — the application ran, or a window system
/// error — is not retried: the ladder stops with it and keeps what failed before.
#[test]
fn a_fatal_error_stops_the_ladder_without_trying_the_next_rung() {
    let plan = plan_rungs(false, HostOs::Windows);
    let mut calls = 0;
    let end = run_ladder(&plan, |i, _| {
        calls += 1;
        if i == 0 { Err(gpu_error(0)) } else { Err(Failure::Fatal("the app failed")) }
    });
    assert_eq!(calls, 2);
    assert_eq!(
        end,
        LadderEnd::Stopped {
            error: "the app failed",
            tried: vec![Attempt { rung: Rung::Configured, error: "error 0".to_string() }],
        }
    );
}

#[test]
fn an_empty_plan_is_exhausted_without_calling_anything() {
    let end: LadderEnd<&str> = run_ladder(&[], |_, _| panic!("no rung to attempt"));
    assert_eq!(end, LadderEnd::Exhausted { tried: Vec::new() });
}

// ── the words ────────────────────────────────────────────────────────────────────────────

#[test]
fn an_adapter_summary_names_backend_kind_name_and_driver() {
    let mut a = adapter(Backend::Vulkan, AdapterKind::Integrated, "Intel(R) UHD Graphics 620");
    assert_eq!(a.summary(), "Vulkan, integrated GPU, \"Intel(R) UHD Graphics 620\"");
    a.driver = "Intel".to_string();
    assert_eq!(a.summary(), "Vulkan, integrated GPU, \"Intel(R) UHD Graphics 620\", driver: Intel");
    a.driver_info = "31.0.101".to_string();
    assert_eq!(
        a.summary(),
        "Vulkan, integrated GPU, \"Intel(R) UHD Graphics 620\", driver: Intel (31.0.101)"
    );
    a.driver.clear();
    assert!(a.summary().ends_with("driver: (31.0.101)"), "{}", a.summary());
    let bare = adapter(Backend::Dx12, AdapterKind::Cpu, "");
    assert_eq!(bare.summary(), "DX12, software renderer (CPU)");
    assert!(bare.is_software());
    assert!(!a.is_software());
}

#[test]
fn the_log_lines_name_the_attempt_the_rung_and_the_adapter() {
    let a = adapter(Backend::Gl, AdapterKind::Discrete, "GeForce");
    let line = startup_line(2, 5, &Rung::OnlyBackend(Backend::Gl), &a);
    assert_eq!(
        line,
        "GPU startup: attempt 3 of 5 (only the OpenGL backend) opened OpenGL, discrete GPU, \"GeForce\""
    );
    let failed = Attempt { rung: Rung::Software, error: "boom".to_string() };
    assert_eq!(
        attempt_failed_line(4, 5, &failed),
        "GPU startup: attempt 5 of 5 (a software renderer (CPU)) failed: boom"
    );
}

#[test]
fn the_software_warning_is_loud_and_says_what_to_do() {
    assert!(SOFTWARE_RENDERER_WARNING.contains("SOFTWARE"));
    assert!(SOFTWARE_RENDERER_WARNING.contains("driver"));
    assert!(SOFTWARE_RENDERER_WARNING.contains("WGPU_BACKEND"));
}

fn failures() -> Vec<Attempt> {
    vec![
        Attempt { rung: Rung::Configured, error: "WGPU error: no adapter".to_string() },
        Attempt {
            rung: Rung::OnlyBackend(Backend::Vulkan),
            error: "Adapter selection failed: nothing".to_string(),
        },
        Attempt { rung: Rung::Software, error: "device lost".to_string() },
    ]
}

/// The evidence the owner asked for, all in one message: every attempt in order with its own error,
/// the adapters wgpu listed (once each, however many attempts listed them), the log path, and the
/// two switches — with the one that does nothing called out.
#[test]
fn the_failure_message_carries_every_attempt_the_adapters_the_log_and_the_hints() {
    let a = adapter(Backend::Vulkan, AdapterKind::Integrated, "Intel UHD");
    let mut blocked = adapter(Backend::Dx12, AdapterKind::Discrete, "NVIDIA");
    blocked.surface_ok = Some(false);
    let listed = [a.clone(), blocked, a];
    let log = PathBuf::from("C:/proj/settings/state/logs");
    let msg = exhausted_message(&failures(), &listed, Some(&log));

    let first = msg.find("1. the configured settings: WGPU error: no adapter").expect(&msg);
    let second = msg.find("2. only the Vulkan backend: Adapter selection failed: nothing").unwrap();
    let third = msg.find("3. a software renderer (CPU): device lost").unwrap();
    assert!(first < second && second < third, "attempts in order: {msg}");

    assert_eq!(
        msg.matches("Vulkan, integrated GPU, \"Intel UHD\"").count(),
        1,
        "listed once: {msg}"
    );
    assert!(
        msg.contains("DX12, discrete GPU, \"NVIDIA\"  [cannot present to this window]"),
        "{msg}"
    );
    assert!(msg.contains(&log.display().to_string()), "{msg}");
    assert!(msg.contains("WGPU_BACKEND") && msg.contains("WGPU_POWER_PREF"), "{msg}");
    assert!(msg.contains("WGPU_ADAPTER_NAME does nothing"), "{msg}");
}

#[test]
fn the_failure_message_says_so_when_no_adapter_was_listed_and_no_log_directory_is_known() {
    let msg = exhausted_message(&failures(), &[], None);
    assert!(msg.contains("none. The graphics instance or the window surface failed"), "{msg}");
    assert!(msg.contains("`logs` folder beside the executable"), "{msg}");
    assert!(msg.contains("$VIKE_LOG_DIR"), "{msg}");
}
