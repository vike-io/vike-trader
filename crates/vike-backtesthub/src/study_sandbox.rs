//! **A research study FILE runs in a SANDBOXED child, never in this process** (owner's rule, 2026-11-15: a
//! study is a FILE, built, loaded and sandboxed exactly like a strategy file). This process loads no study
//! and links no loader, as it loads no strategy file (`crate::strategy_sandbox`).
//!
//! - **The route.** A Rust study (`<project>/user_data/research/studies/rust/<name>/`) is run by
//!   [`runner`], the `vike_study_runner::study_dispatch::StudyFileRunner` this crate's two hosts
//!   (`crate::studio_run::run_study_local`, the daemon's `RunStudy`, and `crate::study_cli`) hand the
//!   study dispatch: it reads the study's folder, asks the strategy builder to build it (the builder the
//!   composition root set, [`use_builder`]) and runs the artifact in a child.
//! - **The child** is `vike-backend strategy-sandbox study <builds-dir> <artifact>`
//!   (`crates/vike-strategy-sandbox`, the one crate that links the loader), `<artifact>` the path the
//!   builder answered (`sdk-N/<folder>/<sha256>.so`, decision 0296). It jails itself to that one
//!   artifact, loads it, and answers with [`serve_child`]: it runs the study and relays every request
//!   the study makes up to this process UNREAD.
//! - **This process** answers those requests from the study's context as `run_study_with` built it
//!   (`vike_user_research::file::host::Parent`: the store, the trainer and the study's ONE backtest),
//!   each on a thread of its own so a study's parallel fits stay parallel, and turns the finished
//!   answer back into the study's outcome, which `run_study_with` persists exactly as it always has.
//!   The backtest runs HERE, through `crate::harness_sim::HarnessSim`, so a strategy FILE its profile
//!   names takes the same builder, loader and sandbox as every backtest (the jailed study process can
//!   start no process); the store the study handed it is read back from the study file over the same
//!   channel (`SERVE` down, `SERVED` up).
//!
//! # The channel
//!
//! The child's stdin and stdout, in FRAMES: a `u32` length, a kind, a `u64` request number, a "last"
//! flag, then the bytes; a message longer than [`CHUNK`] goes as several frames, all of one message
//! written under one lock so two messages never interleave. In order: ONE `START` down (the start
//! message), then, interleaved in whatever order they finish: `CALL`s up, each answered by an `ANSWER`
//! down with the same number, and `SERVE`s down (a read of the store the study handed its backtest),
//! each answered by a `SERVED` up; and ONE `DONE` up (the finished answer). Both sides ask and answer
//! through one [`Link`]. What a message MEANS is `vike_user_research::file`'s.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;

use vike_study_runner::study_dispatch::StudyFileRunner;
use vike_user_research::file::host;
use vike_user_research::{StudyContext, StudyError, StudyOutcome};
use vike_utility::errors::CarriedRefusal;
use vike_utility::mutex::lock;

use crate::strategy_sandbox::relay;

/// The loaded study file's run, as `vike_strategy_sandbox::StudyServe` hands it over: the start message
/// and the host's answer slot in, the finished answer out.
pub type StudyFileRun =
    Box<dyn Fn(&[u8], &(dyn Fn(&[u8]) -> Vec<u8> + Sync)) -> Vec<u8> + Send + Sync>;

/// How this process asks the strategy builder for a study's artifact: the study's name and its folder's
/// `.rs` files `(file name, text)` in, the artifact's path under the builds directory out
/// (`sdk-N/<folder>/<sha256>.so`, the builder's answer, decision 0296), or the refusal with its id, rendered
/// once in the study's error. Blocking for as long as the build takes.
pub type StudyBuilder =
    Box<dyn Fn(&str, &[(String, String)]) -> Result<String, CarriedRefusal> + Send + Sync>;

/// The builder this process asks. Set once by the composition root (`crates/vike-backend/src/main.rs`): the
/// builder's client is a crate this one cannot name.
static STUDY_BUILDER: OnceLock<StudyBuilder> = OnceLock::new();

/// Ask `build` for every study this process runs (the first call wins).
pub fn use_builder(build: StudyBuilder) {
    let _ = STUDY_BUILDER.set(build);
}

/// The study dispatch's runner for a Rust study FILE: its folder read, built by the builder, and run in
/// a sandboxed child of this program (the module doc).
pub fn runner() -> StudyFileRunner {
    Arc::new(
        |dir: &Path,
         ctx: &StudyContext,
         params: &toml::Value|
         -> Result<StudyOutcome, StudyError> {
            let (name, files) = read_study(dir)?;
            let build = STUDY_BUILDER.get().ok_or_else(|| {
            StudyError::Study(format!(
                "study {name}: a Rust study file is built by the strategy builder before it runs, \
                 and this program was started without one (run it through `vike-backend`)"
            ))
        })?;
            let artifact = build(&name, &files).map_err(|e| {
                StudyError::Study(format!("study {name} could not be built: {}", e.rendered()))
            })?;
            let program = crate::strategy_sandbox::program().map_err(|e| {
                StudyError::Study(format!(
                    "the study sandbox could not be started: this program's path is unknown ({e})"
                ))
            })?;
            run_in_child(&program, &crate::studio_run::strategy_files_dir(), &artifact, ctx, params)
        },
    )
}

