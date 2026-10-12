use std::fs;
use std::path::PathBuf;

use crate::paths::{config, user_home};

pub(crate) fn file() -> PathBuf {
    config().join("config")
}

pub(crate) fn value_in(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| name.trim() == key)
        .map(|(_, value)| {
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        })
        .next_back()
        .filter(|value| !value.is_empty())
}

pub(crate) fn get(key: &str) -> Option<String> {
    value_in(&fs::read_to_string(file()).ok()?, key)
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => user_home().join(rest),
        None => PathBuf::from(path),
    }
}

pub(crate) fn slots() -> Option<usize> {
    get("GPU_SLOTS_FROM")
        .and_then(|from| fs::read_to_string(expand(&from)).ok())
        .and_then(|text| value_in(&text, "GPU_SLOTS"))
        .or_else(|| get("GPU_SLOTS"))
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::value_in;

    #[test]
    fn a_key_reads_its_last_line_without_quotes_or_comments() {
        let text = "# GPU_SLOTS=9\nQUEUE_SLOTS=3\nGPU_SLOTS=2\n GPU_SLOTS = \"3\" \nLEND=\n";
        assert_eq!(value_in(text, "GPU_SLOTS").as_deref(), Some("3"));
        assert_eq!(value_in(text, "QUEUE_SLOTS").as_deref(), Some("3"));
        assert_eq!(value_in(text, "LEND"), None);
        assert_eq!(value_in(text, "RENDER_SLOTS"), None);
    }
}
