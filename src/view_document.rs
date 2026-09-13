//! Bounded declarative view content shared by built-in and extension providers.
//!
//! Providers describe information; the client remains responsible for layout, styling,
//! focus, and input. Control characters and unbounded trees are rejected at the boundary.

use serde::{Deserialize, Serialize};

pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
pub const MAX_SECTIONS: usize = 16;
pub const MAX_BLOCKS_PER_SECTION: usize = 32;
pub const MAX_TABLE_COLUMNS: usize = 12;
pub const MAX_TABLE_ROWS: usize = 256;
pub const MAX_TEXT_CHARS: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewDocument {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub sections: Vec<ViewSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewSection {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub blocks: Vec<ViewBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewBlock {
    Text {
        text: String,
        #[serde(default)]
        tone: ViewTone,
    },
    Badge {
        label: String,
        value: String,
        #[serde(default)]
        tone: ViewTone,
    },
    KeyValue {
        items: Vec<ViewKeyValue>,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Timeline {
        items: Vec<ViewTimelineItem>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewKeyValue {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub tone: ViewTone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewTimelineItem {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub tone: ViewTone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewAction {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub destructive: bool,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ViewTone {
    #[default]
    Default,
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewDocumentError(pub String);

impl std::fmt::Display for ViewDocumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ViewDocumentError {}

impl ViewDocument {
    pub fn validate(&self) -> Result<(), ViewDocumentError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| ViewDocumentError(format!("cannot encode document: {error}")))?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("document exceeds byte limit"));
        }
        validate_text("title", &self.title)?;
        if let Some(subtitle) = &self.subtitle {
            validate_text("subtitle", subtitle)?;
        }
        if self.sections.len() > MAX_SECTIONS {
            return Err(invalid("too many sections"));
        }
        let mut section_ids = std::collections::HashSet::new();
        for section in &self.sections {
            validate_id("section id", &section.id)?;
            if !section_ids.insert(section.id.as_str()) {
                return Err(invalid("duplicate section id"));
            }
            if let Some(title) = &section.title {
                validate_text("section title", title)?;
            }
            if section.blocks.len() > MAX_BLOCKS_PER_SECTION {
                return Err(invalid("too many blocks in section"));
            }
            for block in &section.blocks {
                validate_block(block)?;
            }
        }
        Ok(())
    }
}

fn validate_block(block: &ViewBlock) -> Result<(), ViewDocumentError> {
    match block {
        ViewBlock::Text { text, .. } => validate_text("text", text),
        ViewBlock::Badge { label, value, .. } => {
            validate_text("badge label", label)?;
            validate_text("badge value", value)
        }
        ViewBlock::KeyValue { items } => {
            if items.len() > MAX_TABLE_ROWS {
                return Err(invalid("too many key/value items"));
            }
            for item in items {
                validate_text("key", &item.key)?;
                validate_text("value", &item.value)?;
            }
            Ok(())
        }
        ViewBlock::Table { columns, rows } => {
            if columns.is_empty() || columns.len() > MAX_TABLE_COLUMNS {
                return Err(invalid("invalid table column count"));
            }
            if rows.len() > MAX_TABLE_ROWS {
                return Err(invalid("too many table rows"));
            }
            for column in columns {
                validate_text("table column", column)?;
            }
            for row in rows {
                if row.len() != columns.len() {
                    return Err(invalid("table row width does not match columns"));
                }
                for cell in row {
                    validate_text("table cell", cell)?;
                }
            }
            Ok(())
        }
        ViewBlock::Timeline { items } => {
            if items.len() > MAX_TABLE_ROWS {
                return Err(invalid("too many timeline items"));
            }
            for item in items {
                validate_text("timeline label", &item.label)?;
                if let Some(detail) = &item.detail {
                    validate_text("timeline detail", detail)?;
                }
            }
            Ok(())
        }
    }
}

fn validate_text(label: &str, value: &str) -> Result<(), ViewDocumentError> {
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(&format!("{label} exceeds character limit")));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(invalid(&format!("{label} contains a control character")));
    }
    Ok(())
}

fn validate_id(label: &str, value: &str) -> Result<(), ViewDocumentError> {
    if value.is_empty()
        || value.len() > 120
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid(&format!("invalid {label}")));
    }
    Ok(())
}

fn invalid(message: &str) -> ViewDocumentError {
    ViewDocumentError(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> ViewDocument {
        ViewDocument {
            title: "Terminal".into(),
            subtitle: Some("pane-1".into()),
            sections: vec![ViewSection {
                id: "processes".into(),
                title: Some("Processes".into()),
                blocks: vec![ViewBlock::Table {
                    columns: vec!["PID".into(), "CPU".into()],
                    rows: vec![vec!["42".into(), "1.2%".into()]],
                }],
            }],
        }
    }

    #[test]
    fn accepts_bounded_declarative_document() {
        document().validate().unwrap();
    }

    #[test]
    fn rejects_terminal_control_sequences() {
        let mut value = document();
        value.title = "unsafe\u{1b}[31m".into();
        assert!(value.validate().unwrap_err().0.contains("control"));
    }

    #[test]
    fn rejects_ragged_tables() {
        let mut value = document();
        value.sections[0].blocks = vec![ViewBlock::Table {
            columns: vec!["one".into(), "two".into()],
            rows: vec![vec!["only one".into()]],
        }];
        assert!(value.validate().is_err());
    }
}