/// This server's study folder, both tiers: `<project>/user_data/research/studies`, the project found
/// from the working directory as the strategy builds are (`crate::studio_run::strategy_files_dir`),
/// `user_data/…` under it when no project is found. The same second walk from a booted root as that
/// function's, with its residual (its doc). Every study the server lists or runs is under it.
pub fn studies_root() -> PathBuf {
    let cwd = std::env::current_dir().ok();
    cwd.as_deref().and_then(vike_paths::state_path::user_studies_dir).unwrap_or_else(|| {
        use vike_paths::state_path::{PROJECT_USER_DATA_DIR, RESEARCH_SUBDIR, STUDIES_SUBDIR};
        Path::new(PROJECT_USER_DATA_DIR).join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR)
    })
}

/// The folder a Rust study called `name` lives in: `<studies_root>/rust/<name>` ([`studies_root`]).
pub fn study_dir(name: &str) -> PathBuf {
    studies_root().join(vike_paths::state_path::RUST_SUBDIR).join(name)
}

/// A study folder's name and its `.rs` files `(file name, text)`, sorted by name: the study the builder
/// builds. The name is checked first (it arrives on the wire, and names a folder).
fn read_study(dir: &Path) -> Result<(String, Vec<(String, String)>), StudyError> {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !vike_paths::state_path::is_rust_study_name(&name) {
        return Err(StudyError::Study(format!(
            "no study called {name:?}: a study's name is its folder's, [a-z][a-z0-9_]*"
        )));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| {
        StudyError::Study(format!(
            "no study called {name:?}: {} could not be read ({e}). A study is a FILE: copy \
             `{name}.rs` into that folder and run it again",
            dir.display()
        ))
    })?;
    let mut files = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        let is_rust = path.extension().is_some_and(|e| e == "rs") && path.is_file();
        let Some(file) = path.file_name().map(|f| f.to_string_lossy().into_owned()) else {
            continue;
        };
        if is_rust {
            let text = std::fs::read_to_string(&path).map_err(|e| {
                StudyError::Study(format!("{}: could not be read — {e}", path.display()))
            })?;
            files.push((file, text));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if !files.iter().any(|(file, _)| *file == format!("{name}.rs")) {
        return Err(StudyError::Study(format!(
            "no study called {name:?}: {} holds no `{name}.rs`, the study's entry file",
            dir.display()
        )));
    }
    Ok((name, files))
}

/// Run the study built into `<builds>/<artifact>` (`sdk-N/<folder>/<sha>.so`, the builder's answer) in a
/// child of `program` (`<program> strategy-sandbox study <builds> <artifact>`), over `ctx` and the recipe
/// `params`: what it asks
/// is answered from `ctx`, and its finished answer is its outcome. A child that cannot start, dies or
/// breaks the channel is a study error naming why, its exit and its last words.
///
/// # Errors
/// The study's own error, variant for variant, or the sandbox's failure as a study error.
pub fn run_in_child(
    program: &Path,
    builds: &Path,
    artifact: &str,
    ctx: &StudyContext,
    params: &toml::Value,
) -> Result<StudyOutcome, StudyError> {
    let start = host::start(ctx, params)?;
    let args = [Path::new("strategy-sandbox"), Path::new("study"), builds, Path::new(artifact)];
    let mut child = vike_sandbox::spawn_strategy_process(program, args)
        .map_err(|e| StudyError::Study(format!("the study sandbox could not be started: {e}")))?;
    let (Some(to_child), Some(mut from_child), Some(reason)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(StudyError::Study("the study sandbox started without its three pipes".into()));
    };
    let child = Mutex::new(child);
    let link = Arc::new(Link::over(Box::new(to_child)));
    // The parent reads the store a study handed its backtest back from the study file: a `SERVE` down.
    let serving = Arc::clone(&link);
    let parent =
        host::Parent::new(ctx, Arc::new(move |request: &[u8]| serving.call(SERVE, request)));
    thread::scope(|scope| {
        let tail = scope.spawn(move || relay::reason_tail(reason));
        let relayed: Result<Vec<u8>, String> = match link.send(START, 0, &start) {
            Err(e) => Err(format!("the study sandbox took no start message ({e})")),
            Ok(()) => loop {
                match read_message(&mut from_child) {
                    Ok((CALL, id, request)) => {
                        let (parent, link) = (&parent, &link);
                        // One thread per request: a study's parallel reads and fits stay parallel. A
                        // panic answers nothing, which the study reads as "no answer".
                        scope.spawn(move || {
                            let answer = catch_unwind(AssertUnwindSafe(|| parent.answer(&request)))
                                .unwrap_or_default();
                            let _ = link.send(ANSWER, id, &answer);
                        });
                    }
                    Ok((SERVED, id, answer)) => link.answered(id, answer),
                    Ok((DONE, _, finished)) => break Ok(finished),
                    Ok((kind, _, _)) => {
                        break Err(format!("the study sandbox sent a frame of kind {kind}"));
                    }
                    Err(e) => {
                        break Err(format!("the study sandbox ended without an answer ({e})"));
                    }
                }
            },
        };
        // End of the channel: the child's reader sees end-of-file and the child exits; a backtest
        // still reading the study's store is answered nothing.
        link.close();
        let status = relay::end(&child, relayed.is_err());
        let tail = tail.join().unwrap_or_default();
        match relayed {
            Ok(finished) => host::finished(&finished),
            Err(why) => {
                let tail = if tail.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", tail.trim())
                };
                Err(StudyError::Study(format!("{why} ({status}){tail}")))
            }
        }
    })
}

