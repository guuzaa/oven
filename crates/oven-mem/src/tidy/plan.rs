use serde::Deserialize;
use serde_json::Value;

use crate::error::MemoryError;

const NO_OBJECT: &str = "no JSON object in the reply";
const NO_OPS: &str = "the object has no ops array";

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PutOp {
    pub scope: String,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub description: String,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub anchors: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum TidyOp {
    Put(PutOp),
    Delete {
        #[serde(rename = "ref")]
        reference: String,
        #[serde(default)]
        reason: String,
    },
    Demote {
        #[serde(rename = "ref")]
        reference: String,
        #[serde(default)]
        reason: String,
    },
}

/// The ops a reply asked for, each parsed on its own: one malformed op is
/// skipped and reported, and the rest still apply.
#[derive(Debug, Clone, PartialEq)]
pub struct TidyPlan {
    pub ops: Vec<Result<TidyOp, String>>,
}

/// The plan in a reply. Models wrap JSON in prose or fences, so the outermost
/// `{…}` is what is parsed; without one, nothing is applied.
pub fn parse_plan(reply: &str) -> Result<TidyPlan, MemoryError> {
    let invalid = |message: &str| MemoryError::InvalidPlan {
        message: message.to_owned(),
    };
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(invalid(NO_OBJECT));
    };
    let object = reply.get(start..=end).ok_or_else(|| invalid(NO_OBJECT))?;
    let value: Value = serde_json::from_str(object).map_err(|err| invalid(&err.to_string()))?;
    let Some(ops) = value.get("ops").and_then(Value::as_array) else {
        return Err(invalid(NO_OPS));
    };
    Ok(TidyPlan {
        ops: ops
            .iter()
            .map(|op| serde_json::from_value(op.clone()).map_err(|err| format!("{err}: {op}")))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::{NO_OBJECT, NO_OPS, PutOp, TidyOp, parse_plan};
    use crate::error::INVALID_PLAN;

    #[test]
    fn parses_every_op_inside_prose_and_fences() {
        let reply = r#"Here you go:
```json
{"ops": [
  {"op": "put", "scope": "project", "id": "proxy", "type": "fact", "description": "d", "replaces": ["global/x"]},
  {"op": "delete", "ref": "project/old", "reason": "stale"},
  {"op": "demote", "ref": "global/ui"}
]}
```"#;
        let plan = parse_plan(reply).unwrap();
        assert_eq!(
            plan.ops,
            vec![
                Ok(TidyOp::Put(PutOp {
                    scope: "project".into(),
                    id: "proxy".into(),
                    kind: "fact".into(),
                    description: "d".into(),
                    why: None,
                    body: String::new(),
                    anchors: vec![],
                    replaces: vec!["global/x".into()],
                })),
                Ok(TidyOp::Delete {
                    reference: "project/old".into(),
                    reason: "stale".into()
                }),
                Ok(TidyOp::Demote {
                    reference: "global/ui".into(),
                    reason: String::new()
                }),
            ]
        );
    }

    #[test]
    fn a_malformed_op_is_kept_as_an_error_beside_the_others() {
        let plan = parse_plan(
            r#"{"ops": [{"op": "move", "ref": "a/b"}, {"op": "delete", "ref": "project/x"}]}"#,
        )
        .unwrap();
        assert!(plan.ops[0].is_err());
        assert!(plan.ops[1].is_ok());
    }

    #[test]
    fn a_reply_without_a_plan_is_rejected_whole() {
        for (reply, reason) in [("nothing to do", NO_OBJECT), ("{\"changes\": []}", NO_OPS)] {
            let err = parse_plan(reply).unwrap_err();
            assert_eq!(err.to_string(), format!("{INVALID_PLAN}: {reason}"));
        }
        assert!(parse_plan("{not json}").is_err());
        assert_eq!(parse_plan("{\"ops\": []}").unwrap().ops, vec![]);
    }
}
