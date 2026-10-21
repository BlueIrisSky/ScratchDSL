use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Span {
    pub file: String,
    pub line: usize,
    pub column: usize,
    /// Half-open UTF-8 byte range in the original, unmodified source.
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn from_source(path: &Path, source: &str, start: usize, end: usize) -> Self {
        let start = source.floor_char_boundary(start);
        let end = source.floor_char_boundary(end).max(start);
        let prefix = &source[..start];
        Self {
            file: path.display().to_string(),
            line: prefix.bytes().filter(|&b| b == b'\n').count() + 1,
            column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
            start,
            end,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub name: String,
    pub path: PathBuf,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub rotation_center_x: Option<f64>,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub rotation_center_y: Option<f64>,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub bitmap_resolution: Option<u32>,
}

fn present_value<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SpriteInit {
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub direction: f64,
    pub visible: bool,
    pub rotation_style: String,
    pub costumes: Vec<Asset>,
    pub sounds: Vec<Asset>,
}

impl Default for SpriteInit {
    fn default() -> Self {
        Self {
            x: 0.,
            y: 0.,
            size: 100.,
            direction: 90.,
            visible: true,
            rotation_style: "all around".into(),
            costumes: Vec::new(),
            sounds: Vec::new(),
        }
    }
}

impl SpriteInit {
    /// Validate static target properties without resolving resource file paths.
    pub fn validate(&self) -> crate::Result<()> {
        for (name, value) in [
            ("x", self.x),
            ("y", self.y),
            ("size", self.size),
            ("direction", self.direction),
        ] {
            if !value.is_finite() {
                return Err(crate::Error::new(format!("init {name} must be finite")));
            }
        }
        if self.size <= 0. {
            return Err(crate::Error::new("init size must be positive"));
        }
        if !["all around", "left-right", "don't rotate"].contains(&self.rotation_style.as_str()) {
            return Err(crate::Error::new("invalid init rotationStyle"));
        }
        for (kind, assets) in [("costumes", &self.costumes), ("sounds", &self.sounds)] {
            let mut names = BTreeSet::new();
            for asset in assets {
                if kind == "sounds"
                    && (asset.rotation_center_x.is_some()
                        || asset.rotation_center_y.is_some()
                        || asset.bitmap_resolution.is_some())
                {
                    return Err(crate::Error::new(
                        "sounds cannot specify costume rotation centers or bitmapResolution",
                    ));
                }
                if asset.name.is_empty() {
                    return Err(crate::Error::new(format!(
                        "init {kind} names cannot be empty"
                    )));
                }
                if !names.insert(&asset.name) {
                    return Err(crate::Error::new(format!(
                        "duplicate init {kind} name: {}",
                        asset.name
                    )));
                }
                if asset.path.as_os_str().is_empty() {
                    return Err(crate::Error::new(format!(
                        "init {kind} resource path cannot be empty"
                    )));
                }
                if asset.bitmap_resolution == Some(0) {
                    return Err(crate::Error::new("bitmapResolution must be positive"));
                }
                if asset.rotation_center_x.is_some_and(|v| !v.is_finite())
                    || asset.rotation_center_y.is_some_and(|v| !v.is_finite())
                {
                    return Err(crate::Error::new(
                        "resource rotation centers must be finite",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    Import {
        module: String,
        span: Span,
    },
    Data {
        name: String,
        value: Value,
        list: bool,
        global: bool,
        span: Span,
    },
    Sprite {
        name: String,
        add: bool,
        init: Option<SpriteInit>,
        items: Vec<Item>,
        span: Span,
    },
    Stage(Vec<Item>),
    Procedure {
        header: String,
        body: Vec<Statement>,
        span: Span,
    },
    Script {
        header: String,
        body: Vec<Statement>,
        span: Span,
    },
}

#[derive(Debug, Clone)]
pub enum Statement {
    ListAssignment {
        name: String,
        global: bool,
        values: Vec<Value>,
        span: Span,
    },
    Command {
        source: String,
        span: Span,
    },
    Control {
        header: String,
        body: Vec<Statement>,
        else_body: Option<Vec<Statement>>,
        span: Span,
    },
    ExtensionControl {
        header: String,
        branches: Vec<Vec<Statement>>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Shape {
    Command,
    Reporter,
    Boolean,
    Hat,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ArgType {
    Any,
    String,
    Number,
    Boolean,
    Color,
    Angle,
    Variable,
    List,
    Broadcast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Storage {
    Input,
    Field,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shadow {
    pub opcode: String,
    pub field: String,
    pub primitive: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Menu {
    /// None denotes a dynamic menu; pairs are (display text, stored value).
    pub items: Option<Vec<(String, String)>>,
    pub accept_reporters: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Slot {
    pub name: String,
    pub arg_type: ArgType,
    pub storage: Storage,
    pub default: Value,
    pub shadow: Option<Shadow>,
    pub menu: Option<Menu>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Token {
    Literal(String),
    Slot(Slot),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Template {
    pub opcode: String,
    pub shape: Shape,
    pub namespace: Option<String>,
    pub extension_id: Option<String>,
    pub local_opcode: Option<String>,
    pub tokens: Vec<Token>,
    /// Number of ordered C-block substacks. Zero for blocks without bodies.
    pub branch_count: usize,
    pub sprite_only: bool,
    pub stage_only: bool,
    /// Stable source-level procedure identity. None for builtins/extensions.
    pub procedure: Option<String>,
    pub warp: bool,
}
impl Template {
    pub fn slots(&self) -> impl Iterator<Item = &Slot> {
        self.tokens.iter().filter_map(|t| {
            if let Token::Slot(s) = t {
                Some(s)
            } else {
                None
            }
        })
    }
    pub fn signature(&self) -> String {
        self.tokens
            .iter()
            .map(|t| match t {
                Token::Literal(s) => format!("L{}:{}", s.len(), s),
                Token::Slot(s) => {
                    if s.arg_type == ArgType::Boolean {
                        "B".into()
                    } else {
                        "S".into()
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("|")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Expr {
    Number(f64),
    String(String),
    Menu(String),
    Symbol {
        name: String,
        list: bool,
        global: bool,
    },
    Param {
        name: String,
        boolean: bool,
    },
    Bool(bool),
    EmptyBool,
    Call(Box<Invocation>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Invocation {
    pub template: Template,
    pub args: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct Procedure {
    pub template: Template,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug, Clone, Default)]
pub struct Target {
    pub name: String,
    pub init: Option<SpriteInit>,
    pub declaration_span: Option<Span>,
    pub variables: BTreeMap<String, Value>,
    pub lists: BTreeMap<String, Value>,
    pub procedures: Vec<Procedure>,
    pub scripts: Vec<(String, Vec<Statement>, Span)>,
}

#[derive(Debug, Clone, Default)]
pub struct Project {
    pub stage: Target,
    pub sprites: Vec<Target>,
    pub public_procedures: Vec<Procedure>,
}

#[derive(Debug, Clone, Default)]
pub struct GeneratedProject {
    pub json: Value,
    pub assets: BTreeMap<String, Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_init_rejects_nonfinite_programmatic_properties() {
        for init in [
            SpriteInit {
                x: f64::NAN,
                ..SpriteInit::default()
            },
            SpriteInit {
                y: f64::INFINITY,
                ..SpriteInit::default()
            },
            SpriteInit {
                size: f64::NEG_INFINITY,
                ..SpriteInit::default()
            },
            SpriteInit {
                direction: f64::NAN,
                ..SpriteInit::default()
            },
        ] {
            assert!(init.validate().is_err());
        }
    }

    #[test]
    fn optional_asset_metadata_serializes_by_omission_and_allows_cross_kind_names() {
        let asset = Asset {
            name: "same name".into(),
            path: "a.svg".into(),
            rotation_center_x: None,
            rotation_center_y: None,
            bitmap_resolution: None,
        };
        let init = SpriteInit {
            costumes: vec![asset.clone()],
            sounds: vec![Asset {
                path: "a.wav".into(),
                ..asset
            }],
            ..SpriteInit::default()
        };
        init.validate().unwrap();
        let json = serde_json::to_string(&init).unwrap();
        assert!(!json.contains("null"));
        serde_json::from_str::<SpriteInit>(&json)
            .unwrap()
            .validate()
            .unwrap();
    }
}
