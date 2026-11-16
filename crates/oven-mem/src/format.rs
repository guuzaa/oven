//! The OKF concept file a memory is stored as, and the legacy shape it is
//! still read from.

use oven_host::split_frontmatter;
use serde::Deserialize;
use serde::Serialize;
use serde_yaml::{Mapping, Value};

use crate::error::MemoryError;
use crate::model::{Memory, MemoryId, MemoryKind, MemoryScope, MemoryStatus, Source, Stamp};

const FENCE: &str = "---";
const TYPE_KEY: &str = "type";
const DESCRIPTION_KEY: &str = "description";
const WHY_KEY: &str = "why";
const STATUS_KEY: &str = "status";
const ANCHORS_KEY: &str = "anchors";
const STALE_AFTER_KEY: &str = "stale_after";
const GENERATED_KEY: &str = "generated";
const VERIFIED_KEY: &str = "verified";
const SOURCES_KEY: &str = "sources";

#[derive(Debug, Deserialize)]
struct Front {
    #[serde(default, rename = "type")]
    okf_type: Option<String>,
    /// The first design's name for `type`.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    anchors: Vec<String>,
    #[serde(default)]
    stale_after: Option<String>,
    #[serde(default)]
    generated: Option<Stamp>,
    #[serde(default)]
    verified: Option<OneOrMany>,
    #[serde(default)]
    sources: Vec<Source>,
    /// The first design's single session id.
    #[serde(default)]
    source: Option<String>,
    #[serde(flatten)]
    extra: Mapping,
}

/// OKF allows a single verifier as a bare mapping.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(Stamp),
    Many(Vec<Stamp>),
}

pub fn parse(id: MemoryId, scope: MemoryScope, raw: &str) -> Result<Memory, MemoryError> {
    let Some((front, body)) = split_frontmatter(raw) else {
        return Err(MemoryError::MissingFrontmatter);
    };
    let front: Front = serde_yaml::from_str(front).map_err(|_| MemoryError::InvalidFrontmatter)?;
    let kind = front
        .okf_type
        .or(front.kind)
        .filter(|raw| !raw.trim().is_empty())
        .ok_or(MemoryError::MissingType)?;
    let description = front.description.unwrap_or_default().trim().to_owned();
    if description.is_empty() {
        return Err(MemoryError::MissingDescription);
    }
    let mut memory = Memory::new(
        id,
        scope,
        MemoryKind::parse(&kind).unwrap_or(MemoryKind::Fact),
        &description,
    );
    memory.why = non_empty(front.why);
    memory.body = trim_leading_blank_line(body).to_owned();
    memory.anchors = front.anchors;
    memory.status = front
        .status
        .as_deref()
        .map(MemoryStatus::parse)
        .unwrap_or_default();
    memory.stale_after = non_empty(front.stale_after);
    memory.generated = front.generated;
    memory.verified = match front.verified {
        None => Vec::new(),
        Some(OneOrMany::One(stamp)) => vec![stamp],
        Some(OneOrMany::Many(stamps)) => stamps,
    };
    memory.sources = front.sources;
    if let Some(session) = non_empty(front.source) {
        memory.add_source(Source::session(&session));
    }
    memory.extra = front.extra;
    Ok(memory)
}

