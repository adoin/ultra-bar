//! Host-owned editing protocol. Plugins describe values and constraints, not controls.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
pub const PROTOCOL: u32 = 1;
pub const REQUEST: usize = 0x55424531;
pub const EVENT: usize = 0x55424532;
pub type Values = BTreeMap<String, String>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Secret,
    Search,
    Multiline,
    Number,
    Select,
    Toggle,
    MultiSelect,
    Date,
    Time,
    DateTime,
    Color,
    File,
    Directory,
    Hotkey,
    Tags,
    Code,
    Json,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionItem {
    pub value: String,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub field: String,
    pub one_of: Vec<String>,
    #[serde(default)]
    pub any: Vec<Condition>,
}
impl Condition {
    pub fn one(field: &str, values: &[&str]) -> Self {
        Self {
            field: field.into(),
            one_of: values.iter().map(|s| s.to_string()).collect(),
            any: vec![],
        }
    }
    pub fn matches(&self, values: &Values) -> bool {
        if self.any.is_empty() {
            values
                .get(&self.field)
                .is_some_and(|v| self.one_of.contains(v))
        } else {
            self.any.iter().any(|c| c.matches(values))
        }
    }
    fn references(&self, ids: &HashSet<&String>) -> bool {
        if self.any.is_empty() {
            ids.contains(&self.field)
        } else {
            self.any.iter().all(|c| c.references(ids))
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Constraints {
    pub required: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub max_length: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub options: Vec<OptionItem>,
    #[serde(default)]
    pub constraints: Constraints,
    #[serde(default)]
    pub visible_when: Vec<Condition>,
    #[serde(default)]
    pub enabled_when: Vec<Condition>,
    #[serde(default = "full_span")]
    pub span: u8,
}
fn full_span() -> u8 {
    2
}
impl Field {
    pub fn new(id: &str, label: &str, kind: Kind, value: impl ToString) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            value: value.to_string(),
            options: vec![],
            constraints: Constraints::default(),
            visible_when: vec![],
            enabled_when: vec![],
            span: 2,
        }
    }
    pub fn visible(&self, values: &Values) -> bool {
        self.visible_when.iter().all(|c| c.matches(values))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Form {
    pub session: u64,
    pub title: String,
    pub fields: Vec<Field>,
    #[serde(default)]
    pub can_delete: bool,
}
impl Form {
    pub fn validate_schema(&self) -> Result<(), String> {
        if self.fields.len() > 64 || self.title.len() > 512 {
            return Err("编辑器定义过大".into());
        }
        let mut ids = HashSet::new();
        for f in &self.fields {
            if f.id.is_empty()
                || f.id.len() > 128
                || !ids.insert(&f.id)
                || !(1..=2).contains(&f.span)
                || f.options.len() > 128
                || f.value.len() > 32768
                || f.constraints.min.is_some_and(|n| !n.is_finite())
                || f.constraints.max.is_some_and(|n| !n.is_finite())
                || f.constraints
                    .step
                    .is_some_and(|n| !n.is_finite() || n <= 0.)
                || (matches!(f.kind, Kind::Select | Kind::MultiSelect) && f.options.is_empty())
            {
                return Err("编辑器字段定义无效".into());
            }
        }
        for f in &self.fields {
            if f.visible_when
                .iter()
                .chain(&f.enabled_when)
                .any(|c| !c.references(&ids))
            {
                return Err("字段可见条件引用了不存在的字段".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
    Open {
        form: Form,
    },
    Complete {
        session: u64,
        #[serde(default)]
        error: String,
        #[serde(default)]
        close: bool,
    },
    Mount {
        session: u64,
        field: Field,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    ChoosePath {
        session: u64,
        save: bool,
    },
    Settings,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: u32,
    pub plugin_id: String,
    pub operation: Operation,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EditorEvent {
    pub session: u64,
    pub action: String,
    #[serde(default)]
    pub window: usize,
    #[serde(default)]
    pub input: usize,
    #[serde(default)]
    pub values: Values,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editing_types_and_styles_are_owned_by_the_host() {
        assert!(
            serde_json::from_value::<Field>(
                serde_json::json!({"id":"x","label":"X","kind":"custom_canvas","value":""})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<Field>(serde_json::json!({"id":"x","label":"X","kind":"text","value":"","background":"white"})).is_err());
    }
    #[test]
    fn schema_rejects_duplicate_ids_and_conditions_are_data_only() {
        let a = Field::new("a", "A", Kind::Text, "");
        let f = Form {
            session: 1,
            title: String::new(),
            fields: vec![a.clone(), a],
            can_delete: false,
        };
        assert!(f.validate_schema().is_err());
        let mut field = Field::new("b", "B", Kind::Text, "");
        field.visible_when.push(Condition {
            field: "a".into(),
            one_of: vec!["yes".into()],
            any: vec![],
        });
        assert!(field.visible(&BTreeMap::from([("a".into(), "yes".into())])));
    }
}
