//! Synthesis output schema (BLUEPRINT §5.5, SYN-002).
//!
//! The agent must answer with a single JSON object. [`parse`] accepts the object alone or wrapped in a
//! Markdown code fence or surrounding prose, then validates it strictly: unknown fields, missing
//! fields, empty text and out-of-range list sizes are errors with a message naming the problem.

use serde::{Deserialize, Serialize};

pub const MAX_NEXT_STEPS: usize = 7;
pub const MAX_BLOCKERS: usize = 7;
pub const MAX_FIELD_CHARS: usize = 2000;
pub const MAX_DESCRIPTION_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Synthesis {
    /// One-line caption of what the project is.
    pub description: String,
    /// Short paragraph: where the project stands.
    pub state: String,
    /// What the user was trying to do last.
    pub last_goal: String,
    pub next_steps: Vec<String>,
    pub blockers: Vec<String>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SynthesisError {
    #[error("agent output contains no JSON object")]
    NoJson,
    #[error("agent output is not a valid synthesis object: {0}")]
    Schema(String),
    #[error("synthesis fails validation: {0}")]
    Invalid(String),
}

/// JSON Schema of the expected output, for agents that accept one (e.g. structured-output flags).
pub const JSON_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["description", "state", "last_goal", "next_steps", "blockers", "confidence"],
  "properties": {
    "description": {"type": "string", "minLength": 1, "maxLength": 160},
    "state": {"type": "string", "minLength": 1},
    "last_goal": {"type": "string"},
    "next_steps": {"type": "array", "items": {"type": "string", "minLength": 1}, "minItems": 1, "maxItems": 7},
    "blockers": {"type": "array", "items": {"type": "string", "minLength": 1}, "maxItems": 7},
    "confidence": {"type": "string", "enum": ["high", "medium", "low"]}
  }
}"#;

/// Parse and validate raw agent output.
pub fn parse(raw: &str) -> Result<Synthesis, SynthesisError> {
    let json = extract_object(raw).ok_or(SynthesisError::NoJson)?;
    let mut s: Synthesis = serde_json::from_str(json).map_err(|e| SynthesisError::Schema(e.to_string()))?;
    s.normalize();
    s.validate()?;
    Ok(s)
}

impl Synthesis {
    fn normalize(&mut self) {
        for field in [&mut self.description, &mut self.state, &mut self.last_goal] {
            *field = field.trim().to_string();
        }
        for list in [&mut self.next_steps, &mut self.blockers] {
            list.iter_mut().for_each(|item| *item = item.trim().to_string());
            list.retain(|item| !item.is_empty());
        }
    }

    pub fn validate(&self) -> Result<(), SynthesisError> {
        let mut problems = Vec::new();
        if self.description.is_empty() {
            problems.push("description is empty".to_string());
        }
        if self.description.chars().count() > MAX_DESCRIPTION_CHARS {
            problems.push(format!("description exceeds {MAX_DESCRIPTION_CHARS} characters"));
        }
        if self.description.contains('\n') {
            problems.push("description must be a single line".to_string());
        }
        if self.state.is_empty() {
            problems.push("state is empty".to_string());
        }
        if self.next_steps.is_empty() {
            problems.push("next_steps must contain at least one step".to_string());
        }
        if self.next_steps.len() > MAX_NEXT_STEPS {
            problems.push(format!(
                "next_steps has {} items (max {MAX_NEXT_STEPS})",
                self.next_steps.len()
            ));
        }
        if self.blockers.len() > MAX_BLOCKERS {
            problems.push(format!(
                "blockers has {} items (max {MAX_BLOCKERS})",
                self.blockers.len()
            ));
        }
        let texts = [&self.description, &self.state, &self.last_goal]
            .into_iter()
            .chain(&self.next_steps);
        if texts
            .chain(&self.blockers)
            .any(|t| t.chars().count() > MAX_FIELD_CHARS)
        {
            problems.push(format!("a field exceeds {MAX_FIELD_CHARS} characters"));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(SynthesisError::Invalid(problems.join("; ")))
        }
    }

