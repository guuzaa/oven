//! Prefix matching behind every completion list — slash commands, model ids
//! and provider names. The rule lives here so the terminal layer only renders
//! what the app selected.

use oven_llm::ModelId;

/// Whether `key` starts with `query`, ignoring ASCII case. An empty query
/// matches every key, so an empty filter keeps the whole list.
pub fn matches(key: &str, query: &str) -> bool {
    key.get(..query.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(query))
}

/// Indices of the keys `query` selects, in input order.
pub fn select<'a>(keys: impl IntoIterator<Item = &'a str>, query: &str) -> Vec<usize> {
    keys.into_iter()
        .enumerate()
        .filter(|(_, key)| matches(key, query))
        .map(|(index, _)| index)
        .collect()
}

/// Whether model `slug` matches: the slug or the wire id it resolves to may
/// carry the prefix, so `gpt` finds `openai/gpt-4o`.
pub fn matches_model(slug: &str, query: &str) -> bool {
    matches(slug, query) || matches(ModelId::from(slug).wire_id(), query)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [&str; 4] = ["clear", "exit", "compact", "model"];

    #[test]
    fn empty_query_keeps_every_key() {
        assert!(matches("model", ""));
        assert!(matches("", ""));
        assert_eq!(select(KEYS, ""), vec![0, 1, 2, 3]);
    }

    #[test]
    fn matching_is_a_case_insensitive_prefix() {
        assert!(matches("deepseek-chat", "DEEP"));
        assert!(matches("Setup", "set"));
        assert!(matches("model", "model"));
        assert!(!matches("model", "models"));
        assert!(!matches("model", "del"));
    }

    #[test]
    fn select_keeps_input_order() {
        assert_eq!(select(KEYS, "c"), vec![0, 2]);
        assert_eq!(select(KEYS, "mo"), vec![3]);
        assert!(select(KEYS, "zzz").is_empty());
    }

    #[test]
    fn model_matches_slug_or_wire_id() {
        assert!(matches_model("openai/gpt-4o", "openai"));
        assert!(matches_model("openai/gpt-4o", "gpt"));
        assert!(matches_model("gpt-4o", "gpt"));
        assert!(!matches_model("openai/gpt-4o", "claude"));
    }
}
