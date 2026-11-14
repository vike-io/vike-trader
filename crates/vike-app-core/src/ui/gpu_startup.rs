//! The desktop's GPU STARTUP LADDER: every decision about bringing the graphics up that needs no
//! wgpu or eframe type, so CI runs it. The thin mapping onto those two crates is
//! `crates/vike-desktop/src/gpu_startup.rs`; this file is data in, decisions out.
//!
//! # What it is for
//!
//! `eframe::run_native` fails with `eframe::Error::Wgpu(..)` BEFORE the application closure runs when
//! the graphics device cannot be opened: no adapter the window can present to, a driver that
//! refuses the device request, a surface that cannot be created. On a machine where the library's
//! first choice is the broken one, that was the end of the program with one library sentence. The
//! ladder tries again with less and less assumed, says so in the log at every step, and when every
//! rung fails it exits with the evidence: each attempt's error, the adapters wgpu listed, where the
//! log is and the two switches that steer the choice by hand.
//!
//! # The rungs, in order ([`plan_rungs`])
//!
//! 1. [`Rung::Configured`] — the settings as stored (`preferences.vsync`) on top of the library's
//!    own defaults, which include its `WGPU_BACKEND` / `WGPU_POWER_PREF` environment switches.
//! 2. [`Rung::LibraryDefaults`] — the library's defaults alone. SKIPPED when rung 1 already is
//!    that, which is exactly when vsync is on (the library default, [`LIBRARY_DEFAULT_VSYNC`]).
//! 3. [`Rung::OnlyBackend`] — one backend at a time ([`HostOs::backends`]), the adapter chosen by
//!    [`choose_adapter`] with software adapters excluded.
//! 4. [`Rung::Software`] — a CPU adapter, and nothing else is accepted. Slow, so it is last and
//!    the caller warns loudly ([`SOFTWARE_RENDERER_WARNING`]).
//!
//! Then [`exhausted_message`] and exit code 2, which is the settings failure's and the thin-build
//! guard's before it (the caller spells it, as they do).
//!
//! # What the ladder does NOT cover
//!
//! * A PANIC. wgpu's default error handler panics on an uncaptured validation or device-lost error;
//!   `Err` is the only failure this module sees, and it sees only those the library returns
//!   before the application is created ([`Failure::Gpu`]). A crash in the driver stays a crash.
//! * An error AFTER the application was created: retrying would build the application a second
//!   time (threads, sockets, a second window state), so the caller reports it as [`Failure::Fatal`].
//! * A second `run_native` in one process is plausible (eframe reuses one thread-local event loop
//!   so a window can be closed and reopened) but UNPROVEN on every platform this ships on; if the
//!   retry itself fails for another reason it surfaces as [`LadderEnd::Stopped`] with both errors in
//!   the log.
//!
//! # The render target contract
//!
//! [`RENDER_TARGET`] is what `eframe::NativeOptions` is told about the colour/depth/stencil target
//! egui draws into, and the sample count `chart_gpu.rs`'s pipeline is built with comes from the same
//! value ([`RenderTarget::sample_count`]). They must agree or wgpu rejects the draw
//! (`IncompatibleSampleCount`), and the chart pipeline has no depth/stencil state, so a depth or
//! stencil attachment on egui's pass would be rejected as well.

use std::fmt::Write as _;
use std::path::Path;

/// What the GUI library does about vertical sync when nobody configures it: on (wgpu's
/// `AutoVsync`, from eframe's own default surface configuration). Held equal to
/// `vike_config::preferences::DEFAULT_VSYNC` by a test, so a default install never has a "configured"
/// attempt that differs from the library's own.
pub const LIBRARY_DEFAULT_VSYNC: bool = true;

/// The colour/depth/stencil target egui draws into, as `eframe::NativeOptions` spells it.
///
/// A struct rather than three numeric `const`s so the three travel together and the chart pipeline
/// and the window options read one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTarget {
    /// `NativeOptions::multisampling`: `0` turns MSAA off. Anything above 1 makes egui render into a
    /// multisampled texture and resolve it, which a pipeline built with `count: 1` cannot draw into.
    pub multisampling: u16,
    /// `NativeOptions::depth_buffer`, in bits.
    pub depth_buffer: u8,
    /// `NativeOptions::stencil_buffer`, in bits.
    pub stencil_buffer: u8,
}

