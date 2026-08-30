use std::io::Write;
use std::path::Path;

use oven_app::config::AppConfig;

#[test]
fn merge_overrides_non_default_fields() {
    let mut base = AppConfig::default();
    let overlay = toml_lite(
        r#"
request_timeout_secs = 30
max_retries = 5

[provider]
name = "deepseek"
model = "claude-3-5-haiku-20241022"
base_url = "https://example.com/v1/"
"#,
    );
    base.merge(overlay);
    assert_eq!(
        base.active_provider_config().unwrap().model.as_deref(),
        Some("claude-3-5-haiku-20241022")
    );
    assert_eq!(
        base.active_provider_config().unwrap().base_url.as_deref(),
        Some("https://example.com/v1/")
    );
    assert_eq!(base.request_timeout_secs, 30);
    assert_eq!(base.max_retries, 5);
    // Untouched fields keep defaults
    assert_eq!(base.base_backoff_ms, 500);
    assert!(
        base.active_provider_config()
            .unwrap()
            .reasoning_effort
            .is_none()
    );
}

#[test]
fn merge_overrides_reasoning_effort() {
    let mut base = AppConfig::default();
    let overlay = toml_lite(
        r#"
[provider]
name = "deepseek"
reasoning_effort = "medium"
"#,
    );
    base.merge(overlay);
    assert_eq!(
        base.active_provider_config().unwrap().reasoning_effort,
        Some(oven_llm::ReasoningEffort::Medium)
    );
}

#[test]
fn load_missing_files_returns_default() {
    let tmp = tempdir::TempDir::new("oven-load").unwrap();
    let missing = tmp.path().join("nope.toml");
    let cfg = AppConfig::load(None, Some(&missing)).unwrap();
    assert_eq!(cfg, AppConfig::default());
}

#[test]
fn load_user_then_project_merges_with_project_precedence() {
    let tmp = tempdir::TempDir::new("oven-load-merge").unwrap();
    let user = tmp.path().join("user.toml");
    write(
        &user,
        "max_retries = 1\n\n[provider]\nname = \"deepseek\"\nmodel = \"from-user\"\n",
    );
    let project = tmp.path().join("project.toml");
    write(
        &project,
        "max_retries = 9\n\n[provider]\nname = \"deepseek\"\nbase_url = \"from-project\"\n",
    );

    let cfg = AppConfig::load(Some(&user), Some(&project)).unwrap();
    assert_eq!(
        cfg.active_provider_config().unwrap().model.as_deref(),
        Some("from-user")
    );
    assert_eq!(
        cfg.active_provider_config().unwrap().base_url.as_deref(),
        Some("from-project")
    );
    assert_eq!(cfg.max_retries, 9);
}

#[test]
fn load_unions_provider_maps_and_keeps_user_keys() {
    let tmp = tempdir::TempDir::new("oven-load-providers").unwrap();
    let user = tmp.path().join("user.toml");
    write(
        &user,
        "[provider]\nname = \"deepseek\"\nmodel = \"from-user\"\n\n[providers.deepseek]\napi_key = \"sk-ds\"\n[providers.xai]\napi_key = \"xai-key\"\n",
    );
    let project = tmp.path().join("project.toml");
    write(
        &project,
        "[provider]\nname = \"deepseek\"\nmodel = \"from-project\"\n\n[providers.xai]\nmodel = \"grok-4.6\"\n",
    );

    let cfg = AppConfig::load(Some(&user), Some(&project)).unwrap();
    assert_eq!(
        cfg.active_provider_config().unwrap().model.as_deref(),
        Some("from-project")
    );
    assert_eq!(
        cfg.active_provider_config().unwrap().api_key.as_deref(),
        Some("sk-ds")
    );
    assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
    assert_eq!(cfg.providers["xai"].api_key.as_deref(), Some("xai-key"));
    assert_eq!(cfg.providers["xai"].model.as_deref(), Some("grok-4.6"));
}

fn write(path: &Path, content: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

fn toml_lite(s: &str) -> AppConfig {
    toml::from_str(s).unwrap()
}
