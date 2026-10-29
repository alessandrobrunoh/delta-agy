//! Parser for Antigravity CLI's `transcript.jsonl` into Delta Schema-1 [`Update`] records.
//!
//! Antigravity writes conversation steps as JSON Lines to:
//! `<appDataDir>/brain/<conversation-id>/.system_generated/logs/transcript.jsonl`
//! (and `transcript_full.jsonl` when `truncated_fields` is present).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use delta_core::{ContentBlock, ToolCallUpdate, ToolKind, ToolStatus, Update};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
pub struct TranscriptStep {
    pub step_index: u64,
    #[serde(default)]
    pub source: String,
    #[serde(rename = "type", default)]
    pub step_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<TranscriptToolCall>>,
    #[serde(default)]
    pub truncated_fields: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TranscriptToolCall {
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

/// Read new steps from `transcript.jsonl` (falling back to `transcript_full.jsonl` when a line has
/// `truncated_fields`) whose `step_index` is strictly greater than `after_step`.
pub fn read_new_steps(
    transcript_path: &Path,
    after_step: Option<u64>,
) -> std::io::Result<Vec<TranscriptStep>> {
    if !transcript_path.exists() {
        return Ok(Vec::new());
    }

    let full_path = full_transcript_path(transcript_path);
    let full_lines: Vec<String> = if full_path.exists() {
        BufReader::new(File::open(&full_path)?)
            .lines()
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };

    let reader = BufReader::new(File::open(transcript_path)?);
    let mut steps = Vec::new();

    for (line_idx, line_res) in reader.lines().enumerate() {
        let line = line_res?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(mut step) = serde_json::from_str::<TranscriptStep>(&line) else {
            continue;
        };
        if let Some(min_step) = after_step {
            if step.step_index <= min_step {
                continue;
            }
        }
        // Only ingest settled steps
        if step.status != "DONE" && step.status != "ERROR" {
            continue;
        }
        if step.truncated_fields.as_ref().is_some_and(|f| !f.is_empty()) {
            if let Some(full_line) = full_lines.get(line_idx) {
                if let Ok(full_step) = serde_json::from_str::<TranscriptStep>(full_line) {
                    step = full_step;
                }
            }
        }
        steps.push(step);
    }

    Ok(steps)
}

/// Translate settled Antigravity [`TranscriptStep`]s into Delta Schema-1 [`Update`]s.
pub fn steps_to_updates(steps: &[TranscriptStep]) -> (Vec<Update>, Option<u64>) {
    let mut updates = Vec::new();
    let mut max_step: Option<u64> = None;

    for step in steps {
        max_step = Some(max_step.map_or(step.step_index, |m| m.max(step.step_index)));

        match step.step_type.as_str() {
            "USER_INPUT" => {
                if let Some(raw) = &step.content {
                    let cleaned = extract_user_request(raw);
                    if !cleaned.is_empty() {
                        updates.push(Update::user_text(
                            format!("step-{}-user", step.step_index),
                            &cleaned,
                        ));
                    }
                }
            }
            "PLANNER_RESPONSE" => {
                if let Some(thinking) = &step.thinking {
                    let trimmed = thinking.trim();
                    if !trimmed.is_empty() {
                        updates.push(Update::thought_text(
                            format!("step-{}-thought", step.step_index),
                            trimmed,
                        ));
                    }
                }
                if let Some(content) = &step.content {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() {
                        updates.push(Update::agent_text(
                            format!("step-{}-agent", step.step_index),
                            trimmed,
                        ));
                    }
                }
                if let Some(tool_calls) = &step.tool_calls {
                    for (idx, tc) in tool_calls.iter().enumerate() {
                        updates.push(Update::ToolCallUpdate(ToolCallUpdate {
                            tool_call_id: format!("step-{}-tool-{}", step.step_index, idx),
                            name: Some(tc.name.clone()),
                            title: Some(tool_title(&tc.name, &tc.args)),
                            kind: Some(classify_tool_kind(&tc.name)),
                            status: Some(ToolStatus::Completed),
                            raw_input: Some(tc.args.clone()),
                            ..Default::default()
                        }));
                    }
                }
            }
            _ => {}
        }
    }

    (updates, max_step)
}

/// Extract the text inside `<USER_REQUEST>...</USER_REQUEST>` if present, stripping system metadata.
pub fn extract_user_request(raw: &str) -> String {
    if let (Some(start), Some(end)) = (raw.find("<USER_REQUEST>"), raw.find("</USER_REQUEST>")) {
        let inner_start = start + "<USER_REQUEST>".len();
        if inner_start <= end {
            return raw[inner_start..end].trim().to_owned();
        }
    }
    raw.trim().to_owned()
}

pub fn classify_tool_kind(tool_name: &str) -> ToolKind {
    match tool_name {
        "read_file" | "view_file" | "list_dir" => ToolKind::Read,
        "write_file" | "edit_file" => ToolKind::Edit,
        "run_command" | "manage_task" => ToolKind::Execute,
        "search_web" => ToolKind::Fetch,
        "grep_search" | "find_by_name" => ToolKind::Search,
        _ => ToolKind::Other,
    }
}

pub fn tool_title(tool_name: &str, args: &Value) -> String {
    if let Some(summary) = args.get("toolSummary").and_then(Value::as_str) {
        return summary.trim_matches('"').to_owned();
    }
    if let Some(path) = args.get("Path").and_then(Value::as_str) {
        return format!("{tool_name}: {}", path.trim_matches('"'));
    }
    if let Some(query) = args.get("query").and_then(Value::as_str) {
        return format!("{tool_name}: {}", query.trim_matches('"'));
    }
    tool_name.to_owned()
}

pub fn make_live_tool_update(
    step_idx: u64,
    tool_name: &str,
    args: &Value,
    status: ToolStatus,
    error: Option<&str>,
) -> Update {
    let content = error.map(|err| vec![ContentBlock::text(err).as_tool_content()]);
    Update::ToolCallUpdate(ToolCallUpdate {
        tool_call_id: format!("live-tool-{step_idx}"),
        name: Some(tool_name.to_owned()),
        title: Some(tool_title(tool_name, args)),
        kind: Some(classify_tool_kind(tool_name)),
        status: Some(status),
        raw_input: Some(args.clone()),
        content,
        ..Default::default()
    })
}

fn full_transcript_path(transcript_path: &Path) -> PathBuf {
    if let Some(parent) = transcript_path.parent() {
        parent.join("transcript_full.jsonl")
    } else {
        transcript_path.with_file_name("transcript_full.jsonl")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_user_request_tag() {
        let raw = "<USER_REQUEST>\nCiao mondo\n</USER_REQUEST>\n<ADDITIONAL_METADATA>...</ADDITIONAL_METADATA>";
        assert_eq!(extract_user_request(raw), "Ciao mondo");
    }

    #[test]
    fn translates_transcript_steps() {
        let steps = vec![
            TranscriptStep {
                step_index: 0,
                source: "USER_EXPLICIT".into(),
                step_type: "USER_INPUT".into(),
                status: "DONE".into(),
                content: Some("<USER_REQUEST>\nFix bug\n</USER_REQUEST>".into()),
                thinking: None,
                tool_calls: None,
                truncated_fields: None,
            },
            TranscriptStep {
                step_index: 1,
                source: "MODEL".into(),
                step_type: "PLANNER_RESPONSE".into(),
                status: "DONE".into(),
                content: Some("Done!".into()),
                thinking: Some("Checking files".into()),
                tool_calls: Some(vec![TranscriptToolCall {
                    name: "read_file".into(),
                    args: serde_json::json!({"Path": "/tmp/a.rs"}),
                }]),
                truncated_fields: None,
            },
        ];

        let (updates, max_step) = steps_to_updates(&steps);
        assert_eq!(max_step, Some(1));
        assert_eq!(updates.len(), 4);
    }
}
