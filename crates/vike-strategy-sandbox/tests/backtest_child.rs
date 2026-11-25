//! The backtest verb's SEVERAL artifacts, end to end (decision 0281: a Rust strategy file runs sandboxed with its whole
//! backtest, so a portfolio's `.rs` mounts load in ONE child). A real child of this binary runs the crate's real verb
//! (`vike_strategy_sandbox::run`, `strategy-sandbox backtest <builds> <artifact>...`), jail and all, and proven here:
//!
//! - every artifact it is named is loaded, and its serve function is handed one build per artifact, in argv order,
//!   each of which builds a strategy;
//! - inside that jail a file BESIDE them that it was not named cannot be read, at the path the child names, which
//!   this unjailed parent then reads (the positive control: a wrong path cannot pass);
//! - an artifact path that is a symbolic link (here to a stand-in `vike.db` outside the builds directory) refuses
//!   the whole run by name: nothing loads (`crates/vike-strategy-sandbox/src/lib.rs`'s "The artifact grant");
//! - the builder's cache hit runs no code of the artifact it serves: a file whose initializer (`.init_array`)
//!   writes a marker is built, then served from cache, and the marker is absent; loading it here writes it (the
//!   positive control: the initializer works);
//! - the descriptor ceiling (`vike_sandbox::OPEN_FILES`) holds eight artifacts, MEASURED: the fewest descriptors under
//!   which a jailed process (the real policy over the eight, the protocol's descriptor held as the verb holds it) loads
//!   all of them, and how many the real verb still holds open once they are loaded. Printed as `MEASURE fd ...` lines.
//!
//! The eight are COPIES of one built file under eight names: each is its own inode, so the dynamic loader opens, maps
//! and closes each apart, as it would eight files built from eight texts; one build keeps the run cheap.
//!
//! `harness = false`: this binary is ALSO the child (the verb, and `--fd-floor <n> <artifact>...` for the measure),
//! which the jail requires to start with one thread and no descriptor above stderr, as no libtest harness does.

use std::process::ExitCode;

const NAME: &str = "backtest_child";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(target_os = "linux")]
    {
        if let Some(code) = linux::as_child(&args) {
            return code;
        }
    }
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            println!("{NAME}: test");
        }
        return ExitCode::SUCCESS;
    }
    #[cfg(target_os = "linux")]
    return linux::main();
    #[cfg(not(target_os = "linux"))]
    {
        println!("SKIP: a Rust strategy file runs only on Linux");
        ExitCode::SUCCESS
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::io::Write;
    use std::os::fd::AsFd as _;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use vike_strategy_builder::render;

    /// The build this binary states, as its own child.
    const BUILD: &str = "backtest_child";
    /// The measure's child: `--fd-floor <open files> <artifact>...`.
    const FD_FLOOR: &str = "--fd-floor";
    /// The artifact name a child is never handed, though it sits beside the ones it is: the read that must be refused.
    const UNLISTED: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    /// The SDK folder and build folder every copy sits in.
    const FOLDER: &str = "sdk-dev/0123abcd";

    /// A strategy file that does nothing: what is measured is its loading.
    const IDLE: &str = r#"
use vike_model::params::Params;
use vike_model::{OrderBroker, Strategy};

pub struct Idle;

impl<B: OrderBroker> Strategy<B> for Idle {}

pub fn build<B: OrderBroker + 'static>(_params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(Idle)
}
"#;

    /// The idle file with an initializer that writes the marker at `{MARKER}`: what a builder that `dlopen`s
    /// a cache hit would run.
    const MARKS_ON_LOAD: &str = r#"
use vike_model::params::Params;
use vike_model::{OrderBroker, Strategy};

pub struct Idle;

impl<B: OrderBroker> Strategy<B> for Idle {}

pub fn build<B: OrderBroker + 'static>(_params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(Idle)
}

#[used]
#[unsafe(link_section = ".init_array")]
static MARK_ON_LOAD: extern "C" fn() = mark_on_load;

