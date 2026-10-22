//! What a running turn asks of the user, and what the user answered.
//!
//! A tool approval, the loop-limit prompt and a tool's question are the same
//! exchange — a request out, exactly one reply back — so they share one
//! response enum.

use serde::{Deserialize, Serialize};

/// A question the model wants the user to answer before it continues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopLimitDecision {
    Continue,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerResponse {
    Answered {
        answer: String,
    },
    /// The user dismissed the question: the model learns nothing from it.
    Declined,
}

/// What the user answered with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum UserResponse {
    Approval(ApprovalDecision),
    LoopLimit(LoopLimitDecision),
    Answer(AnswerResponse),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_survives_the_wire() {
        let response = UserResponse::Approval(ApprovalDecision::Approved);
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(json, r#"{"kind":"approval","value":"approved"}"#);
        assert_eq!(
            serde_json::from_str::<UserResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn an_answer_carries_its_text() {
        let response = UserResponse::Answer(AnswerResponse::Answered {
            answer: "the second one".into(),
        });
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("the second one"));
        assert_eq!(
            serde_json::from_str::<UserResponse>(&json).unwrap(),
            response
        );
    }
}
