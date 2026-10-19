//! A small, error-tolerant declaration index for completion requests.
//!
//! Completion runs against the current editor buffers, including half-written
//! commands. It cannot depend on the last successful project analysis. This
//! scanner recognizes declaration boundaries and lexical scopes; the compiler
//! remains responsible for validating declarations and resolving invocations.
use crate::{
    analysis::{OverlaySource, SourceProvider},
    config,
    model::{Slot, Template},
    parser::{parse_name, parse_string},
    registry::procedure_template,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSymbol {
    pub name: String,
    pub global: bool,
}

#[derive(Debug, Default)]
pub struct VisibleSymbols {
    pub variables: Vec<DataSymbol>,
    pub lists: Vec<DataSymbol>,
    pub procedures: Vec<Template>,
    pub parameters: Vec<Slot>,
}

/// Return declarations visible at a UTF-8 byte offset in the current document.
/// Only the configured entry's import graph is read. A document outside that
/// graph, or one with a broken config, falls back to its own declarations.
pub fn collect(
    path: Option<&Path>,
    source: &str,
    overlays: &BTreeMap<PathBuf, String>,
    cursor: usize,
) -> VisibleSymbols {
    let Some(path) = path else {
        let file = scan(Path::new("<untitled>"), source);
        let scope = file.scope_at(cursor.min(source.len()));
        let mut index = Index::default();
        index.declarations(file.declarations);
        return index.visible(scope);
    };
    let path = crate::analysis::normalize_path(path);
    let mut buffers: BTreeMap<_, _> = overlays
        .iter()
        .map(|(path, source)| (crate::analysis::normalize_path(path), source.clone()))
        .collect();
    buffers.insert(path.clone(), source.to_owned());
    let provider = OverlaySource::new(&buffers);
    let current = scan(&path, source);
    let scope = current.scope_at(cursor.min(source.len()));

    if let Ok(config) = config::discover_for_document(&path, &provider) {
        let mut loader = Loader {
            provider: &provider,
            src_dir: &config.src_dir,
            visited: BTreeSet::new(),
            index: Index::default(),
        };
        loader.file(&config.entry);
        if loader.visited.contains(&path) {
            return loader.index.visible(scope);
        }
    }

    let mut index = Index::default();
    index.declarations(current.declarations);
    index.visible(scope)
}

#[derive(Default)]
struct ScopeSymbols {
    variables: BTreeSet<String>,
    lists: BTreeSet<String>,
    procedures: BTreeMap<String, Template>,
}

#[derive(Default)]
struct Index {
    global: ScopeSymbols,
    sprites: BTreeMap<String, ScopeSymbols>,
}

impl Index {
    fn declaration(&mut self, declaration: Declaration) {
        let (scope, name, list, procedure) = match declaration {
            Declaration::Import(_) => return,
            Declaration::Data { scope, name, list } => (scope, Some(name), list, None),
            Declaration::Procedure { scope, template } => (scope, None, false, Some(template)),
        };
        let symbols = match scope {
            Some(sprite) => self.sprites.entry(sprite).or_default(),
            None => &mut self.global,
        };
        if let Some(name) = name {
            // Repeated declarations replace their values in the compiler. The
            // completion list only needs one entry for that name and data kind.
            if list {
                symbols.lists.insert(name);
            } else {
                symbols.variables.insert(name);
            }
        }
        if let Some(template) = procedure {
            // Duplicate signatures are a diagnostic; avoid duplicate suggestions
            // while the user is in the middle of changing a definition.
            symbols
                .procedures
                .entry(template.signature())
                .or_insert(template);
        }
    }

    fn declarations(&mut self, declarations: Vec<Declaration>) {
        for declaration in declarations {
            self.declaration(declaration);
        }
    }

    fn visible(self, scope: Scope) -> VisibleSymbols {
        let mut result = VisibleSymbols {
            variables: self
                .global
                .variables
                .into_iter()
                .map(|name| DataSymbol { name, global: true })
                .collect(),
            lists: self
                .global
                .lists
                .into_iter()
                .map(|name| DataSymbol { name, global: true })
                .collect(),
            parameters: scope.parameters,
            ..VisibleSymbols::default()
        };
        let mut procedures = self.global.procedures;
        if let Some(private) = scope.sprite.and_then(|name| self.sprites.get(&name)) {
            result
                .variables
                .extend(private.variables.iter().map(|name| DataSymbol {
                    name: name.clone(),
                    global: false,
                }));
            result
                .lists
                .extend(private.lists.iter().map(|name| DataSymbol {
                    name: name.clone(),
                    global: false,
                }));
            procedures.extend(private.procedures.clone());
        }
        result.procedures = procedures.into_values().collect();
        result
    }
}

struct Loader<'a> {
    provider: &'a dyn SourceProvider,
    src_dir: &'a Path,
    visited: BTreeSet<PathBuf>,
    index: Index,
}

