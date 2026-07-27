use std::io::Write;
use std::path::Path;

use oven_app::config::AppConfig;

#[test]
fn merge_overrides_non_default_fields() {
    let mut base = AppConfig::default();
    let overlay = toml_lite_yaml(
        r#"
provider:
  model: claude-3-5-haiku-20241022
  base_url: "https://example.com/v1/"
request_timeout_secs: 30
max_retries: 5
"#,
    );
    base.merge(overlay);
    assert_eq!(
        base.provider.model.as_deref(),
        Some("claude-3-5-haiku-20241022")
    );
    assert_eq!(
        base.provider.base_url.as_deref(),
        Some("https://example.com/v1/")
    );
    assert_eq!(base.request_timeout_secs, 30);
    assert_eq!(base.max_retries, 5);
    // Untouched fields keep defaults
    assert_eq!(base.base_backoff_ms, 500);
}

#[test]
fn load_missing_files_returns_default() {
    let tmp = tempdir::TempDir::new("oven-load").unwrap();
    let missing = tmp.path().join("nope.yaml");
    let cfg = AppConfig::load(None, Some(&missing)).unwrap();
    assert_eq!(cfg, AppConfig::default());
}

#[test]
fn load_user_then_project_merges_with_project_precedence() {
    let tmp = tempdir::TempDir::new("oven-load-merge").unwrap();
    let user = tmp.path().join("user.yaml");
    write(&user, "provider:\n  model: from-user\nmax_retries: 1\n");
    let project = tmp.path().join("project.yaml");
    write(
        &project,
        "provider:\n  base_url: from-project\nmax_retries: 9\n",
    );

    let cfg = AppConfig::load(Some(&user), Some(&project)).unwrap();
    assert_eq!(cfg.provider.model.as_deref(), Some("from-user"));
    assert_eq!(cfg.provider.base_url.as_deref(), Some("from-project"));
    assert_eq!(cfg.max_retries, 9);
}

fn write(path: &Path, content: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

fn toml_lite_yaml(s: &str) -> AppConfig {
    serde_yaml::from_str(s).unwrap()
}
