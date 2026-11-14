//! The thin wgpu/eframe half of the GPU startup ladder. `vike_app_core::ui::gpu_startup` DECIDES —
//! the rungs, the adapter choice, the stop rules, every word printed — and CI runs it; this file
//! only maps those decisions onto `eframe::NativeOptions` and `eframe::run_native`, which no CI job
//! executes (this crate is outside the roster). Keep it that thin: a branch added here is a branch
//! nothing tests.
//!
//! What only a real window proves: that a second `run_native` in the same process opens a window
//! after the first one failed (eframe reuses one thread-local event loop, so it should); that
//! `native_adapter_selector` is reached on every rung that sets it; and what each rung does on a
//! machine whose first choice is broken. See `docs/ops/desktop-gpu.md`'s "Troubleshooting".

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use eframe::egui_wgpu::{self, WgpuSetup};
use eframe::wgpu;
use vike_app_core::ui::gpu_startup::{
    AdapterFacts, AdapterKind, Attempt, Backend, BackendChoice, Failure, HostOs, LadderEnd,
    PowerPref, RENDER_TARGET, Rung, SOFTWARE_RENDERER_WARNING, Selection, attempt_failed_line,
    choose_adapter, exhausted_message, plan_rungs, run_ladder, startup_line,
};

/// The wgpu present mode for the stored `preferences.vsync`. ONLY the two `Auto*` modes: an
/// explicit `Fifo`/`Mailbox`/`Immediate`/`FifoRelaxed` can be unsupported by the adapter, support
/// cannot be asked before the window exists, and wgpu PANICS on an unsupported explicit mode.
pub(crate) fn present_mode(vsync: bool) -> wgpu::PresentMode {
    if vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync }
}

/// The options every attempt starts from: the library's defaults (which read `WGPU_BACKEND` and
/// `WGPU_POWER_PREF` themselves — no code of ours does), the viewport, and the render target
/// `RENDER_TARGET` states EXPLICITLY, because `chart_gpu.rs`'s pipeline is built for exactly that
/// target. `run_and_return` is the library's default and the ladder depends on it: it makes a
/// failed `run_native` return `Err` (and keep its event loop for the next call) instead of
/// exiting the process.
pub(crate) fn base_options(viewport: egui::ViewportBuilder) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport,
        multisampling: RENDER_TARGET.multisampling,
        depth_buffer: RENDER_TARGET.depth_buffer,
        stencil_buffer: RENDER_TARGET.stencil_buffer,
        run_and_return: true,
        ..Default::default()
    }
}

/// Run the app down the ladder; `make_app` builds one attempt's application creator. Returns when
/// the window has been closed after a successful start, or with the error of an attempt that
/// failed for a reason a retry cannot cure. When EVERY rung fails it prints the evidence and exits
/// the process with code 2 (the settings failure's code).
///
/// `log_guards` are the logger's flush guards, handed in so they can be DROPPED before that exit:
/// `process::exit` runs no destructors, and the non-blocking file appender would lose the very
/// message the failure exists to leave in the log.
pub(crate) fn run(
    app_name: &str,
    base: &eframe::NativeOptions,
    vsync: bool,
    log_dir: Option<&Path>,
    log_guards: impl Sized,
    make_app: impl Fn() -> eframe::AppCreator<'static>,
) -> eframe::Result<()> {
    let rungs = plan_rungs(vsync, HostOs::current());
    let total = rungs.len();
    let seen = Arc::new(Mutex::new(Vec::<AdapterFacts>::new()));
    let end = run_ladder(&rungs, |position, rung| {
        tracing::info!(
            "GPU startup: attempt {} of {total}: {} (preferences.vsync = {vsync})",
            position + 1,
            rung.label()
        );
        // The application closure only runs once the graphics are open, so "it ran" is what tells a
        // failure to open the graphics (retry) from a failure of the running app (stop).
        let created = Arc::new(AtomicBool::new(false));
        let (flag, inner, rung_now) = (Arc::clone(&created), make_app(), *rung);
        let creator: eframe::AppCreator<'static> = Box::new(move |cc| {
            flag.store(true, Ordering::SeqCst);
            log_opened(cc.wgpu_render_state.as_ref(), position, total, &rung_now);
            inner(cc)
        });
        match eframe::run_native(app_name, rung_options(base, rung, vsync, &seen), creator) {
            Ok(()) => Ok(()),
            Err(eframe::Error::Wgpu(e)) if !created.load(Ordering::SeqCst) => {
                let failed = Attempt { rung: *rung, error: e.to_string() };
                tracing::warn!("{}", attempt_failed_line(position, total, &failed));
                Err(Failure::Gpu(failed.error))
            }
            Err(e) => Err(Failure::Fatal(e)),
        }
    });
    match end {
        LadderEnd::Ran { .. } => Ok(()),
        LadderEnd::Stopped { error, tried } => {
            tracing::error!(
                "GPU startup stopped by a non-graphics error after {} failed attempt(s): {error}",
                tried.len()
            );
            Err(error)
        }
        LadderEnd::Exhausted { tried } => {
            let listed = seen.lock().map(|a| a.clone()).unwrap_or_default();
            let message = exhausted_message(&tried, &listed, log_dir);
            tracing::error!("{message}");
            eprintln!("{message}");
            drop(log_guards);
            std::process::exit(2)
        }
    }
}

