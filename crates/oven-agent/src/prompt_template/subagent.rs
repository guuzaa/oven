use std::fmt::Write;

const PREAMBLE: &str = include_str!("subagent.md");

/// The system prompt a subagent runs with: the same instructions, skills and
/// environment the caller has, narrowed by what this role may do and by the
/// fact that only its final message comes back.
pub fn subagent_preamble(role: &str, guidance: &str) -> String {
    let mut out = PREAMBLE.trim().to_string();
    let _ = write!(out, "\n\n# Your role: {role}\n\n{guidance}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_names_the_role_and_its_guidance() {
        let text = subagent_preamble("explore", "You may only read.");
        assert!(text.starts_with("You are a subagent"), "{text}");
        assert!(text.contains("# Your role: explore"), "{text}");
        assert!(text.ends_with("You may only read.\n"), "{text}");
    }
}
