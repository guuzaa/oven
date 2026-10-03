use oven_host::split_frontmatter;
use serde::Deserialize;
use serde_yaml::{Mapping, Value};

use crate::error::MemoryError;
use crate::model::{Memory, MemoryId, MemoryKind, MemoryScope};

const FENCE: &str = "---";
const KIND_KEY: &str = "kind";
const DESCRIPTION_KEY: &str = "description";
const SOURCE_KEY: &str = "source";

#[derive(Debug, Deserialize)]
struct Front {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

pub fn parse(id: MemoryId, scope: MemoryScope, raw: &str) -> Result<Memory, MemoryError> {
    let Some((front, body)) = split_frontmatter(raw) else {
        return Err(MemoryError::MissingFrontmatter);
    };
    let front: Front = serde_yaml::from_str(front).map_err(|_| MemoryError::InvalidFrontmatter)?;
    let kind = kind_from_front(front.kind)?;
    let description = front.description.unwrap_or_default().trim().to_owned();
    if description.is_empty() {
        return Err(MemoryError::MissingDescription);
    }
    let source = front.source.and_then(|value| {
        let value = value.trim().to_owned();
        if value.is_empty() { None } else { Some(value) }
    });
    Ok(Memory {
        id,
        kind,
        description,
        body: trim_leading_blank_line(body).to_owned(),
        scope,
        source,
    })
}

pub fn render(memory: &Memory) -> String {
    let mut map = Mapping::new();
    map.insert(Value::from(KIND_KEY), Value::from(memory.kind.as_str()));
    map.insert(
        Value::from(DESCRIPTION_KEY),
        Value::from(memory.description.as_str()),
    );
    if let Some(source) = &memory.source {
        map.insert(Value::from(SOURCE_KEY), Value::from(source.as_str()));
    }
    let yaml = serde_yaml::to_string(&map).expect("memory frontmatter is valid yaml");
    let yaml = yaml
        .trim_start_matches("---\n")
        .trim_start_matches("---\r\n")
        .trim_end_matches(['\n', '\r']);
    format!("{FENCE}\n{yaml}\n{FENCE}\n\n{}", memory.body)
}

fn kind_from_front(kind: Option<String>) -> Result<MemoryKind, MemoryError> {
    let Some(kind) = kind else {
        return Err(MemoryError::MissingKind);
    };
    let kind = kind.trim();
    if kind.is_empty() {
        return Err(MemoryError::MissingKind);
    }
    if kind == MemoryKind::Fact.as_str() {
        return Ok(MemoryKind::Fact);
    }
    if kind == MemoryKind::Preference.as_str() {
        return Ok(MemoryKind::Preference);
    }
    Err(MemoryError::UnknownKind {
        kind: kind.to_owned(),
    })
}

fn trim_leading_blank_line(body: &str) -> &str {
    body.strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body)
}

#[cfg(test)]
mod tests {
    use super::{parse, render};
    use crate::error::{
        EXPECTED_KINDS, MISSING_DESCRIPTION, MISSING_KIND, MemoryError, UNKNOWN_KIND,
    };
    use crate::model::{MAX_BODY, Memory, MemoryId, MemoryKind, MemoryScope};

    fn id() -> MemoryId {
        MemoryId::new("proxy-requires-http2").unwrap()
    }

    fn sample(source: Option<String>) -> Memory {
        Memory {
            id: id(),
            kind: MemoryKind::Fact,
            description: "The internal proxy only speaks HTTP/2.".into(),
            body: "use http2_prior_knowledge\n".into(),
            scope: MemoryScope::Workspace,
            source,
        }
    }

    #[test]
    fn round_trip_equality() {
        let memory = sample(Some("01J8Z".into()));
        let parsed = parse(id(), MemoryScope::Workspace, &render(&memory)).unwrap();
        assert_eq!(parsed, memory);
    }

    #[test]
    fn missing_kind_is_an_error() {
        let raw = "---\ndescription: hello\n---\n\nbody\n";
        let err = parse(id(), MemoryScope::Workspace, raw).unwrap_err();
        assert_eq!(err, MemoryError::MissingKind);
        assert_eq!(err.to_string(), MISSING_KIND);
    }

    #[test]
    fn unknown_kind_is_an_error() {
        let raw = "---\nkind: gotcha\ndescription: hello\n---\n\nbody\n";
        let err = parse(id(), MemoryScope::User, raw).unwrap_err();
        assert_eq!(
            err,
            MemoryError::UnknownKind {
                kind: "gotcha".into()
            }
        );
        assert_eq!(
            err.to_string(),
            format!("{UNKNOWN_KIND} 'gotcha'; expected {EXPECTED_KINDS}")
        );
    }

    #[test]
    fn source_omitted_when_none() {
        let memory = sample(None);
        let rendered = render(&memory);
        assert!(!rendered.contains("source:"));
        assert!(rendered.contains(&memory.description));
        let parsed = parse(id(), memory.scope, &rendered).unwrap();
        assert_eq!(parsed, memory);
    }

    #[test]
    fn dashes_inside_body_stay_in_body() {
        let raw = "---\nkind: preference\ndescription: keep it\n---\n\nline\n---\nmore\n";
        let memory = parse(id(), MemoryScope::User, raw).unwrap();
        assert_eq!(memory.body, "line\n---\nmore\n");
        assert_eq!(memory.kind, MemoryKind::Preference);
    }

    #[test]
    fn empty_description_is_an_error() {
        let raw = "---\nkind: fact\ndescription:\n---\n\nbody\n";
        let err = parse(id(), MemoryScope::Workspace, raw).unwrap_err();
        assert_eq!(err, MemoryError::MissingDescription);
        assert_eq!(err.to_string(), MISSING_DESCRIPTION);
    }

    #[test]
    fn unknown_keys_and_over_limit_body_still_load() {
        let body = "x".repeat(MAX_BODY + 1);
        let raw = format!("---\nkind: fact\ndescription: hello\nextra: ignored\n---\n\n{body}");
        let memory = parse(id(), MemoryScope::Workspace, &raw).unwrap();
        assert_eq!(memory.description, "hello");
        assert_eq!(memory.body, body);
        assert!(memory.source.is_none());
    }
}
