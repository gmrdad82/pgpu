fn knob(name: &str) -> Option<String> {
    crate::config::get(name)
}

fn parse_on(text: Option<&str>, default_on: bool) -> bool {
    match text.map(str::trim) {
        Some("on") => true,
        Some("off") => false,
        _ => default_on,
    }
}

fn parse_priority_low(text: Option<&str>) -> bool {
    !matches!(text.map(str::trim), Some("normal"))
}

pub(crate) fn lend_on(alone: bool) -> bool {
    parse_on(knob("LEND").as_deref(), alone)
}

pub(crate) fn priority_low() -> bool {
    parse_priority_low(knob("PRIORITY").as_deref())
}

#[cfg(test)]
mod tests {
    use super::{parse_on, parse_priority_low};

    #[test]
    fn knobs_take_each_value_and_the_default() {
        assert!(!parse_on(None, false));
        assert!(parse_on(Some("on"), false));
        assert!(!parse_on(Some("off"), false));
        assert!(!parse_on(Some("yes"), false));
        assert!(parse_on(None, true));
        assert!(!parse_on(Some(" off\n"), true));
        assert!(parse_on(Some("nope"), true));
        assert!(parse_priority_low(None));
        assert!(!parse_priority_low(Some("normal")));
        assert!(parse_priority_low(Some("high")));
    }
}
