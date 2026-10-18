pub const TOP_LEVEL_PROMPT: &str = include_str!("top_level.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_prompt_names_the_main_only_tools() {
        for tool in ["`answer`", "`task`"] {
            assert!(TOP_LEVEL_PROMPT.contains(tool), "{tool}");
        }
        assert!(!TOP_LEVEL_PROMPT.contains("`todo_write`"));
    }
}