impl Loader<'_> {
    fn file(&mut self, path: &Path) {
        let path = self.provider.identity(path);
        if !self.visited.insert(path.clone()) {
            return;
        }
        let Ok(source) = self.provider.read_to_string(&path) else {
            return;
        };
        for declaration in scan(&path, &source).declarations {
            if let Declaration::Import(module) = declaration {
                let relative = format!("{}.sbdsl", module.replace('.', "/"));
                let nearby = path.parent().unwrap_or(Path::new(".")).join(&relative);
                let fallback = self.src_dir.join(&relative);
                if self.provider.is_file(&nearby) {
                    self.file(&nearby);
                } else if self.provider.is_file(&fallback) {
                    self.file(&fallback);
                }
            } else {
                self.index.declaration(declaration);
            }
        }
    }
}

enum Declaration {
    Import(String),
    Data {
        scope: Option<String>,
        name: String,
        list: bool,
    },
    Procedure {
        scope: Option<String>,
        template: Template,
    },
}

#[derive(Clone, Default)]
struct Scope {
    sprite: Option<String>,
    parameters: Vec<Slot>,
}

struct Region {
    start: usize,
    end: usize,
    scope: Scope,
}

#[derive(Default)]
struct FileIndex {
    declarations: Vec<Declaration>,
    regions: Vec<Region>,
}

