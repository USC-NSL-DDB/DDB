use super::*;
use std::fs;

#[test]
fn final_carriage_return_is_not_treated_as_crlf() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("source"), "first\r\nlast\r").unwrap();
    assert_eq!(
        root(&directory)
            .read("source")
            .unwrap()
            .window(1, 2)
            .unwrap()
            .content,
        "first\nlast\r"
    );
}

#[test]
fn shared_cache_budget_is_global_but_document_identity_is_root_scoped() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("same"), "a").unwrap();
    fs::write(b.path().join("same"), "b").unwrap();
    let cache = Arc::new(SourceCache::new(4096, 1));
    let a = SourceRoot::open_with_cache(a.path(), Limits::default(), Arc::clone(&cache)).unwrap();
    let b = SourceRoot::open_with_cache(b.path(), Limits::default(), cache).unwrap();
    let first = a.read("same").unwrap();
    assert_eq!(b.read("same").unwrap().window(1, 1).unwrap().content, "b");
    let reloaded = a.read("same").unwrap();
    assert!(!Arc::ptr_eq(&first, &reloaded));
    assert_eq!(reloaded.window(1, 1).unwrap().content, "a");
}

#[test]
fn cache_byte_bound_does_not_retain_oversized_documents() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("source"), "line\n".repeat(100)).unwrap();
    let root = SourceRoot::open_with_cache(
        directory.path(),
        Limits::default(),
        Arc::new(SourceCache::new(100, 32)),
    )
    .unwrap();
    let first = root.read("source").unwrap();
    let next = root.read("source").unwrap();
    assert!(!Arc::ptr_eq(&first, &next));
    assert_eq!(first.total_lines(), 100);
}

fn root(directory: &tempfile::TempDir) -> SourceRoot {
    SourceRoot::open(directory.path(), Limits::default()).unwrap()
}

#[test]
fn directory_listing_is_sorted_nonrecursive_and_includes_nondebug_files() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("src")).unwrap();
    fs::write(directory.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(directory.path().join("config.yaml"), "key: value\n").unwrap();
    fs::write(directory.path().join(".private"), "hidden\n").unwrap();
    let root = root(&directory);
    let listing = root.list("", false).unwrap();
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["src", "config.yaml"]
    );
    assert_eq!(listing.entries[0].kind, EntryKind::Directory);
    assert_eq!(
        root.list("src", false).unwrap().entries[0].relative_path,
        "src/main.rs"
    );
    assert_eq!(root.list("", true).unwrap().entries.len(), 3);
}

#[test]
fn listing_revision_changes_when_entries_change() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    let empty = root.list("", false).unwrap().revision.clone();
    fs::write(directory.path().join("source.rs"), "a\n").unwrap();
    let next = root.list("", false).unwrap().revision.clone();
    assert_ne!(empty, next);
    assert_eq!(next, root.list("", false).unwrap().revision);
}

#[test]
fn source_windows_preserve_blank_lines_and_report_actual_eof() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("source.rs"), "first\r\n\r\nlast\n").unwrap();
    let document = root(&directory).read("source.rs").unwrap();
    let window = document.window(2, 1).unwrap();
    assert_eq!(window.content, "");
    assert_eq!(window.line_count, 1);
    assert_eq!(window.total_lines, 3);
    assert!(window.has_more);
    let tail = document.window(3, 10).unwrap();
    assert_eq!(tail.content, "last");
    assert!(!tail.has_more);
    let beyond = document.window(20, 5).unwrap();
    assert_eq!(beyond.line_count, 0);
    assert_eq!(beyond.total_lines, 3);
    assert!(!beyond.has_more);
}

#[test]
fn empty_file_and_single_blank_line_are_distinct() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("empty"), "").unwrap();
    fs::write(directory.path().join("blank"), "\n").unwrap();
    let root = root(&directory);
    assert_eq!(root.read("empty").unwrap().total_lines(), 0);
    assert_eq!(root.read("blank").unwrap().total_lines(), 1);
}

#[test]
fn repeated_windows_reuse_the_same_immutable_content_version() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("source.rs"), "one\ntwo\nthree\n").unwrap();
    let root = root(&directory);
    let first = root.read("source.rs").unwrap();
    let second = root.read("source.rs").unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(second.window(2, 1).unwrap().content, "two");
    fs::write(directory.path().join("replacement"), "new\ncontent\n").unwrap();
    fs::rename(
        directory.path().join("replacement"),
        directory.path().join("source.rs"),
    )
    .unwrap();
    let next = root.read("source.rs").unwrap();
    assert_ne!(first.content_hash(), next.content_hash());
    assert_eq!(first.window(1, 1).unwrap().content, "one");
    assert_eq!(next.window(1, 1).unwrap().content, "new");
}