    /// The single next step shown on a board card.
    pub fn top_next_step(&self) -> Option<&str> {
        self.next_steps.first().map(String::as_str)
    }
}

/// The outermost `{…}` span, tolerating code fences and prose around it.
/// First message for an interactive agent started in the project ("Launch agent here", LATER-002).
pub fn handoff_prompt(s: &Synthesis, project: &str) -> String {
    let mut out = format!(
        "Context from Logpose about {project} (a summary of where the project stands, written from its git state and recent agent sessions):\n\n\
About: {}\nState: {}\n",
        s.description, s.state
    );
    if !s.last_goal.is_empty() {
        out.push_str(&format!("Last goal: {}\n", s.last_goal));
    }
    out.push_str("Next steps:\n");
    for (i, step) in s.next_steps.iter().enumerate() {
        out.push_str(&format!("{}. {step}\n", i + 1));
    }
    if !s.blockers.is_empty() {
        out.push_str("Blockers:\n");
        for b in &s.blockers {
            out.push_str(&format!("- {b}\n"));
        }
    }
    out.push_str("\nCheck this against the repository first, then tell me what you would do next. Don't change anything yet.");
    out
}

/// Schema for a batch answer (SYN-012): `{"projects": [{"id": …, <synthesis fields>}, …]}`.
pub fn batch_schema() -> String {
    let mut item: serde_json::Value = serde_json::from_str(JSON_SCHEMA).expect("JSON_SCHEMA is valid JSON");
    item["properties"]["id"] = serde_json::json!({"type": "string", "minLength": 1});
    item["required"]
        .as_array_mut()
        .expect("required is an array")
        .insert(0, "id".into());
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["projects"],
        "properties": {"projects": {"type": "array", "items": item}},
    })
    .to_string()
}

/// Parse a batch answer into one result per requested id, in `ids` order. Each item is validated
/// on its own, so one bad item doesn't discard the others; ids missing from the answer are errors.
pub fn parse_batch(raw: &str, ids: &[&str]) -> Vec<Result<Synthesis, SynthesisError>> {
    let items: Result<Vec<serde_json::Value>, SynthesisError> = extract_object(raw)
        .ok_or(SynthesisError::NoJson)
        .and_then(|json| {
            serde_json::from_str::<serde_json::Value>(json).map_err(|e| SynthesisError::Schema(e.to_string()))
        })
        .and_then(|v| match v.get("projects") {
            Some(serde_json::Value::Array(a)) => Ok(a.clone()),
            _ => Err(SynthesisError::Schema("no `projects` array".into())),
        });
    let items = match items {
        Ok(items) => items,
        Err(e) => return ids.iter().map(|_| Err(e.clone())).collect(),
    };
    ids.iter()
        .map(|id| {
            let mut obj = items
                .iter()
                .find(|i| i.get("id").and_then(serde_json::Value::as_str) == Some(id))
                .cloned()
                .ok_or_else(|| {
                    SynthesisError::Schema(format!("project {id} missing from the batch answer"))
                })?;
            obj.as_object_mut().map(|o| o.remove("id"));
            let mut s: Synthesis =
                serde_json::from_value(obj).map_err(|e| SynthesisError::Schema(e.to_string()))?;
            s.normalize();
            s.validate()?;
            Ok(s)
        })
        .collect()
}

fn extract_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end > start).then(|| &raw[start..=end])
}

#[cfg(test)]
mod tests {
    #[test]
    fn handoff_prompt_lists_the_synthesis() {
        let s = Synthesis {
            description: "Telegram deals bot".into(),
            state: "Alerts work.".into(),
            last_goal: String::new(),
            next_steps: vec!["Add tests".into(), "Deploy".into()],
            blockers: vec!["Missing API key".into()],
            confidence: Confidence::Medium,
        };
        let p = handoff_prompt(&s, "deals-bot");
        assert!(p.starts_with("Context from Logpose about deals-bot"));
        assert!(p.contains(
            "About: Telegram deals bot\nState: Alerts work.\nNext steps:\n1. Add tests\n2. Deploy\n"
        ));
        assert!(!p.contains("Last goal"), "empty goal omitted");
        assert!(p.contains("- Missing API key") && p.ends_with("Don't change anything yet."));
    }

