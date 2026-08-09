//! Config-driven tool and MCP registration through the app layer.

use std::io::Write;
use std::path::Path;

use oven_app::config::AppConfig;
use oven_app::{App, McpServerConfig};

#[test]
fn config_enables_tools_and_mcps() {
    let tmp = tempdir::TempDir::new("app-apply").unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let cfg_path = root.join(".oven.toml");
    write(
        &cfg_path,
        r#"
tools = ["file_read", "file_write", "bash"]

[mcps.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
"#,
    );
    let cfg = AppConfig::load(None, Some(&cfg_path)).unwrap();
    assert!(cfg.tools.contains(&"file_read".to_string()));
    assert!(cfg.mcps.contains_key("filesystem"));
    assert_eq!(cfg.mcps.get("filesystem").unwrap().command, "npx");

    let app = App::new(&root).with_config(cfg);
    let tools = app.tools().merged_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"file_read"));
    assert!(names.contains(&"file_write"));
    assert!(names.contains(&"bash"));

    assert_eq!(app.mcps().len(), 1);
    let fs = app.mcps().get("filesystem").unwrap();
    assert_eq!(fs.command, "npx");
}

#[test]
fn unknown_tool_is_skipped_silently() {
    let cfg: AppConfig = toml::from_str(r#"tools = ["file_read", "nope-tool"]"#).unwrap();
    let tmp = tempdir::TempDir::new("app-unknown-ids").unwrap();
    let app = App::new(tmp.path()).with_config(cfg);
    assert!(app.tools().contains("file_read"));
    assert!(!app.tools().contains("nope-tool"));
}

#[test]
fn unregistered_skill_ids_leave_empty_registry() {
    let cfg: AppConfig = toml::from_str(r#"skills = ["files", "nope-skill"]"#).unwrap();
    let tmp = tempdir::TempDir::new("app-skill-skip").unwrap();
    let app = App::new(tmp.path()).with_config(cfg);
    // No bundled skill modules are registered anymore; ids are accepted but
    // nothing is mounted, and the registry stays empty.
    assert!(app.skills().ids().is_empty());
    assert!(!app.skills().contains("files"));
}

#[test]
fn register_skill_contributes_prompt() {
    use oven_app::Skill;

    struct TestSkill;
    impl Skill for TestSkill {
        fn id(&self) -> &str {
            "test"
        }
        fn description(&self) -> &str {
            "test skill"
        }
        fn system_prompt(&self) -> Option<String> {
            Some("use the tools carefully".into())
        }
    }

    let tmp = tempdir::TempDir::new("app-skill-register").unwrap();
    let mut app = App::new(tmp.path());
    app.register_skill(Box::new(TestSkill));
    assert!(app.skills().contains("test"));
    let prompt = app.skills().merged_system_prompt().unwrap();
    assert!(prompt.contains("[skill: test]"));
    assert!(prompt.contains("use the tools carefully"));
}

#[test]
fn empty_command_mcp_is_dropped() {
    let cfg_toml = r#"
[mcps.bad]
command = ""
"#;
    let cfg: AppConfig = toml::from_str(cfg_toml).unwrap();
    let tmp = tempdir::TempDir::new("app-bad-mcp").unwrap();
    let app = App::new(tmp.path()).with_config(cfg);
    assert!(app.mcps().is_empty());
}

#[test]
fn default_tools_when_none_requested() {
    let tmp = tempdir::TempDir::new("app-fallback").unwrap();
    let app = App::new(tmp.path());
    // Tools default independently of skills: an unconfigured App still mounts
    // the built-in set.
    let names = app.tools().names();
    assert!(names.contains(&"file_read"));
    assert!(names.contains(&"file_write"));
    assert!(names.contains(&"bash"));
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
        ..Default::default()
    };
    let s = toml::to_string(&cfg).unwrap();
    let back: McpServerConfig = toml::from_str(&s).unwrap();
    assert_eq!(cfg, back);
}