pub fn render(memory: &Memory) -> Result<String, MemoryError> {
    let mut map = Mapping::new();
    map.insert(TYPE_KEY.into(), memory.kind.okf_type().into());
    map.insert(DESCRIPTION_KEY.into(), memory.description.as_str().into());
    if let Some(why) = &memory.why {
        map.insert(WHY_KEY.into(), why.as_str().into());
    }
    if memory.status != MemoryStatus::Stable {
        map.insert(STATUS_KEY.into(), memory.status.as_str().into());
    }
    if !memory.anchors.is_empty() {
        map.insert(ANCHORS_KEY.into(), to_value(&memory.anchors)?);
    }
    if let Some(stale_after) = &memory.stale_after {
        map.insert(STALE_AFTER_KEY.into(), stale_after.as_str().into());
    }
    if let Some(generated) = &memory.generated {
        map.insert(GENERATED_KEY.into(), to_value(generated)?);
    }
    if !memory.verified.is_empty() {
        map.insert(VERIFIED_KEY.into(), to_value(&memory.verified)?);
    }
    if !memory.sources.is_empty() {
        map.insert(SOURCES_KEY.into(), to_value(&memory.sources)?);
    }
    for (key, value) in &memory.extra {
        if !map.contains_key(key) {
            map.insert(key.clone(), value.clone());
        }
    }
    let yaml = serde_yaml::to_string(&map).map_err(render_error)?;
    let yaml = yaml
        .trim_start_matches("---\n")
        .trim_start_matches("---\r\n")
        .trim_end_matches(['\n', '\r']);
    let mut out = format!("{FENCE}\n{yaml}\n{FENCE}\n");
    if !memory.body.is_empty() {
        out.push('\n');
        out.push_str(&memory.body);
    }
    Ok(out)
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, MemoryError> {
    serde_yaml::to_value(value).map_err(render_error)
}

fn render_error(err: serde_yaml::Error) -> MemoryError {
    MemoryError::Render {
        message: err.to_string(),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn trim_leading_blank_line(body: &str) -> &str {
    body.strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body)
}

#[cfg(test)]
mod tests {
    use serde_yaml::Value;

    use super::{parse, render};
    use crate::error::{MISSING_DESCRIPTION, MISSING_TYPE, MemoryError};
    use crate::model::{
        MAX_BODY, Memory, MemoryId, MemoryKind, MemoryScope, MemoryStatus, Source, Stamp,
    };

    fn id() -> MemoryId {
        MemoryId::new("proxy-requires-http2").unwrap()
    }

    fn full() -> Memory {
        let mut memory = Memory::new(
            id(),
            MemoryScope::Project,
            MemoryKind::Fact,
            "The internal proxy only speaks HTTP/2.",
        );
        memory.why = Some("handshakes reset otherwise".into());
        memory.body = "use http2_prior_knowledge\n".into();
        memory.anchors = vec!["crates/a.rs".into()];
        memory.status = MemoryStatus::Draft;
        memory.stale_after = Some("2027-05-01T00:00:00Z".into());
        memory.generated = Some(Stamp::model("m1"));
        memory.verified = vec![Stamp::human("me")];
        memory.sources = vec![Source::project("/ws"), Source::session("01J8Z")];
        memory
    }

    #[test]
    fn every_key_round_trips() {
        let memory = full();
        let rendered = render(&memory).unwrap();
        assert!(rendered.starts_with("---\ntype: Fact\n"));
        assert_eq!(
            parse(id(), MemoryScope::Project, &rendered).unwrap(),
            memory
        );
    }

    #[test]
    fn a_preference_without_a_body_ends_at_the_fence() {
        let memory = Memory::new(id(), MemoryScope::Global, MemoryKind::Preference, "rule");
        let rendered = render(&memory).unwrap();
        assert_eq!(rendered, "---\ntype: Preference\ndescription: rule\n---\n");
        assert_eq!(parse(id(), MemoryScope::Global, &rendered).unwrap(), memory);
    }

    #[test]
    fn unknown_keys_survive_a_rewrite() {
        let raw =
            "---\ntype: Fact\ndescription: hello\ntags: [net]\nresource: https://x\n---\n\nbody\n";
        let memory = parse(id(), MemoryScope::Project, raw).unwrap();
        assert_eq!(memory.extra.len(), 2);
        let rendered = render(&memory).unwrap();
        assert!(rendered.contains("resource: https://x"));
        let again = parse(id(), MemoryScope::Project, &rendered).unwrap();
        assert_eq!(again.extra.get("tags"), memory.extra.get("tags"));
    }

    #[test]
    fn a_bare_verified_mapping_is_one_verifier() {
        let raw = "---\ntype: Preference\ndescription: rule\nverified: { by: \"human:me\", at: 2026-11-16T00:00:00Z }\n---\n";
        let memory = parse(id(), MemoryScope::Global, raw).unwrap();
        assert_eq!(memory.verified.len(), 1);
        assert!(memory.is_reviewed());
    }

    #[test]
    fn a_legacy_file_reads_as_okf() {
        let raw = "---\nkind: preference\ndescription: keep it\nsource: 01J8Z\n---\n\nold body\n";
        let memory = parse(id(), MemoryScope::Global, raw).unwrap();
        assert_eq!(memory.kind, MemoryKind::Preference);
        assert_eq!(memory.body, "old body\n");
        assert_eq!(memory.sources, vec![Source::session("01J8Z")]);
        assert!(memory.extra.is_empty());
        let rendered = render(&memory).unwrap();
        assert!(rendered.contains("type: Preference"));
        assert!(!rendered.contains("kind:"));
        assert!(!rendered.contains("source: 01J8Z"));
    }

    #[test]
    fn an_unknown_type_reads_as_a_fact() {
        let raw = "---\ntype: Playbook\ndescription: steps\n---\n\nbody\n";
        let memory = parse(id(), MemoryScope::Shared, raw).unwrap();
        assert_eq!(memory.kind, MemoryKind::Fact);
    }

    #[test]
    fn missing_type_is_an_error() {
        let raw = "---\ndescription: hello\n---\n\nbody\n";
        let err = parse(id(), MemoryScope::Project, raw).unwrap_err();
        assert_eq!(err, MemoryError::MissingType);
        assert_eq!(err.to_string(), MISSING_TYPE);
    }

    #[test]
    fn empty_description_is_an_error() {
        let raw = "---\ntype: Fact\ndescription:\n---\n\nbody\n";
        let err = parse(id(), MemoryScope::Project, raw).unwrap_err();
        assert_eq!(err, MemoryError::MissingDescription);
        assert_eq!(err.to_string(), MISSING_DESCRIPTION);
    }

    #[test]
    fn dashes_inside_body_stay_in_body() {
        let raw = "---\ntype: Fact\ndescription: keep it\n---\n\nline\n---\nmore\n";
        let memory = parse(id(), MemoryScope::Project, raw).unwrap();
        assert_eq!(memory.body, "line\n---\nmore\n");
    }

    #[test]
    fn an_over_limit_body_still_loads() {
        let body = "x".repeat(MAX_BODY + 1);
        let raw = format!("---\ntype: Fact\ndescription: hello\n---\n\n{body}");
        let memory = parse(id(), MemoryScope::Project, &raw).unwrap();
        assert_eq!(memory.body, body);
        assert_eq!(memory.extra.get("kind"), None::<&Value>);
    }
}