/// **The sandboxed child's half**: read the start message from `input`, run the loaded study over it, and
/// write its finished answer to `output`. Every request the study makes is relayed to the parent unread
/// (a `CALL`); every read the parent makes of a store the study handed its backtest (a `SERVE`) is
/// answered by the study file (`serve`, its `vike_study_serve`). `study` is the loaded file, or the
/// jail's or the load's refusal, which becomes the study's error, the jail's id shown once.
///
/// The multicall dispatcher hands this to `vike_strategy_sandbox::run` as its `StudyServe`.
pub fn serve_child(
    mut input: Box<dyn Read + Send>,
    mut output: Box<dyn Write + Send>,
    study: Result<(StudyFileRun, host::Serve), CarriedRefusal>,
) -> ExitCode {
    let start = match read_message(&mut *input) {
        Ok((START, _, start)) => start,
        _ => {
            eprintln!("strategy-sandbox study: no start message on the protocol channel");
            return ExitCode::from(3);
        }
    };
    let (run, serve) = match study {
        Ok(study) => study,
        Err(refusal) => {
            // Rendered HERE, its one id: after this the study's error is text (`StudyError::Study`, the hub's
            // `Response::Error`) and nothing on the way adds an id. A study refusal given an id of its own must
            // carry this one instead.
            let refused = host::refused(&format!(
                "the study file could not be loaded: {}",
                refusal.rendered()
            ));
            return finish(write_message(&mut *output, DONE, 0, &refused));
        }
    };
    let link = Arc::new(Link::over(output));
    let reader = Arc::clone(&link);
    let started = thread::Builder::new()
        .name("study-link".into())
        .spawn(move || child_reads(&reader, input, &serve));
    if let Err(e) = started {
        eprintln!("strategy-sandbox study: the channel's reader could not start: {e}");
        return ExitCode::from(3);
    }
    let host = |request: &[u8]| -> Vec<u8> { link.call(CALL, request) };
    let finished = run(&start, &host);
    finish(link.send(DONE, 0, &finished))
}

/// The child's reader: each `ANSWER` handed to the thread that asked; each `SERVE` answered by the
/// study file on a thread of its own (a store read may itself ask the parent, whose answer this reader
/// must still be free to deliver). When the channel ends, every waiting thread is woken with no answer.
fn child_reads(link: &Arc<Link>, mut input: Box<dyn Read + Send>, serve: &host::Serve) {
    loop {
        match read_message(&mut *input) {
            Ok((ANSWER, id, answer)) => link.answered(id, answer),
            Ok((SERVE, id, request)) => {
                let (answering_link, serve) = (Arc::clone(link), Arc::clone(serve));
                let answering =
                    thread::Builder::new().name("study-serve".into()).spawn(move || {
                        let answer = serve(&request);
                        let _ = answering_link.send(SERVED, id, &answer);
                    });
                if answering.is_err() {
                    // No thread for it: answered nothing, which the parent reads as a failed read.
                    let _ = link.send(SERVED, id, &[]);
                }
            }
            _ => break,
        }
    }
    link.stop_waiting();
}

/// The child's exit once its finished answer was (or could not be) written.
fn finish(sent: io::Result<()>) -> ExitCode {
    match sent {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("strategy-sandbox study: the finished answer could not be written: {e}");
            ExitCode::from(3)
        }
    }
}

/// One side of the channel: its output, and the requests it asked that wait for their answer.
struct Link {
    /// `None` once this side closed its output.
    output: Mutex<Option<Box<dyn Write + Send>>>,
    /// The requests waiting for their answer, by number; `None` once the other side's channel closed.
    waiting: Mutex<Option<HashMap<u64, mpsc::Sender<Vec<u8>>>>>,
    next: AtomicU64,
}

