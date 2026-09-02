use std::path::Path;

use globset::Glob;
use regex::RegexBuilder;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MatchError {
    #[error("invalid glob: {0}")]
    Glob(String),
    #[error("invalid regex: {0}")]
    Regex(String),
}

pub struct GlobMatcher {
    matcher: globset::GlobMatcher,
}

impl GlobMatcher {
    pub fn is_match(&self, path: &Path) -> bool {
        self.matcher.is_match(path)
    }
}

pub struct Regex {
    regex: regex::Regex,
}

impl Regex {
    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }
}

pub fn compile_glob(pattern: &str) -> Result<GlobMatcher, MatchError> {
    let matcher = Glob::new(pattern)
        .map_err(|error| MatchError::Glob(error.to_string()))?
        .compile_matcher();
    Ok(GlobMatcher { matcher })
}

pub fn compile_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, MatchError> {
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|error| MatchError::Regex(error.to_string()))?;
    Ok(Regex { regex })
}