impl RenderTarget {
    /// The sample count of the target a custom pipeline must be built for: egui-wgpu's own
    /// `msaa_samples.max(1)`.
    pub const fn sample_count(&self) -> u32 {
        if self.multisampling == 0 { 1 } else { self.multisampling as u32 }
    }

    /// Whether egui's render pass carries a depth or stencil attachment, which a pipeline with
    /// `depth_stencil: None` cannot be used in.
    pub const fn has_depth_or_stencil(&self) -> bool {
        self.depth_buffer != 0 || self.stencil_buffer != 0
    }
}

/// The one render target this app runs on: single-sampled, no depth, no stencil. egui draws flat
/// 2D with its own feathering, and `chart_gpu.rs`'s candle pipeline is built for exactly this.
/// Changing any field is a change to that pipeline too.
pub const RENDER_TARGET: RenderTarget =
    RenderTarget { multisampling: 0, depth_buffer: 0, stencil_buffer: 0 };

/// A graphics API, as the ladder names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Vulkan,
    Dx12,
    Metal,
    Gl,
    /// A backend no rung ever forces (WebGPU, wgpu's no-op test backend): only listed.
    Other,
}

impl Backend {
    /// The name as it is printed.
    pub const fn label(self) -> &'static str {
        match self {
            Backend::Vulkan => "Vulkan",
            Backend::Dx12 => "DX12",
            Backend::Metal => "Metal",
            Backend::Gl => "OpenGL",
            Backend::Other => "other",
        }
    }
}

/// What kind of device an adapter is (wgpu's `DeviceType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Discrete,
    Integrated,
    Virtual,
    Other,
    /// Software rendering on the CPU (WARP, lavapipe, llvmpipe).
    Cpu,
}

impl AdapterKind {
    /// The name as it is printed.
    pub const fn label(self) -> &'static str {
        match self {
            AdapterKind::Discrete => "discrete GPU",
            AdapterKind::Integrated => "integrated GPU",
            AdapterKind::Virtual => "virtual GPU",
            AdapterKind::Other => "unclassified device",
            AdapterKind::Cpu => "software renderer (CPU)",
        }
    }
}

/// One adapter wgpu listed, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterFacts {
    pub backend: Backend,
    pub kind: AdapterKind,
    pub name: String,
    pub driver: String,
    pub driver_info: String,
    /// Whether the adapter can present to the window's surface; `None` when no surface was at hand
    /// to ask. `Some(false)` is never chosen.
    pub surface_ok: Option<bool>,
}

impl AdapterFacts {
    /// `Vulkan, integrated GPU, "Intel(R) UHD Graphics", driver: Intel (31.0.101)`: what a log line
    /// and the failure message print for an adapter.
    pub fn summary(&self) -> String {
        let mut s = format!("{}, {}", self.backend.label(), self.kind.label());
        if !self.name.is_empty() {
            let _ = write!(s, ", {:?}", self.name);
        }
        match (self.driver.is_empty(), self.driver_info.is_empty()) {
            (false, false) => {
                let _ = write!(s, ", driver: {} ({})", self.driver, self.driver_info);
            }
            (false, true) => {
                let _ = write!(s, ", driver: {}", self.driver);
            }
            (true, false) => {
                let _ = write!(s, ", driver: ({})", self.driver_info);
            }
            (true, true) => {}
        }
        s
    }

    /// Whether this adapter renders on the CPU.
    pub fn is_software(&self) -> bool {
        self.kind == AdapterKind::Cpu
    }
}

/// The library's own adapter preference (`WGPU_POWER_PREF` or its default), read back out of the
/// options the library built rather than from the environment: a selector that replaces the
/// library's choice must not silently drop the preference the operator set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerPref {
    /// Discrete GPUs first.
    High,
    /// Integrated GPUs first.
    Low,
    /// Whatever order the backend listed them in.
    NoPreference,
}

