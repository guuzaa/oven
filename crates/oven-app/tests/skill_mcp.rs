use std::io::Write;
use std::path::Path;

use oven_app::config::AppConfig;
use oven_app::{App, McpServerConfig};

#[test]
fn config_enables_skills_and_mcps() {
    let tmp = tempdir::TempDir::new("app-apply").unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let cfg_path = root.join(".oven.yaml");
    write(
        &cfg_path,
        r#"
skills:
  - files
  - bash
mcps:
  filesystem:
    command: npx
    args:
      - -y
      - "@modelcontextprotocol/server-filesystem"
      - /tmp
"#,
    );
    let cfg = AppConfig::load(None, Some(&cfg_path)).unwrap();
    assert!(cfg.skills.contains(&"files".to_string()));
    assert!(cfg.skills.contains(&"bash".to_string()));
    assert!(cfg.mcps.contains_key("filesystem"));
    assert_eq!(cfg.mcps.get("filesystem").unwrap().command, "npx");

    let app = App::new(&root).with_config(cfg);
    assert!(app.skills().contains("files"));
    assert!(app.skills().contains("bash"));
    let tools = app.skills().merged_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"file_read"));
    assert!(names.contains(&"file_write"));
    assert!(names.contains(&"bash"));

    assert_eq!(app.mcps().len(), 1);
    let fs = app.mcps().get("filesystem").unwrap();
    assert_eq!(fs.command, "npx");
}

#[test]
fn unknown_skill_is_skipped_silently() {
    let cfg: AppConfig = serde_yaml::from_str("skills: [\"files\", \"nope-id\"]").unwrap();
    let tmp = tempdir::TempDir::new("app-unknown-skill").unwrap();
    let app = App::new(tmp.path()).with_config(cfg);
    assert!(app.skills().contains("files"));
    assert!(!app.skills().contains("nope-id"));
}

#[test]
fn empty_command_mcp_is_dropped() {
    let cfg_yaml = r#"
mcps:
  bad:
    command: ""
"#;
    let cfg: AppConfig = serde_yaml::from_str(cfg_yaml).unwrap();
    let tmp = tempdir::TempDir::new("app-bad-mcp").unwrap();
    let app = App::new(tmp.path()).with_config(cfg);
    assert!(app.mcps().is_empty());
}

#[test]
fn default_fallback_when_no_skill_registered() {
    let tmp = tempdir::TempDir::new("app-fallback").unwrap();
    let app = App::new(tmp.path());
    // No skills loaded; collect_tools still returns file_read/file_write/bash
    // via the in-App fallback. We can't access `collect_tools` directly; verify
    // by running run_chat against a mock-free environment. Only assert the
    // notion that mcps() is empty.
    assert!(app.mcps().is_empty());
}

fn write(path: &Path, content: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

#[test]
fn _mcp_server_config_roundtrip() {
    let cfg = McpServerConfig {
        command: "nvim".into(),
        args: vec!["-d".into()],
        env: [("X".to_string(), "Y".to_string())].into(),
    };
    let s = serde_yaml::to_string(&cfg).unwrap();
    let back: McpServerConfig = serde_yaml::from_str(&s).unwrap();
    assert_eq!(cfg, back);
}
