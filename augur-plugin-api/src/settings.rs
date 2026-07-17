use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SettingsSchema {
    pub sections: Vec<SettingsSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettingsSection {
    pub label: String,
    pub description: Option<String>,
    pub default_open: bool,
    pub items: Vec<SettingItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettingItem {
    pub key: String,
    pub label: String,
    pub tooltip: Option<String>,
    pub kind: SettingKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettingKind {
    Bool {
        default: bool,
    },
    F64Slider {
        min: f64,
        max: f64,
        default: f64,
        suffix: Option<String>,
    },
    I64Slider {
        min: i64,
        max: i64,
        default: i64,
        suffix: Option<String>,
    },
    F64Drag {
        min: f64,
        max: f64,
        speed: f64,
        default: f64,
    },
    I64Drag {
        min: i64,
        max: i64,
        default: i64,
    },
    Enum {
        variants: Vec<String>,
        default: usize,
    },
    /// Free-form text (names, prefixes). Exchanged as a JSON string.
    Text {
        default: String,
    },
    /// Filesystem path chosen through a native dialog the host renders next
    /// to an editable text field. Exchanged as a JSON string.
    Path {
        dialog: PathDialogKind,
        default: String,
    },
    /// Momentary action button. The host sends `set_setting(key, true)` once
    /// per click; the plugin treats every call as an edge, never as state,
    /// and `get_setting` should return `false`. This is the frame-independent
    /// trigger channel — it works with no camera attached, unlike host-view
    /// actions, which are only delivered inside `process_frame`.
    Button,
}

/// Which native dialog the host opens for a [`SettingKind::Path`] item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PathDialogKind {
    OpenFile,
    SaveFile,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum StatusEntry {
    Text(String),
    LabeledValue {
        label: String,
        value: String,
        color: Option<[u8; 3]>,
    },
    Sparkline {
        label: String,
        values: Vec<f64>,
        lower_is_better: bool,
    },
}