/// The operating system, for the one thing that differs: which backends exist to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Windows,
    Linux,
    MacOs,
}

impl HostOs {
    /// The OS this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            HostOs::Windows
        } else if cfg!(target_os = "macos") {
            HostOs::MacOs
        } else {
            HostOs::Linux
        }
    }

    /// The backends worth forcing one at a time, in the order they are tried.
    pub const fn backends(self) -> &'static [Backend] {
        match self {
            HostOs::Windows => &[Backend::Vulkan, Backend::Dx12, Backend::Gl],
            HostOs::Linux => &[Backend::Vulkan, Backend::Gl],
            HostOs::MacOs => &[Backend::Metal],
        }
    }
}

/// One step of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// The stored settings over the library's defaults.
    Configured,
    /// The library's defaults alone.
    LibraryDefaults,
    /// Exactly this backend, hardware adapters only.
    OnlyBackend(Backend),
    /// A CPU adapter, any backend.
    Software,
}

impl Rung {
    /// The rung as a log line and the failure message print it.
    pub fn label(&self) -> String {
        match self {
            Rung::Configured => "the configured settings".to_string(),
            Rung::LibraryDefaults => "the graphics library's own defaults".to_string(),
            Rung::OnlyBackend(b) => format!("only the {} backend", b.label()),
            Rung::Software => "a software renderer (CPU)".to_string(),
        }
    }

    /// What this rung changes in the options the library built. `power` is the library's own
    /// adapter preference, which a rung that chooses the adapter itself keeps honouring.
    pub fn setup(&self, power: PowerPref) -> RungSetup {
        match self {
            Rung::Configured => RungSetup {
                apply_stored_vsync: true,
                backends: BackendChoice::Library,
                selection: None,
            },
            Rung::LibraryDefaults => RungSetup {
                apply_stored_vsync: false,
                backends: BackendChoice::Library,
                selection: None,
            },
            Rung::OnlyBackend(b) => RungSetup {
                apply_stored_vsync: false,
                backends: BackendChoice::Only(*b),
                selection: Some(Selection::Hardware(power)),
            },
            Rung::Software => RungSetup {
                apply_stored_vsync: false,
                backends: BackendChoice::Every,
                selection: Some(Selection::Software),
            },
        }
    }
}

/// What a [`Rung`] changes in the options the library built (`NativeOptions::default()`, which has
/// already read the library's own environment switches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RungSetup {
    /// Whether the stored `preferences.vsync` is put on the surface. Only the first rung does:
    /// every later one runs on the library's own surface settings, the least that can go wrong.
    pub apply_stored_vsync: bool,
    /// Which backends the instance may use.
    pub backends: BackendChoice,
    /// How the rung picks the adapter itself, or `None` to leave that to the library's
    /// power-preference request.
    pub selection: Option<Selection>,
}

/// Which backends an attempt's instance may enumerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// Whatever the library chose (its default set, or `WGPU_BACKEND`).
    Library,
    /// Exactly this one — overriding `WGPU_BACKEND`, which is the point of trying them one at a time.
    Only(Backend),
    /// Every backend the platform could offer, so a software adapter on any of them is found.
    Every,
}

/// The rungs to try, in order, for a stored `vsync` on `os`.
///
/// Rung 2 appears only when rung 1 differs from it, i.e. when vsync is off: with the library's own
/// default stored there is nothing "configured" to take away.
pub fn plan_rungs(vsync: bool, os: HostOs) -> Vec<Rung> {
    let mut rungs = vec![Rung::Configured];
    if vsync != LIBRARY_DEFAULT_VSYNC {
        rungs.push(Rung::LibraryDefaults);
    }
    rungs.extend(os.backends().iter().map(|b| Rung::OnlyBackend(*b)));
    rungs.push(Rung::Software);
    rungs
}

/// How a rung that chooses for itself picks among the adapters wgpu listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// A hardware adapter ranked by the power preference; a software one is never taken.
    Hardware(PowerPref),
    /// A software adapter, and nothing else.
    Software,
}

