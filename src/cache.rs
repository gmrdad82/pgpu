use std::fs;
use std::path::Path;

pub(crate) fn fresh(at: u128, now: u128, keep: u128) -> bool {
    now.checked_sub(at).is_some_and(|age| age < keep)
}

pub(crate) fn stamped(body: &str) -> Option<(u128, &str)> {
    let (at, rest) = body.split_once('\n').unwrap_or((body, ""));
    Some((at.parse().ok()?, rest))
}

pub(crate) fn read(path: &Path, now: u128, keep: u128) -> Option<String> {
    let body = fs::read_to_string(path).ok()?;
    let (at, rest) = stamped(&body)?;
    fresh(at, now, keep).then(|| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::{fresh, read, stamped};
    use std::fs;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join("test-cache")
            .join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_stamp_is_fresh_until_it_is_keep_old_and_never_from_the_future() {
        assert!(fresh(1_000, 1_000, 1_000));
        assert!(fresh(1_000, 1_999, 1_000));
        assert!(!fresh(1_000, 2_000, 1_000));
        assert!(!fresh(1_001, 1_000, 1_000), "a stamp from the future");
        assert!(!fresh(u128::MAX, 0, u128::MAX));
    }

    #[test]
    fn the_stamp_is_the_first_line_and_the_rest_is_kept_as_written() {
        assert_eq!(stamped("10\na\n\nb\n"), Some((10, "a\n\nb\n")));
        assert_eq!(stamped("10"), Some((10, "")));
        assert_eq!(stamped("x\n1\n"), None);
        assert_eq!(stamped(""), None);
        let dir = test_dir("read");
        let path = dir.join("hour");
        fs::write(&path, "5\n\n").unwrap();
        assert_eq!(read(&path, 5, 1).as_deref(), Some("\n"));
        assert_eq!(read(&dir.join("missing"), 5, 1), None);
    }
}
