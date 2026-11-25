//! The study byte boundary, both halves in one process: the exports a study file's template writes
//! (`run_export`, `serve_export`) bound into a [`StudyFileLoaded`] the way the loader binds them.

use super::*;

/// A study that asks its host once and appends to the answer.
extern "C" fn asking_run(host: StudyHostAbi, input: BytesAbi, output: SinkAbi) {
    run_export(host, input, output, |link, input| {
        let mut answer = link.call(&input);
        answer.extend_from_slice(b"|done");
        answer
    });
}

/// A study whose objects answer a request with its bytes reversed.
extern "C" fn reversing_serve(request: BytesAbi, output: SinkAbi) {
    serve_export(request, output, |request| request.into_iter().rev().collect());
}

extern "C" fn panicking_run(host: StudyHostAbi, input: BytesAbi, output: SinkAbi) {
    run_export(host, input, output, |_, _| panic!("a study that panics"));
}

extern "C" fn panicking_serve(request: BytesAbi, output: SinkAbi) {
    serve_export(request, output, |_| panic!("a study object that panics"));
}

fn loaded(run: StudyRunFn, serve: StudyServeFn) -> StudyFileLoaded {
    StudyFileLoaded { path: PathBuf::new(), run, serve }
}

#[test]
fn a_study_asks_its_host_and_its_answer_comes_back() {
    let study = loaded(asking_run, reversing_serve);
    let host = |request: &[u8]| {
        let mut answer = b"host saw ".to_vec();
        answer.extend_from_slice(request);
        answer
    };
    assert_eq!(study.run(b"start", &host), b"host saw start|done");
    assert_eq!(study.serve(b"abc"), b"cba");
    assert_eq!(study.serve(b""), b"", "an empty request crosses as empty");
}

#[test]
fn a_panic_on_either_side_answers_nothing_and_never_unwinds_across() {
    let host = |_: &[u8]| -> Vec<u8> { panic!("a host that panics") };
    // The host's panic is caught in its call slot: the study sees an empty answer and goes on.
    assert_eq!(loaded(asking_run, reversing_serve).run(b"x", &host), b"|done");
    let quiet = |_: &[u8]| Vec::new();
    assert!(loaded(panicking_run, panicking_serve).run(b"x", &quiet).is_empty());
    assert!(loaded(panicking_run, panicking_serve).serve(b"x").is_empty());
}

#[test]
fn the_study_may_call_its_host_from_its_own_threads() {
    extern "C" fn threaded_run(host: StudyHostAbi, input: BytesAbi, output: SinkAbi) {
        run_export(host, input, output, |link, _| {
            let answers: Vec<Vec<u8>> = std::thread::scope(|s| {
                let workers: Vec<_> = (0u8..8).map(|i| s.spawn(move || link.call(&[i]))).collect();
                workers.into_iter().map(|w| w.join().unwrap()).collect()
            });
            answers.concat()
        });
    }
    let host = |request: &[u8]| request.iter().map(|b| b * 2).collect::<Vec<u8>>();
    let answer = loaded(threaded_run, reversing_serve).run(b"", &host);
    assert_eq!(answer, (0u8..8).map(|i| i * 2).collect::<Vec<u8>>());
}