/// wgpu-core's own ranking for the adapter kinds it would pick (lower is better), minus software:
/// `HighPerformance` prefers discrete over integrated, `LowPower` the reverse, an unclassified
/// device ranks below both and a virtual one below that. `None` for a CPU adapter, which only
/// [`Selection::Software`] may take.
fn hardware_rank(kind: AdapterKind, power: PowerPref) -> Option<u8> {
    if power == PowerPref::NoPreference {
        return (kind != AdapterKind::Cpu).then_some(0);
    }
    let prefer_integrated = power == PowerPref::Low;
    match kind {
        AdapterKind::Discrete => Some(if prefer_integrated { 2 } else { 1 }),
        AdapterKind::Integrated => Some(if prefer_integrated { 1 } else { 2 }),
        AdapterKind::Other => Some(3),
        AdapterKind::Virtual => Some(4),
        AdapterKind::Cpu => None,
    }
}

/// The index of the adapter `selection` takes out of `adapters`, or the sentence that says why none
/// qualifies. An adapter that cannot present to the window (`surface_ok == Some(false)`) never
/// qualifies; ties go to the earlier-listed adapter, as wgpu's own stable sort does.
///
/// The `Err` text becomes the library's "Adapter selection failed: …" and is what the failure
/// message shows for the attempt, so it names what was listed.
pub fn choose_adapter(selection: Selection, adapters: &[AdapterFacts]) -> Result<usize, String> {
    let usable = |a: &AdapterFacts| a.surface_ok != Some(false);
    let picked = match selection {
        Selection::Hardware(power) => adapters
            .iter()
            .enumerate()
            .filter(|(_, a)| usable(a))
            .filter_map(|(i, a)| hardware_rank(a.kind, power).map(|r| (r, i)))
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, i)| i),
        Selection::Software => adapters.iter().position(|a| a.is_software() && usable(a)),
    };
    picked.ok_or_else(|| {
        let wanted = match selection {
            Selection::Hardware(_) => {
                "no hardware adapter on this backend can present to the window"
            }
            Selection::Software => "no software adapter that can present to the window was listed",
        };
        format!("{wanted} (wgpu listed: {})", list_adapters(adapters))
    })
}

fn list_adapters(adapters: &[AdapterFacts]) -> String {
    if adapters.is_empty() {
        return "nothing".to_string();
    }
    let parts: Vec<String> = adapters
        .iter()
        .map(|a| {
            let presentable = match a.surface_ok {
                Some(false) => " [cannot present to this window]",
                _ => "",
            };
            format!("{{{}}}{presentable}", a.summary())
        })
        .collect();
    parts.join(", ")
}

/// One failed attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub rung: Rung,
    /// The library's own words for why the graphics did not open.
    pub error: String,
}

/// Why an attempt did not run the application.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure<E> {
    /// The graphics did not open and the application closure never ran: the next rung may succeed.
    Gpu(String),
    /// Anything else — the application ran and failed, or the error is not a graphics one. A retry
    /// cannot cure it (or would run the application twice), so the ladder stops with it.
    Fatal(E),
}

/// How the ladder ended.
#[derive(Debug, PartialEq, Eq)]
pub enum LadderEnd<E> {
    /// An attempt ran the application to its end (the window was closed). `tried` are the attempts
    /// that failed before it.
    Ran { tried: Vec<Attempt> },
    /// An attempt failed in a way a retry must not repeat; `error` is that attempt's own.
    Stopped { error: E, tried: Vec<Attempt> },
    /// Every rung failed to open the graphics.
    Exhausted { tried: Vec<Attempt> },
}

/// Run `attempt` down `rungs` until one stops the ladder: `Ok` (the application ran), a
/// [`Failure::Fatal`], or the end of the list. `attempt` gets the rung's position and the rung.
pub fn run_ladder<E>(
    rungs: &[Rung],
    mut attempt: impl FnMut(usize, &Rung) -> Result<(), Failure<E>>,
) -> LadderEnd<E> {
    let mut tried = Vec::new();
    for (i, rung) in rungs.iter().enumerate() {
        match attempt(i, rung) {
            Ok(()) => return LadderEnd::Ran { tried },
            Err(Failure::Gpu(error)) => tried.push(Attempt { rung: *rung, error }),
            Err(Failure::Fatal(error)) => return LadderEnd::Stopped { error, tried },
        }
    }
    LadderEnd::Exhausted { tried }
}

