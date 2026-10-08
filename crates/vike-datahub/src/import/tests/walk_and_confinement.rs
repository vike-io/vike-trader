//! The walk's caps, and confinement: each planted object is refused and never read (unix).

use super::*;

// ---- the walk ---------------------------------------------------------------------------------

/// The walk never descends past the depth cap, whatever the grammar accepts.
#[test]
fn the_walk_stops_at_the_depth_cap() {
    let tree = Tree::new();
    let deep = tree.dataset().join("1").join("2").join("3").join("4");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join(format!("{}.day", MON / DAY)), b"\x01").unwrap();
    fs::write(
        tree.dataset().join("1").join("2").join("3").join(format!("{}.day", (MON + DAY) / DAY)),
        b"\x01",
    )
    .unwrap();
    let w = walk_of(&tree, &WalkCaps::DEFAULT);
    assert_eq!(
        w.daily.keys().copied().collect::<Vec<_>>(),
        vec![MON + DAY],
        "depth 4 found, depth 5 never"
    );
    assert_eq!(w.other_objects, 1, "the depth-4 directory is counted, not descended");
}

/// One entry past the cap REFUSES the request rather than planning part of the directory.
#[test]
fn a_walk_past_the_entry_cap_refuses_the_request() {
    let tree = Tree::new();
    for i in 0..5 {
        tree.daily(MON + i * DAY, b"\x01");
    }
    // 1 bucket directory + 5 files = 6 entries.
    let caps = WalkCaps { max_entries: 6, max_depth: 4 };
    let (format, _) = FakeFormat::new();
    assert!(walk::walk_dataset(&tree.root, &format, "EURUSD", 0, &caps).is_ok(), "at the cap");
    let caps = WalkCaps { max_entries: 5, max_depth: 4 };
    let why = walk::walk_dataset(&tree.root, &format, "EURUSD", 0, &caps).expect_err("one past");
    assert!(why.contains("more than 5 entries") && why.contains("Nothing was read"), "{why}");
    assert_eq!(WalkCaps::DEFAULT, WalkCaps { max_entries: 100_000, max_depth: 4 }, "§5's values");
}

// ---- confinement (unix): each planted object is refused and never read ------------------------

#[cfg(unix)]
mod confinement {
    use std::os::unix::fs::symlink;
    use std::process::Command;

    use vike_datahub_client::archive::EntryClass;

    use super::*;

    /// A directory OUTSIDE the imports root holding a valid-looking daily file — what every
    /// planted link below points at.
    fn outside(tree: &Tree) -> PathBuf {
        let out = tree._tmp.path().join("outside");
        fs::create_dir_all(out.join("2")).unwrap();
        fs::write(out.join("2").join(format!("{}.day", MON / DAY)), b"\x09SECRET").unwrap();
        out
    }

    #[test]
    fn a_symlinked_dataset_is_unreadable_and_never_walked() {
        let tree = Tree::new();
        let out = outside(&tree);
        symlink(&out, tree.root.join(FAKE).join("GBPUSD")).unwrap();
        let (format, _) = FakeFormat::new();
        let w = walk::walk_dataset(&tree.root, &format, "GBPUSD", 0, &WalkCaps::DEFAULT).unwrap();
        assert!(matches!(&w.dir, DatasetDir::Unreadable { why } if why.contains("symbolic link")));
        assert!(w.daily.is_empty(), "the link was not followed");
        assert_eq!(w.skipped[0].class, EntryClass::Symlink);
        assert_eq!(w.skipped[0].path, format!("{FAKE}/GBPUSD"), "relative to the root");
    }

    #[test]
    fn a_symlinked_year_and_a_symlinked_file_are_skipped_and_never_read() {
        let tree = Tree::new();
        let out = outside(&tree);
        symlink(out.join("2"), tree.dataset().join("2")).unwrap();
        fs::create_dir_all(tree.dataset().join("3")).unwrap();
        symlink(
            out.join("2").join(format!("{}.day", MON / DAY)),
            tree.dataset().join("3").join(format!("{}.day", (MON + DAY) / DAY)),
        )
        .unwrap();
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        assert!(w.daily.is_empty(), "neither link was followed: {:?}", w.daily.keys());
        let classes: Vec<_> = w.skipped.iter().map(|s| s.class).collect();
        assert_eq!(classes, vec![EntryClass::Symlink, EntryClass::Symlink]);
    }