impl FileIndex {
    fn scope_at(&self, cursor: usize) -> Scope {
        self.regions
            .iter()
            .filter(|region| region.start <= cursor && cursor <= region.end)
            .max_by_key(|region| region.start)
            .map(|region| region.scope.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Body {
    Stage,
    Sprite,
    Script,
}

struct Frame {
    body: Body,
    start: usize,
    scope: Scope,
}

fn keyword<'a>(source: &'a str, keyword: &str) -> Option<&'a str> {
    source
        .strip_prefix(keyword)
        .filter(|rest| rest.chars().next().is_none_or(syntax_space))
        .map(str::trim)
}

fn syntax_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn leading_name(source: &str) -> Option<(String, usize)> {
    let end = if source.starts_with('"') {
        quoted_end(source, 0)
    } else {
        source
            .find(|c: char| syntax_space(c) || c == '=')
            .unwrap_or(source.len())
    };
    Some((parse_name(&source[..end]).ok()?, end))
}

fn declaration_header(
    path: &Path,
    source: &str,
    start: usize,
    end: usize,
    has_body: bool,
    frames: &[Frame],
    output: &mut FileIndex,
) -> Option<(Body, Scope)> {
    let header = source[start..end].trim();
    let scope = frames
        .last()
        .map(|frame| frame.scope.clone())
        .unwrap_or_default();
    let allowed = frames.last().is_none_or(|frame| frame.body != Body::Script);
    if allowed {
        if !has_body {
            if let Some(module) = keyword(header, "import") {
                if scope.sprite.is_none()
                    && !module.ends_with(".sbdsl")
                    && module
                        .split('.')
                        .all(|name| !name.starts_with('"') && parse_name(name).is_ok())
                {
                    output.declarations.push(Declaration::Import(module.into()));
                }
            } else {
                let (global, declaration) = if let Some(rest) = keyword(header, "@global") {
                    (true, rest)
                } else {
                    (false, header)
                };
                let data = keyword(declaration, "@var")
                    .map(|rest| (false, rest))
                    .or_else(|| keyword(declaration, "@list").map(|rest| (true, rest)));
                if let Some((list, rest)) = data
                    && (!global || scope.sprite.is_some())
                    && let Some((name, end)) = leading_name(rest)
                    && (rest[end..].trim().is_empty() || rest[end..].trim().starts_with('='))
                {
                    output.declarations.push(Declaration::Data {
                        scope: if global { None } else { scope.sprite.clone() },
                        name,
                        list,
                    });
                }
            }
        } else {
            if keyword(header, "@stage") == Some("") && frames.is_empty() {
                return Some((Body::Stage, Scope::default()));
            }
            if let Some(rest) = keyword(header, "@sprite")
                && scope.sprite.is_none()
                && rest.starts_with('"')
            {
                let end = quoted_end(rest, 0);
                if let Ok(name) = parse_string(&rest[..end])
                    && matches!(rest[end..].trim(), "" | "add")
                {
                    return Some((
                        Body::Sprite,
                        Scope {
                            sprite: Some(name),
                            parameters: Vec::new(),
                        },
                    ));
                }
            }
            if let Some(header) = keyword(header, "定义自制积木") {
                let mut scope = scope;
                let id = format!("{}:{start}", path.display());
                if let Ok(template) = procedure_template(header, &id) {
                    scope.parameters = template.slots().cloned().collect();
                    output.declarations.push(Declaration::Procedure {
                        scope: scope.sprite.clone(),
                        template,
                    });
                }
                return Some((Body::Script, scope));
            }
        }
    }
    has_body.then_some((Body::Script, scope))
}

fn scan(path: &Path, original: &str) -> FileIndex {
    let source = without_comments(original);
    let mut result = FileIndex::default();
    let mut frames = Vec::<Frame>::new();
    let mut groups = Vec::<(char, usize)>::new();
    let mut start = 0;
    let mut position = 0;
    while position < source.len() {
        let c = source[position..].chars().next().unwrap();
        match c {
            '"' => position = quoted_end(&source, position),
            '/' if source[position..].starts_with("/'")
                || operator_end(&source, position, &groups).is_none() =>
            {
                position = menu_end(&source, position).0
            }
            '\n' | '\r' => {
                if groups.is_empty() || recovery_line(&source[position + 1..]) {
                    declaration_header(path, &source, start, position, false, &frames, &mut result);
                    groups.clear();
                    start = position + 1;
                }
                position += 1;
            }
            '{' => {
                if source[start..position].trim() == "@init" {
                    position = json_end(&source, position);
                } else {
                    if let Some((body, scope)) = declaration_header(
                        path,
                        &source,
                        start,
                        position,
                        true,
                        &frames,
                        &mut result,
                    ) {
                        frames.push(Frame {
                            body,
                            start: position + 1,
                            scope,
                        });
                    }
                    position += 1;
                }
                groups.clear();
                start = position;
            }
            '}' => {
                declaration_header(path, &source, start, position, false, &frames, &mut result);
                if let Some(frame) = frames.pop() {
                    result.regions.push(Region {
                        start: frame.start,
                        end: position,
                        scope: frame.scope,
                    });
                }
                groups.clear();
                position += 1;
                start = position;
            }
            '<' | '>' if operator_end(&source, position, &groups).is_some() => {
                position = operator_end(&source, position, &groups).unwrap();
            }
            '(' | '[' | '<' => {
                groups.push((
                    match c {
                        '(' => ')',
                        '[' => ']',
                        _ => '>',
                    },
                    position,
                ));
                position += 1;
            }
            ')' | ']' | '>' => {
                if groups.last().is_some_and(|(close, _)| *close == c) {
                    groups.pop();
                }
                position += 1;
            }
            _ => position += c.len_utf8(),
        }
    }
    declaration_header(path, &source, start, position, false, &frames, &mut result);
    for frame in frames {
        result.regions.push(Region {
            start: frame.start,
            end: source.len(),
            scope: frame.scope,
        });
    }
    result
}

fn recovery_line(source: &str) -> bool {
    let line = source.trim_start_matches(syntax_space);
    line.starts_with('}')
        || [
            "@var",
            "@list",
            "@global",
            "@sprite",
            "@stage",
            "@init",
            "import",
            "定义自制积木",
        ]
        .iter()
        .any(|word| keyword(line, word).is_some())
}

/// Unlike the strict parser, an unfinished string/menu consumes only its line.
fn quoted_end(source: &str, start: usize) -> usize {
    let mut escaped = false;
    for (offset, c) in source[start + 1..].char_indices() {
        let position = start + 1 + offset;
        if matches!(c, '\r' | '\n') {
            return position;
        }
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return position + 1;
        }
    }
    source.len()
}