/// The loud line when the window runs on a CPU renderer, whichever rung got it there.
pub const SOFTWARE_RENDERER_WARNING: &str = "GPU STARTUP: this window is drawn by a SOFTWARE \
     renderer on the CPU. Everything works but the chart and every other window will be slow and \
     the CPU busy. Install or update the graphics driver, or set WGPU_BACKEND / WGPU_POWER_PREF \
     (see docs/ops/desktop-gpu.md, Troubleshooting).";

/// The log line for the adapter the window runs on.
pub fn startup_line(position: usize, of: usize, rung: &Rung, adapter: &AdapterFacts) -> String {
    format!(
        "GPU startup: attempt {} of {} ({}) opened {}",
        position + 1,
        of,
        rung.label(),
        adapter.summary()
    )
}

/// The log line for an attempt that failed and is about to be followed by another.
pub fn attempt_failed_line(position: usize, of: usize, attempt: &Attempt) -> String {
    format!(
        "GPU startup: attempt {} of {} ({}) failed: {}",
        position + 1,
        of,
        attempt.rung.label(),
        attempt.error
    )
}

/// The text printed to stderr and logged when every rung failed: each attempt's error in order, the
/// adapters wgpu listed (across all attempts, once each), where the log is, and the two switches
/// that steer the choice by hand.
///
/// `log_dir` is the best-known log directory: the explicit `config.log_dir` row, else the project's
/// log home, else `None` — the logger then falls back to a `logs` folder beside the executable, and
/// `$VIKE_LOG_DIR`, when set, beats all three.
pub fn exhausted_message(
    tried: &[Attempt],
    adapters: &[AdapterFacts],
    log_dir: Option<&Path>,
) -> String {
    let mut out = String::from(
        "vike-desktop could not open a graphics device, so it cannot show a window.\n\n\
         Every startup attempt failed, in order:\n",
    );
    for (i, a) in tried.iter().enumerate() {
        let _ = writeln!(out, "  {}. {}: {}", i + 1, a.rung.label(), a.error);
    }
    out.push_str("\nAdapters wgpu listed:\n");
    let mut seen: Vec<&AdapterFacts> = Vec::new();
    for a in adapters {
        if !seen.contains(&a) {
            seen.push(a);
        }
    }
    if seen.is_empty() {
        out.push_str(
            "  none. The graphics instance or the window surface failed before any adapter was \
             listed: usually a missing or broken graphics driver.\n",
        );
    }
    for a in seen {
        let presentable = match a.surface_ok {
            Some(false) => "  [cannot present to this window]",
            _ => "",
        };
        let _ = writeln!(out, "  - {}{presentable}", a.summary());
    }
    match log_dir {
        Some(dir) => {
            let _ = writeln!(out, "\nLog files, with the same errors and more: {}", dir.display());
        }
        None => out.push_str(
            "\nLog files, with the same errors and more: the `logs` folder beside the \
             executable (or the directory $VIKE_LOG_DIR names).\n",
        ),
    }
    out.push_str(
        "\nThings to try:\n\
         \x20 - Update the graphics driver, then start again.\n\
         \x20 - WGPU_BACKEND=vulkan, dx12 or gl restricts the graphics API wgpu may use (a comma list \
         is accepted); WGPU_POWER_PREF=low, high or none picks the integrated or the discrete GPU \
         first. Set them in the shell that starts the app. They belong to the graphics library, \
         not to vike's settings, and WGPU_ADAPTER_NAME does nothing in this app.\n\
         \x20 - A crash inside the driver is not caught by these retries; the log shows how far it got.\n",
    );
    out
}

#[path = "gpu_startup_tests.rs"]
#[cfg(test)]
mod gpu_startup_tests;