/// One attempt's options: `base`, plus what `Rung::setup` says the rung changes (the decision is
/// `vike-app-core`'s; this only maps it onto the library's types).
fn rung_options(
    base: &eframe::NativeOptions,
    rung: &Rung,
    vsync: bool,
    seen: &Arc<Mutex<Vec<AdapterFacts>>>,
) -> eframe::NativeOptions {
    let mut options = base.clone();
    if let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
        // The library's own preference (it read `WGPU_POWER_PREF`), carried into our selector.
        let setup = rung.setup(power_of(create.power_preference));
        match setup.backends {
            BackendChoice::Library => {}
            BackendChoice::Only(b) => create.instance_descriptor.backends = backends_of(b),
            BackendChoice::Every => {
                create.instance_descriptor.backends =
                    wgpu::Backends::PRIMARY | wgpu::Backends::SECONDARY;
            }
        }
        if let Some(selection) = setup.selection {
            create.native_adapter_selector = Some(selector(selection, Arc::clone(seen)));
        }
        if setup.apply_stored_vsync {
            options.wgpu_options.surface.present_mode = present_mode(vsync);
        }
    }
    options
}

/// The selector a rung that chooses for itself installs: describe what wgpu listed (recording it
/// for the failure message), let the pure decision pick, hand back that adapter.
fn selector(
    selection: Selection,
    seen: Arc<Mutex<Vec<AdapterFacts>>>,
) -> egui_wgpu::NativeAdapterSelectorMethod {
    Arc::new(move |adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>| {
        pick(selection, adapters, surface, &seen)
    })
}

fn pick(
    selection: Selection,
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    seen: &Mutex<Vec<AdapterFacts>>,
) -> Result<wgpu::Adapter, String> {
    let facts: Vec<AdapterFacts> = adapters
        .iter()
        .map(|a| facts_of(&a.get_info(), surface.map(|s| a.is_surface_supported(s))))
        .collect();
    if let Ok(mut all) = seen.lock() {
        all.extend(facts.iter().cloned());
    }
    choose_adapter(selection, &facts).map(|i| adapters[i].clone())
}

/// The adapter the window opened on, at info level, and the loud warning when it is software.
fn log_opened(
    render_state: Option<&egui_wgpu::RenderState>,
    position: usize,
    total: usize,
    rung: &Rung,
) {
    let Some(state) = render_state else { return };
    let adapter = facts_of(&state.adapter.get_info(), None);
    tracing::info!("{}", startup_line(position, total, rung, &adapter));
    if adapter.is_software() {
        tracing::warn!("{SOFTWARE_RENDERER_WARNING}");
    }
}

fn facts_of(info: &wgpu::AdapterInfo, surface_ok: Option<bool>) -> AdapterFacts {
    AdapterFacts {
        backend: backend_from(info.backend),
        kind: kind_from(info.device_type),
        name: info.name.clone(),
        driver: info.driver.clone(),
        driver_info: info.driver_info.clone(),
        surface_ok,
    }
}

fn backend_from(backend: wgpu::Backend) -> Backend {
    match backend {
        wgpu::Backend::Vulkan => Backend::Vulkan,
        wgpu::Backend::Dx12 => Backend::Dx12,
        wgpu::Backend::Metal => Backend::Metal,
        wgpu::Backend::Gl => Backend::Gl,
        wgpu::Backend::Noop | wgpu::Backend::BrowserWebGpu => Backend::Other,
    }
}

fn kind_from(kind: wgpu::DeviceType) -> AdapterKind {
    match kind {
        wgpu::DeviceType::DiscreteGpu => AdapterKind::Discrete,
        wgpu::DeviceType::IntegratedGpu => AdapterKind::Integrated,
        wgpu::DeviceType::VirtualGpu => AdapterKind::Virtual,
        wgpu::DeviceType::Cpu => AdapterKind::Cpu,
        wgpu::DeviceType::Other => AdapterKind::Other,
    }
}