    #[test]
    fn a_hard_linked_file_is_skipped() {
        let tree = Tree::new();
        let out = outside(&tree);
        fs::create_dir_all(tree.dataset().join("2")).unwrap();
        fs::hard_link(
            out.join("2").join(format!("{}.day", MON / DAY)),
            tree.dataset().join("2").join(format!("{}.day", MON / DAY)),
        )
        .unwrap();
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        assert!(w.daily.is_empty());
        assert_eq!(w.skipped.len(), 1);
        assert_eq!(w.skipped[0].class, EntryClass::HardLinked);
    }

    fn mkfifo(path: &Path) {
        let status = Command::new("mkfifo").arg(path).status().expect("mkfifo runs");
        assert!(status.success(), "mkfifo {}", path.display());
    }

    /// A FIFO named like a day file is skipped by the walk — and one swapped in AFTER the walk
    /// is refused by the open WITHOUT parking the thread (`O_NONBLOCK`), as not a regular file.
    #[test]
    fn a_fifo_is_skipped_and_one_swapped_in_after_the_walk_never_parks_the_open() {
        let tree = Tree::new();
        fs::create_dir_all(tree.dataset().join("2")).unwrap();
        mkfifo(&tree.dataset().join("2").join(format!("{}.day", MON / DAY)));
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        assert!(w.daily.is_empty());
        assert_eq!(w.skipped[0].class, EntryClass::Fifo);

        // Swap: vet a real file, then replace it with a FIFO of the same name.
        let path = tree.daily(MON + DAY, b"\x05abc");
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        let file = w.daily[&(MON + DAY)].clone();
        fs::remove_file(&path).unwrap();
        mkfifo(&path);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(walk::read_vetted(&file, 64));
        });
        let read = rx.recv_timeout(WAIT).expect("the open must not block on a FIFO");
        assert_eq!(read.expect_err("a FIFO is refused").class, walk::CHANGED_SINCE_WALK);
    }

    /// **THE SWAP.** A file replaced between the walk and the open — by a rename, so the name is
    /// the same, the LENGTH is the same and only the object differs — is refused, and the planted
    /// bytes are never returned. Only the `(dev, ino)` comparison can see this.
    #[test]
    fn a_file_swapped_between_the_walk_and_the_open_is_refused_and_not_read() {
        let tree = Tree::new();
        let path = tree.daily(MON, b"\x05real");
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        let file = &w.daily[&MON];

        let planted = tree._tmp.path().join("planted");
        fs::write(&planted, b"\x05EVIL").unwrap(); // the SAME length as the vetted file
        fs::rename(&planted, &path).unwrap();

        match walk::read_vetted(file, 64) {
            Err(refusal) => {
                assert_eq!(refusal.class, walk::CHANGED_SINCE_WALK, "{refusal:?}");
                assert!(!refusal.detail.contains("EVIL"), "no file byte is ever echoed");
            }
            Ok(bytes) => {
                panic!("the swapped-in file was READ: {:?}", String::from_utf8_lossy(&bytes))
            }
        }
        assert!(walk::read_vetted_prefix(file, 1).is_err(), "the header read is held to it too");

        // ...and a symlink swapped in for the file is refused at the open (O_NOFOLLOW).
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        let file = w.daily[&MON].clone();
        let out = outside(&tree);
        fs::remove_file(&path).unwrap();
        symlink(out.join("2").join(format!("{}.day", MON / DAY)), &path).unwrap();
        assert_eq!(
            walk::read_vetted(&file, 64).expect_err("a link").class,
            walk::CHANGED_SINCE_WALK
        );
    }
}
