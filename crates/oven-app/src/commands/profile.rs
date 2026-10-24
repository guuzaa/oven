//! `/profile`：跨厂商的质量预设。
//!
//! profile 的名字是我们定的（`balanced`、`deep`…），落到每个厂商各自的模型
//! 与强度上。命令只负责把意图说出来——表在配置里，落法在运行时查。

use crate::core::error::AppError;

use super::{CommandContext, CommandOutcome, SlashCommand};

pub struct Profile;

impl Profile {
    pub(crate) const NAME: &'static str = "profile";
}

impl SlashCommand for Profile {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &'static str {
        "Switch the quality profile: /profile <name>"
    }

    fn execute(
        &self,
        _cx: &mut CommandContext<'_>,
        args: &str,
    ) -> Result<CommandOutcome, AppError> {
        let name = args.trim();
        if name.is_empty() {
            return Ok(CommandOutcome::ProfileQuery);
        }
        Ok(CommandOutcome::ProfileChanged {
            name: name.to_string(),
        })
    }
}