fn backends_of(backend: Backend) -> wgpu::Backends {
    match backend {
        Backend::Vulkan => wgpu::Backends::VULKAN,
        Backend::Dx12 => wgpu::Backends::DX12,
        Backend::Metal => wgpu::Backends::METAL,
        Backend::Gl => wgpu::Backends::GL,
        // No rung forces it (`HostOs::backends` never lists it); an unrestricted set is the
        // harmless answer to the question anyway.
        Backend::Other => wgpu::Backends::PRIMARY | wgpu::Backends::SECONDARY,
    }
}

fn power_of(power: wgpu::PowerPreference) -> PowerPref {
    match power {
        wgpu::PowerPreference::HighPerformance => PowerPref::High,
        wgpu::PowerPreference::LowPower => PowerPref::Low,
        wgpu::PowerPreference::None => PowerPref::NoPreference,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_app_core::ui::gpu_startup::LIBRARY_DEFAULT_VSYNC;

    fn seen() -> Arc<Mutex<Vec<AdapterFacts>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    /// The premise of "rung 2 is skipped when it would repeat rung 1": the library's own default
    /// present mode is the one `preferences.vsync = true` maps to.
    #[test]
    fn vsync_on_is_the_librarys_own_present_mode_and_off_is_the_other_auto_mode() {
        let library = eframe::NativeOptions::default().wgpu_options.surface.present_mode;
        assert_eq!(present_mode(LIBRARY_DEFAULT_VSYNC), library);
        assert_eq!(present_mode(true), wgpu::PresentMode::AutoVsync);
        assert_eq!(present_mode(false), wgpu::PresentMode::AutoNoVsync);
    }

    /// The pin that keeps `chart_gpu.rs`'s pipeline valid: the options carry exactly the render
    /// target its sample count and absent depth/stencil state are derived from.
    #[test]
    fn the_options_carry_the_render_target_the_chart_pipeline_is_built_for() {
        let o = base_options(egui::ViewportBuilder::default());
        assert_eq!(o.multisampling, RENDER_TARGET.multisampling);
        assert_eq!(o.depth_buffer, RENDER_TARGET.depth_buffer);
        assert_eq!(o.stencil_buffer, RENDER_TARGET.stencil_buffer);
        assert_eq!(u32::from(o.multisampling).max(1), RENDER_TARGET.sample_count());
        assert!(o.run_and_return, "a failed attempt must return so the next rung can run");
    }

    /// The mapping of `Rung::setup` (decided and tested in `vike-app-core`) onto the library's
    /// fields: the present mode, the backend set and whether a selector is installed.
    #[test]
    fn a_rungs_setup_lands_in_the_librarys_options() {
        let base = base_options(egui::ViewportBuilder::default());
        let WgpuSetup::CreateNew(library) = base.wgpu_options.wgpu_setup.clone() else {
            panic!("the library's default setup is CreateNew")
        };
        let library_mode = base.wgpu_options.surface.present_mode;
        let landed = |rung: Rung| {
            let o = rung_options(&base, &rung, false, &seen());
            let WgpuSetup::CreateNew(c) = o.wgpu_options.wgpu_setup else {
                panic!("the ladder only builds CreateNew setups")
            };
            (
                o.wgpu_options.surface.present_mode,
                c.instance_descriptor.backends,
                c.native_adapter_selector.is_some(),
            )
        };
        let every = wgpu::Backends::PRIMARY | wgpu::Backends::SECONDARY;
        let kept = library.instance_descriptor.backends;
        let off = wgpu::PresentMode::AutoNoVsync;
        assert_eq!(landed(Rung::Configured), (off, kept, false));
        assert_eq!(landed(Rung::LibraryDefaults), (library_mode, kept, false));
        let vulkan = Rung::OnlyBackend(Backend::Vulkan);
        assert_eq!(landed(vulkan), (library_mode, wgpu::Backends::VULKAN, true));
        assert_eq!(landed(Rung::Software), (library_mode, every, true));
    }

    #[test]
    fn wgpus_kinds_and_backends_map_onto_the_ladders() {
        assert_eq!(kind_from(wgpu::DeviceType::Cpu), AdapterKind::Cpu);
        assert_eq!(kind_from(wgpu::DeviceType::IntegratedGpu), AdapterKind::Integrated);
        assert_eq!(kind_from(wgpu::DeviceType::DiscreteGpu), AdapterKind::Discrete);
        assert_eq!(backend_from(wgpu::Backend::Dx12), Backend::Dx12);
        assert_eq!(backend_from(wgpu::Backend::BrowserWebGpu), Backend::Other);
        assert_eq!(power_of(wgpu::PowerPreference::LowPower), PowerPref::Low);
        assert_eq!(backends_of(Backend::Gl), wgpu::Backends::GL);
    }
}