extern "C" fn mark_on_load() {
    let _ = std::fs::write("{MARKER}", b"the initializer ran");
}
"#;

    /// The loaded file `vike_strategy_sandbox::BacktestServe` hands its serve function.
    type LoadedFile = Box<
        dyn Fn(
                &str,
                &str,
            ) -> Result<
                Box<dyn vike_model::Strategy<vike_simulator::OrderBrokerSimulator>>,
                String,
            > + Send
            + Sync,
    >;

    /// The loaded study file `vike_strategy_sandbox::StudyServe` hands its serve function.
    type LoadedStudy = (
        Box<dyn Fn(&[u8], &(dyn Fn(&[u8]) -> Vec<u8> + Sync)) -> Vec<u8> + Send + Sync>,
        std::sync::Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>,
    );

    /// This binary started as one of its children: the real verb, or the measure's child.
    pub fn as_child(args: &[String]) -> Option<ExitCode> {
        match args.first().map(String::as_str) {
            Some("strategy-sandbox") => {
                Some(vike_strategy_sandbox::run(&args[1..], report_builds, no_study, BUILD))
            }
            Some(FD_FLOOR) => Some(fd_floor(&args[1..])),
            _ => None,
        }
    }

    pub fn main() -> ExitCode {
        // This binary plays the hub: the boot's step 0 first, so no descriptor it inherited (CI's runner hands it
        // pipes) reaches the strategy process, which would refuse it.
        let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
        if let Err(why) = vike_sandbox::probe() {
            println!("SKIP: this box cannot apply the jail ({why}); nothing to prove here");
            return ExitCode::SUCCESS;
        }
        let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        // The live child's build folder: its template's dependencies are compiled there already.
        let strategy_files = tmp.join("live-child-strategy-files");
        let built = build(&strategy_files, IDLE, "other/idle.rs");
        let builds = tmp.join("backtest-child-builds");
        let _ = std::fs::remove_dir_all(&builds);
        std::fs::create_dir_all(builds.join(FOLDER)).expect("the builds folder");
        let names: Vec<String> =
            (0..8).map(|i| format!("{FOLDER}/{}.so", format!("{i:x}").repeat(64))).collect();
        let unlisted = format!("{FOLDER}/{UNLISTED}.so");
        for name in names.iter().chain([&unlisted]) {
            std::fs::copy(&built, builds.join(name)).unwrap_or_else(|e| panic!("copy {name}: {e}"));
        }
        for count in [1, 8] {
            let line = verb(&builds, &names[..count]);
            let expected = format!("handed={count} built={count} unlisted_read=false ");
            assert!(line.starts_with(&expected), "{count} artifact(s): {line}");
            let unlisted =
                line.split(" unlisted_path=").nth(1).and_then(|rest| rest.split(' ').next());
            assert!(
                unlisted.is_some_and(|path| std::fs::read(path).is_ok()),
                "the positive control: the path the jailed child could not read reads here: {line}"
            );
            let open = line.split("open=").nth(1).and_then(|rest| rest.split(' ').next());
            let ceiling = vike_sandbox::OPEN_FILES;
            let loads = |open_files: &u64| floor_child(*open_files, &builds, &names[..count]);
            let floor = (1..=ceiling).find(loads).unwrap_or_else(|| {
                panic!("{count} artifact(s) do not load under {ceiling} descriptors")
            });
            println!(
                "MEASURE fd: {count} artifact(s) load under RLIMIT_NOFILE >= {floor} (ceiling {ceiling}); \
                 the verb holds {} open once they are loaded",
                open.unwrap_or("?")
            );
        }
        a_link_in_the_list_refuses_the_run(&builds, &names[0]);
        a_cache_hit_runs_no_initializer(&strategy_files);
        println!("{}: ok", super::NAME);
        ExitCode::SUCCESS
    }

    /// The real verb named a built artifact and a symbolic link beside it to a stand-in `vike.db` OUTSIDE the
    /// builds directory: the jail refuses the run, naming the link, and nothing loads.
    fn a_link_in_the_list_refuses_the_run(builds: &Path, good: &str) {
        let secret = builds.with_file_name("backtest-child-vike.db");
        std::fs::write(&secret, b"pretend-credential").expect("the stand-in secret");
        let link = format!("{FOLDER}/{}.so", "e".repeat(64));
        let _ = std::fs::remove_file(builds.join(&link));
        std::os::unix::fs::symlink(&secret, builds.join(&link)).expect("the link");
        let line = verb(builds, &[good.to_string(), link.clone()]);
        assert!(line.starts_with("handed=2 built=0 "), "a link in the list loads nothing: {line}");
        assert!(
            line.contains(&format!("the artifact {link} under ")) && line.contains(" is refused: "),
            "the refusal names the link: {line}"
        );
    }

    /// The builder's cache hit runs no code of the artifact it serves (`vike_strategy_builder`'s `render`
    /// reads its compatibility record): a file whose initializer writes a marker, built and then served from
    /// the cache, leaves no marker; loaded here, it writes one.
    fn a_cache_hit_runs_no_initializer(strategy_files: &Path) {
        let marker = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("initializer-ran-marker");
        let text = MARKS_ON_LOAD.replace("{MARKER}", &marker.display().to_string());
        let _ = std::fs::remove_file(&marker);
        let built = build(strategy_files, &text, "other/marks_on_load.rs");
        let served = build(strategy_files, &text, "other/marks_on_load.rs");
        assert_eq!(served, built, "the second build of one text is a cache hit");
        assert!(!marker.exists(), "the builder ran the initializer of the artifact it served");
        vike_rust_strategy::loader::load(&served).expect("the marked file loads");
        assert!(marker.exists(), "the positive control: loading the file runs its initializer");
    }

    /// The real verb over `names` under `builds`: its one line.
    fn verb(builds: &Path, names: &[String]) -> String {
        let me = std::env::current_exe().expect("this binary's path");
        let args: Vec<OsString> = ["strategy-sandbox", "backtest"]
            .into_iter()
            .map(OsString::from)
            .chain([builds.as_os_str().to_owned()])
            .chain(names.iter().map(OsString::from))
            .collect();
        let out = vike_sandbox::spawn_strategy_process(&me, args)
            .expect("spawn the verb")
            .wait_with_output()
            .expect("the verb ends");
        let said = String::from_utf8_lossy(&out.stdout).into_owned();
        let why = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "the verb failed ({:?}): {said} {why}", out.status);
        said
    }

    /// Whether a jailed child of `open_files` descriptors loads every one of `names` under `builds`.
    fn floor_child(open_files: u64, builds: &Path, names: &[String]) -> bool {
        let me = std::env::current_exe().expect("this binary's path");
        let args: Vec<OsString> =
            [OsString::from(FD_FLOOR), OsString::from(open_files.to_string())]
                .into_iter()
                .chain([builds.as_os_str().to_owned()])
                .chain(names.iter().map(OsString::from))
                .collect();
        vike_sandbox::spawn_strategy_process(&me, args)
            .expect("spawn the measure")
            .wait_with_output()
            .expect("the measure ends")
            .status
            .success()
    }

    /// The file `text` (named `path` under `strategies/`) built by the real builder into `dir`: its artifact's path.
    fn build(dir: &Path, text: &str, path: &str) -> PathBuf {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let env = render::child_env(&std::env::vars().collect());
        render::build_strategy_file(
            text,
            path,
            dir,
            &workspace,
            &cargo,
            &env,
            render::Profile::of_this_binary(),
            &render::CargoHomePin::disabled(),
        )
        .unwrap_or_else(|e| panic!("the template must build {path}:\n{e}"))
    }

    /// The descriptors this process may still open: each a copy of stdin, held until the ceiling refuses one.
    fn free_descriptors() -> u64 {
        let mut held = Vec::new();
        while let Ok(fd) = std::io::stdin().as_fd().try_clone_to_owned() {
            held.push(fd);
        }
        held.len() as u64
    }

    /// The serve function the verb hands what it loaded, INSIDE the jail: one line on the protocol's output, how many
    /// builds it was handed, how many loaded and built a strategy, whether the unlisted file beside them read (and its
    /// path, for the parent's positive control), and how many descriptors are open.
    fn report_builds(
        _input: Box<dyn std::io::Read + Send>,
        mut output: Box<dyn Write + Send>,
        _identity: &str,
        builds: Vec<(Result<LoadedFile, vike_utility::errors::CarriedRefusal>, Option<u32>)>,
    ) -> ExitCode {
        let built = builds
            .iter()
            .filter(|(build, _)| build.as_ref().is_ok_and(|build| build("", "{}").is_ok()))
            .count();
        let refusals: Vec<String> = builds
            .iter()
            .filter_map(|(build, _)| build.as_ref().err().map(ToString::to_string))
            .collect();
        // `strategy-sandbox backtest <builds> ...`: argv is in memory, so reading it opens nothing.
        let unlisted_path = std::env::args()
            .nth(3)
            .map(|builds| Path::new(&builds).join(format!("{FOLDER}/{UNLISTED}.so")))
            .unwrap_or_default();
        let unlisted = std::fs::read(&unlisted_path).is_ok();
        // A refused run is not jailed: no ceiling to count descriptors against.
        let open = if built == 0 { 0 } else { vike_sandbox::OPEN_FILES - free_descriptors() };
        let line = format!(
            "handed={} built={built} unlisted_read={unlisted} unlisted_path={} open={open} \
             refusals={refusals:?}\n",
            builds.len(),
            unlisted_path.display()
        );
        match output.write_all(line.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(4),
        }
    }

    fn no_study(
        _input: Box<dyn std::io::Read + Send>,
        _output: Box<dyn Write + Send>,
        _study: Result<LoadedStudy, vike_utility::errors::CarriedRefusal>,
    ) -> ExitCode {
        ExitCode::from(2)
    }

    /// The measure's child (`--fd-floor <open files> <builds> <name>...`): the real policy over the names under
    /// `<builds>` with `RLIMIT_NOFILE` = `<open files>`, the protocol's descriptor held as the verb holds it while it
    /// loads (its `protocol_output`), then every artifact loaded. Exit 0 when all load.
    fn fd_floor(args: &[String]) -> ExitCode {
        let [open_files, builds, names @ ..] = args else {
            return ExitCode::from(2);
        };
        let Ok(open_files) = open_files.parse::<u64>() else {
            return ExitCode::from(2);
        };
        let names: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
        let mut policy = vike_sandbox::strategy_process_policy(Path::new(builds), &names);
        policy.address_space_bytes = None;
        policy.open_files = open_files;
        if let Err(why) = policy.apply() {
            eprintln!("no jail: {why}");
            return ExitCode::from(3);
        }
        let Ok(_protocol) = std::io::stdout().as_fd().try_clone_to_owned() else {
            return ExitCode::from(1);
        };
        for artifact in names.iter().map(|name| Path::new(builds).join(name)) {
            if let Err(e) = vike_rust_strategy::loader::load(&artifact) {
                eprintln!("{}: {e}", artifact.display());
                return ExitCode::from(1);
            }
        }
        ExitCode::SUCCESS
    }
}
