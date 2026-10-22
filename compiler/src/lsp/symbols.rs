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
    parser::{is_data_name, keyword_rest, parse_string},
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
    pub(super) scopes: Option<Vec<LayoutScope>>,
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
    let cursor = cursor.min(source.len());
    let Some(path) = path else {
        let (declarations, scope, scopes) = scan_at(Path::new("<untitled>"), source, cursor);
        let mut index = Index::default();
        index.declarations(declarations);
        return index.visible(scope, Some(scopes));
    };
    let path = crate::analysis::normalize_path(path);
    let mut provider = OverlaySource::new(overlays);
    provider.insert(path.clone(), source.to_owned());
    let (declarations, scope, scopes) = scan_at(&path, source, cursor);

    if let Ok(config) = config::discover_for_document(&path, &provider) {
        let mut loader = Loader {
            provider: &provider,
            src_dir: &config.src_dir,
            visited: BTreeSet::new(),
            index: Index::default(),
        };
        loader.file(&config.entry);
        if loader.visited.contains(&path) {
            return loader.index.visible(scope, Some(scopes));
        }
    }

    let mut index = Index::default();
    index.declarations(declarations);
    index.visible(scope, Some(scopes))
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
    inferred: Vec<(Option<String>, String, bool, bool)>,
}

impl Index {
    fn declaration(&mut self, declaration: Declaration) {
        let (scope, name, list, procedure) = match declaration {
            Declaration::Import(_) => return,
            Declaration::Inferred {
                scope,
                name,
                list,
                global,
            } => {
                self.inferred.push((scope, name, list, global));
                return;
            }
            Declaration::Data { scope, name, list } => (scope, Some(name), list, None),
            Declaration::Procedure { scope, template } => (scope, None, false, Some(template)),
        };
        if let Some(name) = &name {
            let opposite = |symbols: &ScopeSymbols| {
                if list {
                    symbols.variables.contains(name)
                } else {
                    symbols.lists.contains(name)
                }
            };
            let conflict = match &scope {
                Some(sprite) => {
                    opposite(&self.global) || self.sprites.get(sprite).is_some_and(opposite)
                }
                None => opposite(&self.global) || self.sprites.values().any(opposite),
            };
            if conflict {
                return;
            }
        }
        let symbols = match scope {
            Some(sprite) => self.sprites.entry(sprite).or_default(),
            None => &mut self.global,
        };
        if let Some(name) = name {
            // Repeated assignments retain the first data kind. Invalid changes
            // are diagnosed by the binder and must not alter completion kinds.
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

    fn visible(mut self, scope: Scope, scopes: Option<Vec<LayoutScope>>) -> VisibleSymbols {
        for (sprite, name, list, global) in std::mem::take(&mut self.inferred) {
            let private = !global
                && sprite
                    .as_ref()
                    .and_then(|sprite| self.sprites.get(sprite))
                    .is_some_and(|symbols| {
                        symbols.variables.contains(&name) || symbols.lists.contains(&name)
                    });
            self.declaration(Declaration::Data {
                scope: if private { sprite } else { None },
                name,
                list,
            });
        }
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
            scopes,
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
        for declaration in scan_declarations(&path, &source) {
            if let Declaration::Import(module) = declaration {
                if let Ok(target) =
                    crate::project::resolve_module_path(&path, self.src_dir, &module, self.provider)
                {
                    self.file(&target);
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
    Inferred {
        scope: Option<String>,
        name: String,
        list: bool,
        global: bool,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Body {
    Stage,
    Sprite,
    Script,
    Init,
}

struct Frame {
    body: Body,
    indent: usize,
    scope: Scope,
}

fn declaration_header(
    path: &Path,
    source: &str,
    start: usize,
    end: usize,
    has_body: bool,
    frames: &[Frame],
    declarations: &mut Vec<Declaration>,
) -> Option<(Body, Scope)> {
    let header = source[start..end].trim();
    let scope = frames
        .last()
        .map(|frame| frame.scope.clone())
        .unwrap_or_default();
    let allowed = frames
        .last()
        .is_none_or(|frame| !matches!(frame.body, Body::Script | Body::Init));
    if !has_body
        && let Some(assignment) = crate::parser::assignment_parts(header)
        && let Ok((name, global)) = crate::parser::parse_reference(assignment.target)
    {
        let list = assignment.value.starts_with('{');
        if allowed && assignment.operator == "=" {
            declarations.push(Declaration::Data {
                scope: if global { None } else { scope.sprite.clone() },
                name,
                list,
            });
        } else if frames
            .last()
            .is_some_and(|frame| frame.body == Body::Script)
        {
            declarations.push(Declaration::Inferred {
                scope: scope.sprite.clone(),
                name,
                list,
                global,
            });
        }
    }
    if allowed {
        if !has_body {
            if let Some(module) = keyword_rest(header, "import")
                && scope.sprite.is_none()
                && !module.ends_with(".sbdsl")
                && module.split('.').all(is_data_name)
            {
                declarations.push(Declaration::Import(module.into()));
            }
        } else {
            if keyword_rest(header, "stage") == Some("") && frames.is_empty() {
                return Some((Body::Stage, Scope::default()));
            }
            if let Some(rest) = keyword_rest(header, "sprite")
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
            if keyword_rest(header, "init") == Some("") {
                return Some((Body::Init, scope));
            }
            if let Some(header) = keyword_rest(header, "def") {
                let mut scope = scope;
                let id = format!("{}:{start}", path.display());
                if let Ok(template) = procedure_template(header, &id) {
                    scope.parameters = template.slots().cloned().collect();
                    declarations.push(Declaration::Procedure {
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

fn scan_declarations(path: &Path, original: &str) -> Vec<Declaration> {
    let cleaned = without_comments(original);
    let continuations = continuation_lines(&cleaned);
    scan_indented(path, &cleaned, &continuations, None).0
}

fn scan_at(
    path: &Path,
    original: &str,
    cursor: usize,
) -> (Vec<Declaration>, Scope, Vec<LayoutScope>) {
    let cleaned = without_comments(original);
    let continuations = continuation_lines(&cleaned);
    let scopes = layout_scopes_from(&cleaned, &continuations, cursor);
    let active_start = scopes.last().map(|scope| scope.start);
    let (declarations, scope) = scan_indented(path, &cleaned, &continuations, active_start);
    (declarations, scope, scopes)
}

/// Physical indentation is enough to recover declaration scopes even while a
/// command or expression is unfinished. Literal braces never create scopes.
fn scan_indented(
    path: &Path,
    source: &str,
    continuations: &BTreeSet<usize>,
    active_start: Option<usize>,
) -> (Vec<Declaration>, Scope) {
    let mut declarations = Vec::new();
    let mut active_scope = Scope::default();
    let mut frames = Vec::<Frame>::new();
    let mut offset: usize = 0;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !continuations.contains(&offset) {
            let indent = indentation_width(line);
            while frames.last().is_some_and(|frame| indent <= frame.indent) {
                frames.pop();
            }
            if frames.last().is_none_or(|frame| frame.body != Body::Init) {
                let end = offset + line.trim_end().len();
                let body = is_layout_header(trimmed);
                if let Some((body, scope)) = declaration_header(
                    path,
                    source,
                    offset,
                    end - usize::from(body),
                    body,
                    &frames,
                    &mut declarations,
                ) {
                    if active_start == Some(end) {
                        active_scope = scope.clone();
                    }
                    frames.push(Frame {
                        body,
                        indent,
                        scope,
                    });
                }
            }
        }
        offset += line.len();
    }
    (declarations, active_scope)
}

#[derive(Debug, Clone)]
pub(super) struct LayoutScope {
    pub header: String,
    pub start: usize,
    indent: usize,
}

fn indentation_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

fn is_layout_header(line: &str) -> bool {
    line.ends_with(':') && !line.starts_with(['\'', '[', '(', '<', '{', '#'])
}

/// Line starts inside an unfinished atom are continuations, even if the user
/// has not indented them yet. A fresh declaration recovers from a broken atom.
fn continuation_lines(source: &str) -> BTreeSet<usize> {
    let mut result = BTreeSet::new();
    let mut groups = Vec::new();
    let mut position = 0;
    while position < source.len() {
        let c = source[position..].chars().next().unwrap();
        if matches!(c, '<' | '>')
            && let Some(end) = operator_end(source, position, &groups)
        {
            position = end;
            continue;
        }
        match c {
            '"' => position = quoted_end(source, position),
            '\'' => position = menu_end(source, position).0,
            '\n' => {
                position += 1;
                let next = source[position..].split('\n').next().unwrap_or("").trim();
                let recovery = crate::parser::assignment_parts(next).is_some()
                    || [
                        "var", "list", "global", "stage", "sprite", "init", "piece", "def",
                        "import", "start", "if", "elif", "else", "loop", "while", "end",
                    ]
                    .iter()
                    .any(|word| {
                        keyword_rest(next.trim_end_matches(':').trim_end(), word).is_some()
                    });
                if recovery {
                    groups.clear();
                } else if !groups.is_empty() {
                    result.insert(position);
                }
            }
            '(' | '[' | '<' | '{' => {
                groups.push((
                    match c {
                        '(' => ')',
                        '[' => ']',
                        '{' => '}',
                        _ => '>',
                    },
                    position,
                ));
                position += 1;
            }
            ')' | ']' | '>' | '}' => {
                if groups.last().is_some_and(|(close, _)| *close == c) {
                    groups.pop();
                }
                position += 1;
            }
            _ => position += c.len_utf8(),
        }
    }
    result
}

/// Describe the current indentation scope without requiring valid statements.
/// The cursor's indentation also closes a preceding incomplete script.
pub(super) fn layout_scopes(source: &str, cursor: usize) -> Vec<LayoutScope> {
    let source = without_comments(source);
    let continuations = continuation_lines(&source);
    layout_scopes_from(&source, &continuations, cursor)
}

fn layout_scopes_from(
    source: &str,
    continuations: &BTreeSet<usize>,
    cursor: usize,
) -> Vec<LayoutScope> {
    let mut scopes = Vec::<LayoutScope>::new();
    let mut offset: usize = 0;
    for line in source.split_inclusive('\n') {
        let current = cursor < offset + line.len()
            || (cursor == source.len()
                && offset + line.len() == source.len()
                && !line.ends_with('\n'));
        let visible = if current {
            &line[..cursor.saturating_sub(offset).min(line.len())]
        } else {
            line
        };
        if !visible.trim().is_empty() || current {
            let indent = indentation_width(visible);
            if !continuations.contains(&offset) {
                while scopes.last().is_some_and(|scope| indent <= scope.indent) {
                    scopes.pop();
                }
            }
            if current {
                break;
            }
            if !continuations.contains(&offset)
                && scopes
                    .last()
                    .is_none_or(|scope| keyword_rest(&scope.header, "init") != Some(""))
                && is_layout_header(line.trim())
            {
                scopes.push(LayoutScope {
                    header: line.trim().trim_end_matches(':').trim_end().into(),
                    start: offset + line.trim_end().len(),
                    indent,
                });
            }
        }
        offset += line.len();
    }
    if cursor == source.len() && source.ends_with('\n') && !continuations.contains(&cursor) {
        scopes.clear();
    }
    scopes
}

/// Unlike the strict parser, an unfinished string/menu consumes only its line.
pub(super) fn quoted_end(source: &str, start: usize) -> usize {
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
    let content = start + 1;
    let mut escaped = false;
    for (offset, c) in source[content..line_end].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '\'' {
            let end = content + offset + 1;
            return (end, true);
        }
    }
    (line_end, false)
}

pub(super) fn operator_end(
    source: &str,
    position: usize,
    groups: &[(char, usize)],
) -> Option<usize> {
    let &(close, open) = groups.last()?;
    if !matches!(close, ')' | '>') || explicit_group(source, open) || source[..open].ends_with('.')
    {
        return None;
    }
    let end = crate::parser::infix_operator_at(source, open, position, true)?;
    if source[position..].starts_with('>') {
        // A plain > can close this group or compare its left value. Preserve
        // the closing boundary needed by each enclosing complete group, so
        // <@ready> in <ext.template <@ready>(1)> cannot consume the next slot.
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
    let prefix = &source[..open];
    prefix.ends_with('"')
        && prefix.char_indices().rev().any(|(start, c)| {
            c == '"' && prefix[..start].ends_with("::") && parse_string(&prefix[start..]).is_ok()
        })
}

fn without_comments(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    let mut position = 0;
    while position < source.len() {
        let c = source[position..].chars().next().unwrap();
        match c {
            '"' => position = quoted_end(source, position),
            '\'' => position = menu_end(source, position).0,
            '#' => {
                let end = crate::notation::comment_end(source, position).unwrap_or(source.len());
                for byte in &mut bytes[position..end] {
                    if !matches!(*byte, b'\n' | b'\r') {
                        *byte = b' ';
                    }
                }
                position = end;
            }
            _ => position += c.len_utf8(),
        }
    }
    String::from_utf8(bytes).expect("whole comments replaced by ASCII spaces")
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
    fn indentation_tracks_private_data_parameters_and_incomplete_commands() {
        let source = r#"
shared = 1
history = {"start", 1}
sprite "玩家":
    init:
        x: 0
        costumes: [{name: "var fake", path: "a.svg"}]
    score = 2
    bag = {}
    global:level = 1
    def 前进 @steps <@ready>:
        if <@ready>:
            说 'text # with }'
            移动 (@steps + 1) 步
            [说 |
sprite "其他":
    init:
        pass
    hidden = 1
"#;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("shared".into(), true),
                ("level".into(), true),
                ("score".into(), false),
            ])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("history".into(), true), ("bag".into(), false)])
        );
        assert_eq!(
            symbols
                .parameters
                .iter()
                .map(|slot| slot.name.as_str())
                .collect::<Vec<_>>(),
            ["steps", "ready"]
        );
        assert_eq!(symbols.procedures.len(), 1);
    }

    #[test]
    fn dedented_cursor_does_not_keep_a_previous_procedures_parameters() {
        let source =
            "sprite \"玩家\":\n    local = 1\n    def 测试 @value:\n        说 (@value)\n    |";
        let symbols = here(source);
        assert!(symbols.parameters.is_empty());
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([("local".into(), false)])
        );
        let symbols = here(&source.replace("    |", "|"));
        assert!(symbols.variables.is_empty());
        assert!(symbols.parameters.is_empty());
    }

    #[test]
    fn unindented_expression_continuations_preserve_sprite_and_parameter_scope() {
        for expression in ["<score\n>\n|>", "((score)+\n(@n)/\n|)"] {
            let source = format!(
                "shared = 1\nsprite \"玩家\":\n    score = 2\n    def 示例 @n:\n        [说 {expression}]\n"
            );
            let symbols = here(&source);
            assert!(names(&symbols.variables).contains(&("score".into(), false)));
            assert_eq!(symbols.parameters[0].name, "n");
        }
    }

    #[test]
    fn unfinished_blocks_preserve_data_scope_and_add_declarations() {
        let source = r#"score = 1
score = 2
score = {}
sprite "玩家" :
    init :
        pass
    score = 3
    placeholder = 4
    bag = {}
    global:level = 1
sprite "其他" :
    init :
        pass
    hidden = 4
sprite "玩家" add :
    health = 100
    start :
        [说 sc|
"#;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("score".into(), true),
                ("score".into(), false),
                ("placeholder".into(), false),
                ("level".into(), true),
                ("health".into(), false),
            ])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("bag".into(), false)])
        );
    }

    #[test]
    fn procedure_scope_parameters_and_signature_shadowing_are_lexical() {
        let source = r#"shared = 0
def 回答 @public <@ok> :
    说 (@public)
def 公共调用 @publicArg :
    说 |
sprite "玩家" :
    init :
        pass
    secret = 0
    def 回答 @private <@yes> :
        说 (@private)
    def 本地调用 @"文本 名" <@"条件 名"> @warp :
        说 (@"文本 名")
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
            .replace("|", "")
            .replace("说 (@\"文本 名\")", "[说 @|]");
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
        let source = r##"# var line_comment
# var inline_comment # real_inline = 1
#
block_comment = 0
sprite "fake" :
    inside_comment = 0
#
color = "#123456"
sprite "玩家" :
    init :
        "sounds": [{"name": "@var json_name", "path": "a/#file.wav"}],
        "invalid": {
        json_statement = 0
        }
    local = 0
    start :
        说 ("@var string")
        说 '@var menu'
        说 '* # @var star_menu #'
        说 'a\'/# @var escaped_quote'
        说 'a\\/# @var escaped_backslash'
        var illegal_script_declaration
        def 假的 @argument :
            var illegal_nested
        说 (1 / 2)
        说 |
after = 1
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
    fn extension_control_branches_preserve_private_parameters_and_data_scope() {
        for header in [
            "ext.选择 <@ready> (3)",
            "\"扩展 名😀\".选择 <@ready> (3)",
            "ext::\"choose\"(<@ready>, (3))",
        ] {
            let source = format!(
                r#"shared = 1
sprite "玩家😀":
    init:
        pass
    local = 2
    logs = {{}}
    def 检查 @steps <@ready>:
        {header}:
            pass
        else:
            pass
        else:
            说 |
sprite "其他":
    init:
        pass
    hidden = 3
"#
            );
            let symbols = here(&source);
            assert_eq!(
                names(&symbols.variables),
                BTreeSet::from([("shared".into(), true), ("local".into(), false)]),
                "{header}"
            );
            assert_eq!(
                names(&symbols.lists),
                BTreeSet::from([("logs".into(), false)]),
                "{header}"
            );
            assert_eq!(
                symbols
                    .parameters
                    .iter()
                    .map(|slot| slot.name.as_str())
                    .collect::<Vec<_>>(),
                ["steps", "ready"],
                "{header}"
            );
        }
    }

    #[test]
    fn new_infix_operators_preserve_private_procedure_scope() {
        for expression in [
            "<local > @steps>",
            "<local / @steps > 0>",
            "<local % @steps = 0>",
            "<local + 1 < @steps - 1>",
            "<<@ready> > <false>>",
            "<ext.模板 <@ready> (1)>",
            "<ext.模板 <@ready>\n(1)>",
            "<ext.模板 <local > @steps> (1)>",
            "<ext.模板 <ext.模板 <@ready> (1)> (2)>",
        ] {
            let source = format!(
                r#"shared = 1
sprite "玩家😀" :
    init :
        pass
    local = 2
    bag = {{}}
    def 测试 @steps <@ready> :
        if {expression}:
            说 |
sprite "其他" :
    init :
        pass
    hidden = 3
after = 4
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
        let source = r##"sprite "玩家" :
    init :
        pass
    local = 1
    start :
        说 (local / 2)
        说 (<local / 2 > 1>)
        说 '*'
        说 '/# @var menu'
    after = 2
    start :
        说 |
outside = 3
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
            "'var fake",
            "'escaped\\' / # @var fake",
            "'escaped\\\\ / # @var fake",
            "'closed quote' # var fake",
            "'still contains / var fake",
        ] {
            let source = format!("before = 1\n说 {menu}\nafter = 2\n说 |");
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
        let source = r#"before = 0
[unclosed command
after = 0
multi = {
    1,
    "@var not_data",
    2
}
bad_value = "unterminated
文本 = 0
stage:
    staged = 0
|
"#;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("before".into(), true),
                ("after".into(), true),
                ("bad_value".into(), true),
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
    fn assignments_infer_data_kinds_in_scripts_and_preserve_known_private_targets() {
        let source = r#"shared = 0
var retired = 1
list retiredList = {}
sprite "玩家":
    init:
        pass
    local = 2
    records = {}
    def 更新 @amount:
        generated = 3
        generatedRecords = {"item"}
        local += @amount
        records = {}
        global:shared += 1
        说(|)
"#;
        let symbols = here(source);
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([
                ("shared".into(), true),
                ("generated".into(), true),
                ("local".into(), false),
            ])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("generatedRecords".into(), true), ("records".into(), false),])
        );
        assert_eq!(symbols.parameters[0].name, "amount");
    }

    #[test]
    fn invalid_kind_changes_preserve_first_assignment_and_scope_kinds() {
        for (first, second, list) in [("1", "{}", false), ("{}", "1", true)] {
            let source =
                format!("start:\n    value = {first}\n    value = {second}\n    等待(|)秒");
            let symbols = here(&source);
            assert_eq!(
                names(&symbols.variables).contains(&("value".into(), true)),
                !list
            );
            assert_eq!(
                names(&symbols.lists).contains(&("value".into(), true)),
                list
            );

            let source = format!(
                "sprite \"A\":\n    init:\n        pass\n    value = {first}\nsprite \"B\":\n    init:\n        pass\n    start:\n        value = {second}\n        等待(|)秒"
            );
            let symbols = here(&source);
            assert!(symbols.variables.is_empty(), "{source}");
            assert!(symbols.lists.is_empty(), "{source}");

            let source = format!(
                "value = {first}\nsprite \"A\":\n    init:\n        pass\n    value = {second}\n    start:\n        等待(|)秒"
            );
            let symbols = here(&source);
            assert_eq!(
                names(&symbols.variables),
                if list {
                    BTreeSet::new()
                } else {
                    BTreeSet::from([("value".into(), true)])
                }
            );
            assert_eq!(
                names(&symbols.lists),
                if list {
                    BTreeSet::from([("value".into(), true)])
                } else {
                    BTreeSet::new()
                }
            );
        }
    }

    #[test]
    fn current_document_scope_ends_at_dedent() {
        let source = "public = 0\nsprite \"cat\" :\n    init :\n        pass\n    private = 0\n|";
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
        let current =
            "import helper\nsprite \"cat\" add :\n    unsaved = 0\n    start :\n        [说 ";
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
                "fromEntry = 0\nsprite \"cat\" :\n    init :\n        pass\n    private = 0\nimport feature"
                    .into(),
            ),
            (module.clone(), "stale = 0".into()),
            (
                dependency,
                "import feature\nimported = {}\ndef 帮助 @n :\n    说 (@n)".into(),
            ),
            (dir.path().join("unrelated.sbdsl"), "unrelated = 0".into()),
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
        let source = "import helper\nimport extra\ndef 主积木 :\n    pass\n";
        let overlays = BTreeMap::from([
            (
                dir.path().join("sbdsl.buildsb"),
                r#"{"entry":"nested/main.sbdsl","srcDir":"src"}"#.into(),
            ),
            (
                dir.path().join("nested/helper.sbdsl"),
                "nearby = 0\ndef 附近积木 :\n    pass".into(),
            ),
            (
                dir.path().join("src/helper.sbdsl"),
                "fallbackMustNotAppear = 0".into(),
            ),
            (
                dir.path().join("src/extra.sbdsl"),
                "fallback = 0\ndef 目录积木 :\n    pass".into(),
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
        let source = "own = 0\n";
        let module = dir.path().join("unrelated.sbdsl");
        let config = dir.path().join("sbdsl.buildsb");
        let mut overlays = BTreeMap::from([
            (config.clone(), "{".into()),
            (dir.path().join("main.sbdsl"), "otherProject = 0".into()),
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
    fn quoted_data_declarations_do_not_create_completion_symbols() {
        let symbols =
            here("var \"bad name\" = 1\nlist \"bad list\" = {}\nvalid = 1\nstart:\n    说 |");
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([("valid".into(), true)])
        );
        assert!(symbols.lists.is_empty());
    }

    #[test]
    fn unicode_names_and_quoted_parameters_preserve_scope() {
        let source = "特殊分数 = 0\nsprite \"猫😀\" :\n    init :\n        pass\n    背包东西 = {}\n    def 告诉 @\"\" :\n        [说 ";
        let symbols = collect(None, source, &BTreeMap::new(), source.len());
        assert_eq!(
            names(&symbols.variables),
            BTreeSet::from([("特殊分数".into(), true)])
        );
        assert_eq!(
            names(&symbols.lists),
            BTreeSet::from([("背包东西".into(), false)])
        );
        assert_eq!(symbols.parameters[0].name, "");
    }
}
