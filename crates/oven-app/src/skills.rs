//! Bundled skills.
//!
//! Skills are how the App layer exposes features to the Agent layer without
//! baking tool wiring into crates other than `oven-app`. Each `Skill`
//! implementation lives here; the user opts in via the `skills:` list in
//! `config.yaml`.

use std::path::PathBuf;

use oven_agent::{BashTool, FileReadTool, FileWriteTool, Tool};

use crate::skill::Skill;

/// The "files" skill: file reading + writing tools plus workspace guidance.
pub struct FilesSkill {
    root: PathBuf,
}

impl FilesSkill {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Skill for FilesSkill {
    fn id(&self) -> &str {
        "files"
    }
    fn description(&self) -> &str {
        "Read and write files in the workspace."
    }
    fn system_prompt(&self) -> Option<String> {
        Some(
            "When asked to inspect or modify a file, prefer the file_read and \
 file_write tools. Paths are relative to the workspace root."
                .into(),
        )
    }
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(FileReadTool::new(self.root.clone())),
            Box::new(FileWriteTool::new(self.root.clone())),
        ]
    }
}

/// The "bash" skill: shell command execution.
pub struct BashSkill {
    root: PathBuf,
}

impl BashSkill {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Skill for BashSkill {
    fn id(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run shell commands inside the workspace."
    }
    fn system_prompt(&self) -> Option<String> {
        Some(
            "Use the bash tool to run builds, tests, or git. Keep commands \
 short and prefer read-only commands when possible."
                .into(),
        )
    }
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(BashTool::new(self.root.clone()))]
    }
}
