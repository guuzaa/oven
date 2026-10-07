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

active = "deepseek"

[providers.deepseek]
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
active = "deepseek"

[providers.deepseek]
reasoning_effort = "medium"
"#,
    );
    base.merge(overlay);
    assert_eq!(
        base.active_provider_config().unwrap().reasoning_effort,
        Some(oven_llm::ReasoningEffort::Medium)
    );
}

#[tokio::test]
async fn missing_files_leave_defaults_untouched() {
    let tmp = tempdir::TempDir::new("oven-load").unwrap();
    let missing = tmp.path().join("nope.toml");
    let cfg = AppConfig::load(None, Some(&missing)).await.unwrap();
    assert_eq!(cfg, AppConfig::default());
}

#[tokio::test]
async fn load_user_then_project_merges_with_project_precedence() {
    let tmp = tempdir::TempDir::new("oven-load-merge").unwrap();
    let user = tmp.path().join("user.toml");
    write(
        &user,
        "max_retries = 1\n\nactive = \"deepseek\"\n\n[providers.deepseek]\nmodel = \"from-user\"\n",
    );
    let project = tmp.path().join("project.toml");
    write(
        &project,
        "max_retries = 9\n\nactive = \"deepseek\"\n\n[providers.deepseek]\nbase_url = \"from-project\"\n",
    );

    let cfg = AppConfig::load(Some(&user), Some(&project)).await.unwrap();
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

#[tokio::test]
async fn load_unions_provider_maps_and_keeps_user_keys() {
    let tmp = tempdir::TempDir::new("oven-load-providers").unwrap();
    let user = tmp.path().join("user.toml");
    write(
        &user,
        "active = \"deepseek\"\nmodel = \"from-user\"\n\n[providers.deepseek]\napi_key = \"sk-ds\"\n[providers.xai]\napi_key = \"xai-key\"\n",
    );
    let project = tmp.path().join("project.toml");
    write(
        &project,
        "active = \"xai\"\n\n[providers.xai]\nmodel = \"grok-4.6\"\n",
    );

    let cfg = AppConfig::load(Some(&user), Some(&project)).await.unwrap();
    assert_eq!(cfg.active_provider.name, "xai");
    assert_eq!(
        cfg.active_provider_config().unwrap().model.as_deref(),
        Some("grok-4.6")
    );
    assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
    assert_eq!(cfg.providers["xai"].api_key.as_deref(), Some("xai-key"));
}

#[tokio::test]
async fn project_model_metadata_merges_field_by_field() {
    let tmp = tempdir::TempDir::new("oven-load-model-merge").unwrap();
    let user = tmp.path().join("user.toml");
    write(
        &user,
        "active = \"myproxy\"\n\n[providers.myproxy]\napi_key = \"k\"\n\n[providers.myproxy.models.\"m\"]\ncontext_window = 200000\nmax_output_tokens = 8192\nsupports_vision = false\n",
    );
    let project = tmp.path().join("project.toml");
    write(
        &project,
        "[providers.myproxy.models.\"m\"]\ncontext_window = 100000\n",
    );

    let cfg = AppConfig::load(Some(&user), Some(&project)).await.unwrap();
    let params = &cfg.active_provider_config().unwrap().models["m"];
    assert_eq!(params.context_window, Some(100_000));
    assert_eq!(params.max_output_tokens, Some(8192));
    assert_eq!(params.supports_vision, Some(false));
}

fn write(path: &Path, content: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

fn toml_lite(s: &str) -> AppConfig {
    toml::from_str(s).unwrap()
}
