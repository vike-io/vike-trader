//! The study sandbox's channel and its folder read, in-process (the child itself is the lane's:
//! `crates/vike-strategy-sandbox/tests/study_child.rs`).

use super::*;

#[test]
fn a_message_crosses_whole_in_one_frame_or_many() {
    for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK + 3] {
        let message: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let id = 7 + len as u64;
        let mut wire = Vec::new();
        write_message(&mut wire, CALL, id, &message).unwrap();
        write_message(&mut wire, DONE, 0, b"end").unwrap();
        let mut input = wire.as_slice();
        assert_eq!(read_message(&mut input).unwrap(), (CALL, id, message), "{len} bytes");
        assert_eq!(read_message(&mut input).unwrap(), (DONE, 0, b"end".to_vec()));
        assert_eq!(read_message(&mut input).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}

#[test]
fn a_frame_over_the_cap_is_refused_before_it_is_read() {
    let mut wire = Vec::new();
    write_frame(&mut wire, CALL, 1, true, &[]).unwrap();
    wire[..4].copy_from_slice(&u32::try_from(CHUNK + 1).unwrap().to_be_bytes());
    let err = read_message(&mut wire.as_slice()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// A study folder is its `.rs` files, sorted, its entry among them; its name is checked before any
/// read (it arrives on the wire), and a folder without its entry file is no study.
#[test]
fn a_study_folder_is_read_as_its_sorted_rust_files() {
    let root = tempfile::tempdir().expect("studies folder");
    let study = root.path().join("poly_mm");
    std::fs::create_dir_all(&study).unwrap();
    for (file, text) in
        [("poly_mm.rs", "mod matrix;"), ("matrix.rs", "// m"), ("baseline.toml", "x = 1")]
    {
        std::fs::write(study.join(file), text).unwrap();
    }
    let (name, files) = read_study(&study).expect("a study");
    assert_eq!(name, "poly_mm");
    let names: Vec<&str> = files.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(names, ["matrix.rs", "poly_mm.rs"], "the recipe is no source");

    let words = |dir: &Path| match read_study(dir) {
        Err(StudyError::Study(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(words(&root.path().join("..")).contains("[a-z][a-z0-9_]*"));
    assert!(words(&root.path().join("Bad-Name")).contains("[a-z][a-z0-9_]*"));
    assert!(words(&root.path().join("absent")).contains("copy `absent.rs`"));
    std::fs::remove_file(study.join("poly_mm.rs")).unwrap();
    assert!(words(&study).contains("holds no `poly_mm.rs`"));
}