impl Link {
    fn over(output: Box<dyn Write + Send>) -> Self {
        Link {
            output: Mutex::new(Some(output)),
            waiting: Mutex::new(Some(HashMap::new())),
            next: AtomicU64::new(0),
        }
    }

    /// Ask the other side (a `kind` frame); its answer, or empty when the channel is gone (read as "no
    /// answer").
    fn call(&self, kind: u8, request: &[u8]) -> Vec<u8> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answered, answer) = mpsc::channel();
        match lock(&self.waiting).as_mut() {
            Some(waiting) => waiting.insert(id, answered),
            None => return Vec::new(),
        };
        if self.send(kind, id, request).is_err() {
            if let Some(waiting) = lock(&self.waiting).as_mut() {
                waiting.remove(&id);
            }
            return Vec::new();
        }
        answer.recv().unwrap_or_default()
    }

    /// The answer to request `id` this side asked, handed to the thread waiting for it.
    fn answered(&self, id: u64, answer: Vec<u8>) {
        let asked = lock(&self.waiting).as_mut().and_then(|waiting| waiting.remove(&id));
        if let Some(asked) = asked {
            let _ = asked.send(answer);
        }
    }

    fn send(&self, kind: u8, id: u64, payload: &[u8]) -> io::Result<()> {
        match lock(&self.output).as_mut() {
            Some(out) => write_message(&mut **out, kind, id, payload),
            None => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        }
    }

    /// The other side is gone: every waiting thread is woken with no answer, and no new one waits.
    fn stop_waiting(&self) {
        *lock(&self.waiting) = None;
    }

    /// This side is done: its output closes (the other side reads end-of-file) and nothing waits.
    fn close(&self) {
        drop(lock(&self.output).take());
        self.stop_waiting();
    }
}

// ---- the frames ------------------------------------------------------------------------------

/// Parent to child, once: the start message.
const START: u8 = 0;
/// Child to parent: one request of the study's.
const CALL: u8 = 1;
/// Parent to child: the answer to the request of the same number.
const ANSWER: u8 = 2;
/// Child to parent, once: the finished answer.
const DONE: u8 = 3;
/// Parent to child: one read of a store the study handed its backtest.
const SERVE: u8 = 4;
/// Child to parent: the study file's answer to the `SERVE` of the same number.
const SERVED: u8 = 5;

/// The most bytes one frame carries; a longer message goes as several.
const CHUNK: usize = 4 << 20;

/// The most bytes one message may carry, read: a child streaming frames forever stops here.
// shortcut: 2 GiB is far over any measured request (a training matrix); raise it when one is measured above.
const MAX_MESSAGE: usize = 2 << 30;

/// The frame header: length (4), kind (1), number (8), last (1).
const HEADER: usize = 14;

/// One message as frames of at most [`CHUNK`] bytes, flushed.
fn write_message(out: &mut dyn Write, kind: u8, id: u64, message: &[u8]) -> io::Result<()> {
    let mut parts = message.chunks(CHUNK).peekable();
    if parts.peek().is_none() {
        write_frame(out, kind, id, true, &[])?;
    }
    while let Some(part) = parts.next() {
        write_frame(out, kind, id, parts.peek().is_none(), part)?;
    }
    out.flush()
}

fn write_frame(out: &mut dyn Write, kind: u8, id: u64, last: bool, part: &[u8]) -> io::Result<()> {
    let len = u32::try_from(part.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let mut header = [0u8; HEADER];
    header[..4].copy_from_slice(&len.to_be_bytes());
    header[4] = kind;
    header[5..13].copy_from_slice(&id.to_be_bytes());
    header[13] = u8::from(last);
    out.write_all(&header)?;
    out.write_all(part)
}

/// One whole message: its kind, its number and its bytes. A frame over [`CHUNK`] or a message over
/// [`MAX_MESSAGE`] is `InvalidData`; an end of stream is `UnexpectedEof`.
fn read_message(input: &mut dyn Read) -> io::Result<(u8, u64, Vec<u8>)> {
    let mut message = Vec::new();
    loop {
        let mut header = [0u8; HEADER];
        input.read_exact(&mut header)?;
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if len > CHUNK || message.len() + len > MAX_MESSAGE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "a frame over the cap"));
        }
        let at = message.len();
        message.resize(at + len, 0);
        input.read_exact(&mut message[at..])?;
        if header[13] == 1 {
            let id = u64::from_be_bytes(header[5..13].try_into().unwrap_or_default());
            return Ok((header[4], id, message));
        }
    }
}

#[cfg(test)]
#[path = "study_sandbox_tests.rs"]
mod study_sandbox_tests;
