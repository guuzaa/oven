//! `Tool` / `ToolChoice`：工具定义与工具选择策略。

use serde::{Deserialize, Serialize};

/// 一个可供模型调用的工具定义。
///
/// `input_schema` 使用 JSON Schema 描述工具的入参结构，由各 provider 的
/// encoder 负责翻译为其 wire 表示（例如 OpenAI 的 `function.parameters`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

/// 工具选择策略：是否允许/强制模型调用工具，或指定必须调用的工具名。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    /// 由模型自行决定是否调用工具（默认策略）。
    #[default]
    Auto,
    /// 强制模型调用任意一个工具。
    Any,
    /// 禁止模型调用工具。
    None,
    /// 强制模型调用指定名称的工具。
    Tool(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_choice_default_is_auto() {
        assert_eq!(ToolChoice::default(), ToolChoice::Auto);
    }

    #[test]
    fn tool_choice_unit_variants_serialize_lowercase() {
        assert_eq!(
            serde_json::to_string(&ToolChoice::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(serde_json::to_string(&ToolChoice::Any).unwrap(), "\"any\"");
        assert_eq!(
            serde_json::to_string(&ToolChoice::None).unwrap(),
            "\"none\""
        );
    }

    #[test]
    fn tool_choice_tool_variant_round_trips() {
        let choice = ToolChoice::Tool("get_weather".to_string());
        let json = serde_json::to_string(&choice).unwrap();
        let decoded: ToolChoice = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, choice);
    }

    #[test]
    fn tool_round_trips_through_json() {
        let tool = Tool {
            name: "get_weather".to_string(),
            description: Some("查询天气".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let decoded: Tool = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, tool);
    }

    #[test]
    fn tool_description_is_optional() {
        let tool = Tool {
            name: "no_args_tool".to_string(),
            description: None,
            input_schema: serde_json::json!({}),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["description"], serde_json::Value::Null);
    }
}