#[test]
fn cached_file_count_is_bounded_and_eviction_does_not_change_pinned_documents() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("a"), "a").unwrap();
    fs::write(directory.path().join("b"), "b").unwrap();
    let root = SourceRoot::open_with_cache(
        directory.path(),
        Limits::default(),
        Arc::new(SourceCache::new(32 * 1024 * 1024, 1)),
    )
    .unwrap();
    let first = root.read("a").unwrap();
    root.read("b").unwrap();
    let reloaded = root.read("a").unwrap();
    assert!(!Arc::ptr_eq(&first, &reloaded));
    assert_eq!(first.content_hash(), reloaded.content_hash());
}

#[test]
fn content_and_directory_limits_fail_explicitly_without_partial_results() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("a"), "0123456789").unwrap();
    fs::write(directory.path().join("b"), "b").unwrap();
    let root = SourceRoot::open(
        directory.path(),
        Limits {
            max_file_bytes: 4,
            max_directory_entries: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(root.read("a"), Err(Error::Limit("file byte"))));
    assert!(matches!(
        root.list("", true),
        Err(Error::Limit("directory entry"))
    ));
}

#[test]
fn binary_files_and_directories_are_not_source_documents() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("binary"), [0xff, 0x00]).unwrap();
    fs::create_dir(directory.path().join("folder")).unwrap();
    let root = root(&directory);
    assert!(matches!(root.read("binary"), Err(Error::NotText)));
    assert!(root.read("folder").is_err());
}

#[test]
fn source_paths_cannot_escape_the_granted_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = root(&directory);
    for path in ["../secret", "/etc/passwd", "src/../../secret", "a\0b"] {
        assert!(
            matches!(root.read(path), Err(Error::InvalidPath)),
            "{path:?}"
        );
        assert!(
            matches!(root.list(path, true), Err(Error::InvalidPath)),
            "{path:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_escapes_are_visible_but_unreadable_even_after_a_cached_read() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("allowed"), "allowed\n").unwrap();
    fs::write(outside.path().join("secret"), "secret\n").unwrap();
    symlink("allowed", directory.path().join("link")).unwrap();
    let root = root(&directory);
    assert_eq!(
        root.read("link").unwrap().window(1, 1).unwrap().content,
        "allowed"
    );
    fs::remove_file(directory.path().join("link")).unwrap();
    symlink(outside.path().join("secret"), directory.path().join("link")).unwrap();
    assert!(root.read("link").is_err());
    assert_eq!(
        root.list("", false)
            .unwrap()
            .entries
            .iter()
            .find(|entry| entry.name == "link")
            .unwrap()
            .kind,
        EntryKind::Symlink
    );
    symlink(outside.path(), directory.path().join("escape")).unwrap();
    assert!(root.list("escape", true).is_err());
}

#[cfg(unix)]
#[test]
fn fifo_is_rejected_without_waiting_for_a_writer() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let directory = tempfile::tempdir().unwrap();
    let path = CString::new(directory.path().join("fifo").as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a live, NUL-terminated C string; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(matches!(root(&directory).read("fifo"), Err(Error::NotText)));
}

#[test]
fn independent_roots_do_not_merge_identical_relative_paths() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("main.rs"), "version a\n").unwrap();
    fs::write(b.path().join("main.rs"), "version b\n").unwrap();
    let a = root(&a).read("main.rs").unwrap();
    let b = root(&b).read("main.rs").unwrap();
    assert_ne!(a.content_hash(), b.content_hash());
    assert_eq!(a.window(1, 1).unwrap().content, "version a");
    assert_eq!(b.window(1, 1).unwrap().content, "version b");
}

#[test]
fn wide_directories_and_long_files_remain_windowed_and_cached() {
    let directory = tempfile::tempdir().unwrap();
    for index in 0..2_000 {
        fs::write(directory.path().join(format!("file-{index:04}.rs")), "x\n").unwrap();
    }
    let mut text = String::new();
    for line in 1..=100_000 {
        text.push_str(&format!("line {line}\n"));
    }
    fs::write(directory.path().join("long.rs"), text).unwrap();
    let root = root(&directory);
    let listing = root.list("", false).unwrap();
    assert_eq!(listing.entries.len(), 2_001);
    for _ in 0..100 {
        assert!(Arc::ptr_eq(&listing, &root.list_cached("", false).unwrap()));
    }
    fs::create_dir(directory.path().join("new-folder")).unwrap();
    assert!(!Arc::ptr_eq(&listing, &root.list("", false).unwrap()));
    let first = root.read("long.rs").unwrap();
    for _ in 0..100 {
        let same = root.read("long.rs").unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert_eq!(same.window(99_999, 1).unwrap().content, "line 99999");
        assert_eq!(same.total_lines(), 100_000);
    }
}