    #[test]
    fn batch_answers_are_validated_per_item() {
        let schema: serde_json::Value = serde_json::from_str(&batch_schema()).unwrap();
        assert_eq!(schema["properties"]["projects"]["items"]["required"][0], "id");
        let ok = |id: &str| {
            format!(
                r#"{{"id":"{id}","description":"d {id}","state":"s","last_goal":"","next_steps":["n"],"blockers":[],"confidence":"low"}}"#
            )
        };
        let bad = r#"{"id":"p2","description":"","state":"s","last_goal":"","next_steps":[],"blockers":[],"confidence":"low"}"#;
        let raw = format!(r#"Here: {{"projects":[{},{bad},{}]}}"#, ok("p1"), ok("p9"));
        let out = parse_batch(&raw, &["p1", "p2", "p3"]);
        assert_eq!(out[0].as_ref().unwrap().description, "d p1");
        assert!(
            matches!(out[1], Err(SynthesisError::Invalid(_))),
            "empty description fails validation"
        );
        assert!(matches!(&out[2], Err(SynthesisError::Schema(m)) if m.contains("p3 missing")));
        let all_bad = parse_batch("no json", &["a", "b"]);
        assert!(all_bad.iter().all(|r| *r == Err(SynthesisError::NoJson)));
        assert!(parse_batch(r#"{"x":1}"#, &["a"])[0].is_err());
    }

    use super::*;

    const VALID: &str = r#"{"description":"Telegram deals bot","state":"Alerts work; tests missing.",
        "last_goal":"Add price alerts","next_steps":[" Add threshold tests ","Deploy"],"blockers":[],
        "confidence":"high"}"#;

    #[test]
    fn valid_plain_and_fenced() {
        let s = parse(VALID).unwrap();
        assert_eq!(s.top_next_step(), Some("Add threshold tests"));
        assert_eq!(s.confidence, Confidence::High);
        let fenced = format!("Here you go:\n```json\n{VALID}\n```\n");
        assert_eq!(parse(&fenced).unwrap(), s);
    }

    #[test]
    fn partial_and_invalid_json() {
        assert_eq!(
            parse("I could not analyze this project."),
            Err(SynthesisError::NoJson)
        );
        let missing = parse(r#"{"description":"x","state":"y"}"#).unwrap_err();
        assert!(
            matches!(&missing, SynthesisError::Schema(m) if m.contains("missing field")),
            "{missing}"
        );
        let unknown = VALID.replace("\"blockers\"", "\"extra\":1,\"blockers\"");
        assert!(matches!(parse(&unknown), Err(SynthesisError::Schema(m)) if m.contains("unknown field")));
        let bad_enum = VALID.replace("\"high\"", "\"certain\"");
        assert!(matches!(parse(&bad_enum), Err(SynthesisError::Schema(_))));
        assert!(matches!(
            parse("{\"description\": \"x\", "),
            Err(SynthesisError::NoJson | SynthesisError::Schema(_))
        ));
    }

    #[test]
    fn semantic_validation() {
        let no_steps = VALID.replace(r#"[" Add threshold tests ","Deploy"]"#, r#"["  "]"#);
        let err = parse(&no_steps).unwrap_err().to_string();
        assert!(err.contains("at least one step"), "{err}");
        let empty_desc = VALID.replace("Telegram deals bot", " ");
        assert!(
            parse(&empty_desc)
                .unwrap_err()
                .to_string()
                .contains("description is empty")
        );
        let many = format!("[{}]", ["\"s\""; 8].join(","));
        let too_many = VALID.replace(r#"[" Add threshold tests ","Deploy"]"#, &many);
        assert!(parse(&too_many).unwrap_err().to_string().contains("max 7"));
    }

    #[test]
    fn json_schema_is_valid_json_matching_fields() {
        let schema: serde_json::Value = serde_json::from_str(JSON_SCHEMA).unwrap();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let sample = serde_json::to_value(parse(VALID).unwrap()).unwrap();
        let mut keys: Vec<&str> = sample.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        let mut req = required.clone();
        req.sort();
        assert_eq!(keys, req);
    }
}
