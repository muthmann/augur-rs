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