pub(super) fn menu_end(source: &str, start: usize) -> (usize, bool) {
    let line_end = source[start..]
        .find(['\n', '\r'])
        .map_or(source.len(), |offset| start + offset);
    if !source[start..].starts_with("/'") {
        return (line_end, false);
    }
    let mut escaped = false;
    for (offset, c) in source[start + 2..line_end].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '\'' {
            let end = start + 2 + offset + 1;
            return if source[end..].starts_with('/') {
                (end + 1, true)
            } else {
                (line_end, false)
            };
        }
    }
    (line_end, false)
}

fn comment_end(source: &str, start: usize) -> usize {
    let line_end = source[start..]
        .find(['\n', '\r'])
        .map_or(source.len(), |offset| start + offset);
    if let Some(offset) = source[start + 1..line_end].find('#') {
        return start + 1 + offset + 1;
    }
    let line_start = source[..start]
        .rfind(['\n', '\r'])
        .map_or(0, |offset| offset + 1);
    let alone = source[line_start..start]
        .chars()
        .all(|c| matches!(c, ' ' | '\t'))
        && source[start + 1..line_end]
            .chars()
            .all(|c| matches!(c, ' ' | '\t'));
    if alone {
        source[line_end..]
            .find('#')
            .map_or(source.len(), |offset| line_end + offset + 1)
    } else {
        line_end
    }
}

pub(super) fn operator_end(
    source: &str,
    position: usize,
    groups: &[(char, usize)],
) -> Option<usize> {
    let &(close, open) = groups.last()?;
    if !matches!(close, ')' | '>') || explicit_group(source, open) {
        return None;
    }
    let end = crate::parser::infix_operator_at(source, open, position, true)?;
    if source[position..].starts_with('>') {
        // A plain > can close this group or compare its left value. Preserve
        // the closing boundary needed by each enclosing complete group, so
        // <@ready> in ext.<template <@ready> 1> cannot consume the next slot.
        let mut outer_end = source.len().saturating_add(1);
        let mut has_parent_boundary = false;
        for &(_, group_open) in groups {
            if let Ok(candidates) = crate::parser::scan_atom_candidates(source, group_open)
                && let Some(candidate) = candidates.into_iter().filter(|&end| end < outer_end).max()
            {
                outer_end = candidate;
                if group_open == open && has_parent_boundary && candidate == position + 1 {
                    return None;
                }
                has_parent_boundary = true;
            }
        }
    }
    Some(end)
}

fn explicit_group(source: &str, open: usize) -> bool {
    let Some(prefix) = source[..open]
        .strip_suffix('.')
        .or_else(|| source[..open].strip_suffix(".hat"))
    else {
        return false;
    };
    prefix.ends_with('"')
        && prefix.char_indices().rev().any(|(start, c)| {
            c == '"' && prefix[..start].ends_with("::") && parse_string(&prefix[start..]).is_ok()
        })
}

fn without_comments(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    let mut groups = Vec::new();
    let mut position = 0;
    while position < source.len() {
        let c = source[position..].chars().next().unwrap();
        match c {
            '"' => position = quoted_end(source, position),
            '/' if source[position..].starts_with("/'")
                || operator_end(source, position, &groups).is_none() =>
            {
                position = menu_end(source, position).0
            }
            '#' => {
                let end = comment_end(source, position);
                for byte in &mut bytes[position..end] {
                    if !matches!(*byte, b'\n' | b'\r') {
                        *byte = b' ';
                    }
                }
                position = end;
            }
            '<' | '>' if operator_end(source, position, &groups).is_some() => {
                position = operator_end(source, position, &groups).unwrap()
            }
            '(' | '[' | '<' => {
                groups.push((
                    match c {
                        '(' => ')',
                        '[' => ']',
                        _ => '>',
                    },
                    position,
                ));
                position += 1;
            }
            ')' | ']' | '>' => {
                if groups.last().is_some_and(|(close, _)| *close == c) {
                    groups.pop();
                }
                position += 1;
            }
            _ => position += c.len_utf8(),
        }
    }
    String::from_utf8(bytes).expect("whole comments replaced by ASCII spaces")
}

