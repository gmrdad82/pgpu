#[cfg(debug_assertions)]
pub(crate) fn fake(name: &str) -> Option<String> {
    let dir = std::env::var_os("PGPU_FAKE").filter(|dir| !dir.is_empty())?;
    let body = std::fs::read_to_string(std::path::Path::new(&dir).join(name)).ok()?;
    Some(body.trim().to_string())
}

#[cfg(not(debug_assertions))]
pub(crate) fn fake(_: &str) -> Option<String> {
    None
}

pub(crate) fn fake_percent(name: &str) -> Option<u32> {
    fake(name)?.parse().ok()
}
