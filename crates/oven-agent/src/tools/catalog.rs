use std::path::PathBuf;

use super::{BashTool, FileEditTool, FileReadTool, FileWriteTool, GlobTool, GrepTool, Tool};

pub struct BuiltinTool {
    pub name: &'static str,
    pub make: fn(PathBuf) -> Box<dyn Tool>,
}

pub const BUILTIN_TOOLS: &[BuiltinTool] = &[
    BuiltinTool {
        name: FileReadTool::NAME,
        make: |r| Box::new(FileReadTool::new(r)),
    },
    BuiltinTool {
        name: FileWriteTool::NAME,
        make: |r| Box::new(FileWriteTool::new(r)),
    },
    BuiltinTool {
        name: FileEditTool::NAME,
        make: |r| Box::new(FileEditTool::new(r)),
    },
    BuiltinTool {
        name: BashTool::NAME,
        make: |r| Box::new(BashTool::new(r)),
    },
    BuiltinTool {
        name: GlobTool::NAME,
        make: |r| Box::new(GlobTool::new(r)),
    },
    BuiltinTool {
        name: GrepTool::NAME,
        make: |r| Box::new(GrepTool::new(r)),
    },
];