fn json_end(source: &str, start: usize) -> usize {
    let mut depth = 1;
    let mut position = start + 1;
    while position < source.len() {
        match source[position..].chars().next().unwrap() {
            '"' => position = quoted_end(source, position),
            '{' => {
                depth += 1;
                position += 1;
            }
            '}' => {
                position += 1;
                depth -= 1;
                if depth == 0 {
                    return position;
                }
            }
            c => position += c.len_utf8(),
        }
    }
    source.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ArgType;

    fn here(source: &str) -> VisibleSymbols {
        let position = source.find('|').expect("cursor marker");
        let source = source.replacen('|', "", 1);
        collect(None, &source, &BTreeMap::new(), position)
    }

    fn names(symbols: &[DataSymbol]) -> BTreeSet<(String, bool)> {
        symbols
            .iter()
            .map(|symbol| (symbol.name.clone(), symbol.global))
            .collect()
    }

    #[test]
    fn unfinished_blocks_preserve_data_scope_and_add_declarations() {
        let source = r#"
@var score = 1
@var score = 2
@list score = []
@sprite "玩家" {
    @init {}
    @var score = 3
    @var "" = 4
    @list bag = []
    @global @var level = 1
}
@sprite "其他" {
    @init {}
    @var hidden = 4
}
@sprite "玩家" add {
    @var health = 100
    当绿旗被点击 {
        [说 $sc|
"#;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("score".into(), true),
                ("score".into(), false),
                ("".into(), false),
                ("level".into(), true),
                ("health".into(), false),
            ])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("score".into(), true), ("bag".into(), false),])
        );
    }

    #[test]
    fn procedure_scope_parameters_and_signature_shadowing_are_lexical() {
        let source = r#"
@var shared
定义自制积木 回答 @public <@ok> {
    [说 @public]
}
定义自制积木 公共调用 @publicArg {
    [说 $|]
}
@sprite "玩家" {
    @init {}
    @var secret
    定义自制积木 回答 @private <@yes> {
        [说 @private]
    }
    定义自制积木 本地调用 @"文本 名" <@"条件 名"> @warp {
        [说 @"文本 名"]
    }
}
"#;
        let public = here(source);
        assert_eq!(
            names(&public.variables),
            BTreeSet::from([("shared".into(), true)])
        );
        assert_eq!(
            public
                .parameters
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["publicArg"]
        );
        assert_eq!(public.procedures.len(), 2);
        assert!(
            public
                .procedures
                .iter()
                .flat_map(Template::slots)
                .any(|s| s.name == "public")
        );

        let source = source
            .replace("$|", "$")
            .replace("[说 @\"文本 名\"]", "[说 @|]");
        let private = here(&source);
        assert_eq!(private.procedures.len(), 3);
        assert!(
            private
                .procedures
                .iter()
                .flat_map(Template::slots)
                .any(|s| s.name == "private")
        );
        assert!(
            !private
                .procedures
                .iter()
                .flat_map(Template::slots)
                .any(|s| s.name == "public")
        );
        assert_eq!(
            private
                .parameters
                .iter()
                .map(|s| (s.name.as_str(), s.arg_type))
                .collect::<Vec<_>>(),
            [("文本 名", ArgType::Any), ("条件 名", ArgType::Boolean),]
        );
        assert!(names(&private.variables).contains(&("secret".into(), false)));
    }

    #[test]
    fn comments_strings_menus_json_and_script_declarations_do_not_leak() {
        let source = r##"
# @var line_comment
# @var inline_comment # @var real_inline = 1
#
@var block_comment
@sprite "fake" {
    @var inside_comment
}
#
@var color = "#123456"
@sprite "玩家" {
    @init {
        "sounds": [{"name": "@var json_name", "path": "a/#file.wav"}],
        "invalid": {
            @var json_statement
        }
    }
    @var local
    当绿旗被点击 {
        [说 "@var string"]
        [说 /'@var menu'/]
        [说 /'* # @var star_menu #'/]
        [说 /'a\'/# @var escaped_quote'/]
        [说 /'a\\/# @var escaped_backslash'/]
        @var illegal_script_declaration
        定义自制积木 假的 @argument {
            @var illegal_nested
        }
        [说 (1 / # division # 2)]
        [说 $|]
    }
}
@var after = 1
"##;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("real_inline".into(), true),
                ("color".into(), true),
                ("after".into(), true),
                ("local".into(), false),
            ])
        );
        assert!(symbols.procedures.is_empty());
        assert!(symbols.parameters.is_empty());
    }

    #[test]
    fn new_infix_operators_preserve_private_procedure_scope() {
        for expression in [
            "<$local > @steps>",
            "<$local>@steps>",
            "<$local\n>\n@steps>",
            "<($local / @steps) > 0>",
            "<($local/@steps)>0>",
            "<($local\n/\n@steps)>0>",
            "<($local mod @steps)=0>",
            "<($local+1)<(@steps-1)>",
            "<<@ready>><false>>",
            "ext.<模板 <@ready> 1>",
            "ext.<模板 <@ready>\n1>",
            "ext.<模板 <$local>@steps> 1>",
            "ext.<模板 ext.<模板 <@ready> 1> 2>",
        ] {
            let source = format!(
                r#"
@var shared = 1
@sprite "玩家😀" {{
    @init {{}}
    @var local = 2
    @list bag = []
    定义自制积木 测试 @steps <@ready> {{
        如果 {expression} 那么 {{
            [说 $|]
        }}
    }}
}}
@sprite "其他" {{
    @init {{}}
    @var hidden = 3
}}
@var after = 4
"#,
            );
            let symbols = here(&source);
            assert_eq!(
                names(&symbols.variables),
                BTreeSet::from([
                    ("shared".into(), true),
                    ("local".into(), false),
                    ("after".into(), true),
                ]),
                "{expression}",
            );
            assert_eq!(
                names(&symbols.lists),
                BTreeSet::from([("bag".into(), false)])
            );
            assert_eq!(
                symbols
                    .parameters
                    .iter()
                    .map(|slot| slot.name.as_str())
                    .collect::<Vec<_>>(),
                ["steps", "ready"],
                "{expression}",
            );
        }
    }

    #[test]
    fn division_comments_and_menu_literals_do_not_corrupt_following_declarations() {
        let source = r##"
@sprite "玩家" {
    @init {}
    @var local = 1
    当绿旗被点击 {
        [说 ($local / # @var not_a_declaration # 2)]
        [说 ($local
            /
            # @var also_not_a_declaration
            2)]
        [说 <($local/2)>1>]
        [说 /'*'/]
        [说 /'/# @var menu'/]
    }
    @var after = 2
    当绿旗被点击 { [说 $|] }
}
@var outside = 3
"##;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("local".into(), false),
                ("after".into(), false),
                ("outside".into(), true),
            ]),
        );
    }

    #[test]
    fn unfinished_menus_hide_fake_symbols_and_recover_on_the_next_line() {
        for menu in [
            "/'@var fake",
            "/'escaped\\' / # @var fake",
            "/'escaped\\\\ / # @var fake",
            "/'closed quote' # @var fake",
            "/'still contains / @var fake",
        ] {
            let source = format!("@var before = 1\n[说 {menu}\n@var after = 2\n[说 $|]");
            let symbols = here(&source);
            assert_eq!(
                names(&symbols.variables),
                BTreeSet::from([("before".into(), true), ("after".into(), true)]),
                "{menu}",
            );
        }
    }

    #[test]
    fn recovers_after_bad_statements_and_handles_multiline_arrays() {
        let source = r#"
@var before
[unclosed command
@var after
@list multi = [
    1,
    "@var not_data",
    2
]
@var "a b" = "unterminated
@var 文本
@stage {
    @var staged
}
|
"#;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("before".into(), true),
                ("after".into(), true),
                ("a b".into(), true),
                ("文本".into(), true),
                ("staged".into(), true),
            ])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("multi".into(), true)])
        );
    }

    #[test]
    fn current_document_scope_ends_at_the_closing_brace() {
        let source = "@var public\n@sprite \"cat\" {\n@init {}\n@var private\n}\n|";
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([("public".into(), true)])
        );
        assert!(symbols.parameters.is_empty());
    }

    #[test]
    fn project_entry_imports_overlays_and_cycles_share_sprite_scope() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.sbdsl");
        let module = dir.path().join("src/feature.sbdsl");
        let dependency = dir.path().join("src/helper.sbdsl");
        let current = "import helper\n@sprite \"cat\" add {\n@var unsaved\n当绿旗被点击 {\n[说 $";
        std::fs::write(
            dir.path().join("sbdsl.buildsb"),
            r#"{"entry":"wrong.sbdsl"}"#,
        )
        .unwrap();
        let overlays = BTreeMap::from([
            (
                dir.path().join("sbdsl.buildsb"),
                r#"{"entry":"main.sbdsl","srcDir":"src"}"#.into(),
            ),
            (
                main.clone(),
                "@var fromEntry\n@sprite \"cat\" {\n@init {}\n@var private\n}\nimport feature"
                    .into(),
            ),
            (module.clone(), "@var stale".into()),
            (
                dependency,
                "import feature\n@list imported\n定义自制积木 帮助 @n {\n[说 @n]\n}".into(),
            ),
            (dir.path().join("unrelated.sbdsl"), "@var unrelated".into()),
        ]);
        let symbols = collect(Some(&module), current, &overlays, current.len());
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("fromEntry".into(), true),
                ("private".into(), false),
                ("unsaved".into(), false),
            ])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("imported".into(), true)])
        );
        assert_eq!(symbols.procedures.len(), 1);
        assert_eq!(symbols.procedures[0].slots().next().unwrap().name, "n");
    }

    #[test]
    fn relative_import_wins_over_source_directory_and_ids_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("nested/main.sbdsl");
        let source = "import helper\nimport extra\n定义自制积木 主积木 {\n}\n";
        let overlays = BTreeMap::from([
            (
                dir.path().join("sbdsl.buildsb"),
                r#"{"entry":"nested/main.sbdsl","srcDir":"src"}"#.into(),
            ),
            (
                dir.path().join("nested/helper.sbdsl"),
                "@var nearby\n定义自制积木 附近积木 {\n}".into(),
            ),
            (
                dir.path().join("src/helper.sbdsl"),
                "@var fallbackMustNotAppear".into(),
            ),
            (
                dir.path().join("src/extra.sbdsl"),
                "@var fallback\n定义自制积木 目录积木 {\n}".into(),
            ),
        ]);
        let symbols = collect(Some(&main), source, &overlays, source.len());
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([("nearby".into(), true), ("fallback".into(), true),])
        );
        let ids: BTreeSet<_> = symbols
            .procedures
            .iter()
            .filter_map(|p| p.procedure.clone())
            .collect();
        assert_eq!(ids.len(), 3);
        let repeated = collect(Some(&main), source, &overlays, source.len());
        assert_eq!(symbols.procedures, repeated.procedures);
    }

    #[test]
    fn broken_config_or_unrelated_document_does_not_borrow_entry_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let source = "@var own\n";
        let module = dir.path().join("unrelated.sbdsl");
        let config = dir.path().join("sbdsl.buildsb");
        let mut overlays = BTreeMap::from([
            (config.clone(), "{".into()),
            (dir.path().join("main.sbdsl"), "@var otherProject".into()),
        ]);
        for config_source in [
            "{",
            r#"{"entry":"main.sbdsl"}"#,
            r#"{"entry":"https://invalid/main.sbdsl"}"#,
        ] {
            overlays.insert(config.clone(), config_source.into());
            let symbols = collect(Some(&module), source, &overlays, source.len());
            assert_eq!(
                names(&symbols.variables),
                BTreeSet::from([("own".into(), true)])
            );
        }
    }

    #[test]
    fn quoted_names_and_utf8_offsets_do_not_change_scope() {
        let source = "@var \"😀 分数\"\n@sprite \"猫😀\" {\n@init {}\n@list \"背包 东西\"\n定义自制积木 告诉 @\"\" {\n[说 $";
        let symbols = collect(None, source, &BTreeMap::new(), source.len());
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([("😀 分数".into(), true)])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("背包 东西".into(), false)])
        );
        assert_eq!(symbols.parameters[0].name, "");
    }
}
