//! Tolerant, UTF-16 completion for a document which is usually not parseable yet.
//!
//! This scanner only determines safe insertion contexts. The registry remains the
//! authority for block text, argument defaults, namespaces, and target support.
use super::{
    symbols::{VisibleSymbols, menu_end, quoted_end},
    text::{byte_offset, position as position_at},
};
use crate::model::{ArgType, Shape, Slot, Template, Token};
use crate::registry::Registry;
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, InsertTextFormat, Position, Range,
    TextEdit,
};
use std::collections::{BTreeMap, BTreeSet};

pub fn complete(source: &str, position: Position, registry: &Registry) -> Vec<CompletionItem> {
    let Some(cursor) = byte_offset(source, position) else {
        return Vec::new();
    };
    let symbols = super::symbols::collect(None, source, &BTreeMap::new(), cursor);
    complete_with_symbols(source, position, registry, &symbols)
}

pub fn complete_with_symbols(
    source: &str,
    position: Position,
    registry: &Registry,
    symbols: &VisibleSymbols,
) -> Vec<CompletionItem> {
    let Some(cursor) = byte_offset(source, position) else {
        return Vec::new();
    };
    match context(source, cursor, registry, symbols) {
        Context::Blocked => Vec::new(),
        Context::Init { start } => {
            data_completions(source, start + 1, cursor, DataSchema::Init, false)
        }
        Context::Reference(reference) => reference_completions(source, cursor, &reference, symbols),
        Context::Blocks {
            start,
            content,
            shape,
            namespace,
            target,
            script,
            expected,
            identity,
        } => {
            let prefix = normalize(&source[content..cursor]);
            // Once a value has been entered, replacing an entire invocation is
            // surprising. Only complete its initial fixed words in this version.
            let assignment_target = shape.is_none() && namespace.is_none();
            let mut result = if namespace.is_none()
                && (assignment_target || matches!(shape, Some(Shape::Reporter | Shape::Boolean)))
            {
                reference_at(source, content, cursor, None, &[])
                    .map(|mut reference| {
                        reference.assignment_target = assignment_target;
                        reference.expected = expected;
                        reference_completions(source, cursor, &reference, symbols)
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if assignment_target {
                result.extend(assignment_operator_completions(
                    source, content, cursor, symbols,
                ));
            }
            if identity || !at_block_boundary(source, cursor) {
                return result;
            }
            let existing_suite = source[cursor..].trim_start().starts_with(':');
            let mut seen = BTreeSet::new();
            for template in registry.templates.iter().chain(symbols.procedures.iter()) {
                if shape.is_some_and(|shape| template.shape != shape)
                    || (shape.is_none()
                        && !match template.shape {
                            Shape::Hat => script != Some(true),
                            Shape::Control => script != Some(false),
                            Shape::Command => script == Some(true) || target.is_none(),
                            _ => false,
                        })
                    || namespace
                        .as_ref()
                        .is_some_and(|ns| template.namespace.as_ref() != Some(ns))
                    || (target == Some(Target::Stage) && template.sprite_only)
                    || (target == Some(Target::Sprite) && template.stage_only)
                {
                    continue;
                }
                let initial = match template.tokens.first() {
                    Some(Token::Literal(text)) => normalize(text),
                    _ => String::new(),
                };
                if !initial.starts_with(&prefix) {
                    continue;
                }
                let Some((label, mut snippet)) =
                    completion_text_visible(template, registry, &symbols.procedures)
                else {
                    continue;
                };
                if existing_suite && !matches!(template.shape, Shape::Control | Shape::Hat) {
                    continue;
                }
                // Extension metadata is untrusted and may describe a huge
                // branch count. Avoid allocating an unbounded snippet.
                if extension_control(template) && template.branch_count > 256 && !existing_suite {
                    continue;
                }
                if !seen.insert(label.clone()) {
                    continue;
                }
                if shape == Some(Shape::Boolean) {
                    snippet = snippet
                        .strip_prefix('<')
                        .and_then(|text| text.strip_suffix('>'))
                        .unwrap_or(&snippet)
                        .to_owned();
                }
                if matches!(template.shape, Shape::Hat | Shape::Control) {
                    if !existing_suite {
                        let indent = indentation(source, start);
                        if extension_control(template) {
                            for branch in 0..template.branch_count.max(1) {
                                if branch > 0 {
                                    snippet.push_str(&format!("\n{indent}else"));
                                }
                                snippet.push_str(&format!(
                                    ":\n{indent}    ${{{}:pass}}",
                                    template.slots().count() + branch + 1
                                ));
                            }
                            snippet.push_str(&format!("\n{indent}end$0"));
                        } else {
                            snippet.push_str(&format!(":\n{indent}    $0\n{indent}end"));
                        }
                    }
                } else {
                    snippet.push_str("$0");
                }
                let mut candidate = item(
                    source,
                    start,
                    cursor,
                    label,
                    snippet,
                    CompletionItemKind::SNIPPET,
                    if template.procedure.is_some() {
                        format!("自制积木 · {}", procedure_label(template))
                    } else {
                        format!("{} · {:?}", template.opcode, template.shape)
                    },
                );
                // Keep matching the user's display text when insertion needs
                // an explicit opcode to disambiguate third-party templates.
                candidate.filter_text = Some(block_text(template).0);
                result.push(candidate);
            }
            if shape.is_none()
                && namespace.is_none()
                && script == Some(true)
                && "else".starts_with(&prefix)
            {
                let indent = indentation(source, start);
                let snippet = if source[cursor..].trim_start().starts_with(':') {
                    "else".into()
                } else {
                    format!("else:\n{indent}    $0")
                };
                result.push(item(
                    source,
                    start,
                    cursor,
                    "else".into(),
                    snippet,
                    CompletionItemKind::KEYWORD,
                    "条件分支".into(),
                ));
            }
            result
        }
    }
}

fn item(
    source: &str,
    start: usize,
    end: usize,
    label: String,
    snippet: String,
    kind: CompletionItemKind,
    detail: String,
) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail: Some(detail),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range: Range::new(position_at(source, start), position_at(source, end)),
            new_text: snippet,
        })),
        ..CompletionItem::default()
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn indentation(source: &str, at: usize) -> &str {
    let line = &source[..at][source[..at].rfind('\n').map_or(0, |p| p + 1)..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn quote(text: &str) -> String {
    let mut result = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            _ => result.push(c),
        }
    }
    result.push('"');
    result
}

fn escape_snippet(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('$', "\\$")
        .replace('}', "\\}")
}

fn namespace_text(namespace: &str) -> String {
    if crate::parser::is_bare_namespace(namespace) {
        namespace.to_owned()
    } else {
        quote(namespace)
    }
}

fn slot_default(slot: &Slot) -> String {
    if let Some(menu) = &slot.menu {
        let default = slot.default.as_str().map(str::to_owned).unwrap_or_else(|| {
            if slot.default.is_null() {
                String::new()
            } else {
                slot.default.to_string()
            }
        });
        let value = match &menu.items {
            Some(items) if !items.iter().any(|(_, value)| value == &default) => {
                items.first().map(|(_, value)| value.as_str()).unwrap_or("")
            }
            _ => &default,
        };
        return crate::parser::format_menu(value);
    }
    match slot.arg_type {
        ArgType::Boolean => match slot.default.as_bool() {
            Some(true) => "<true>".into(),
            Some(false) => "<false>".into(),
            None => "<>".into(),
        },
        ArgType::Variable | ArgType::List => slot
            .default
            .as_str()
            .filter(|name| crate::parser::is_data_name(name))
            .unwrap_or(if slot.arg_type == ArgType::List {
                "列表"
            } else {
                "变量"
            })
            .to_owned(),
        ArgType::Color => quote(slot.default.as_str().unwrap_or("#000000")),
        _ => match &slot.default {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => quote(s),
            serde_json::Value::Bool(b) => quote(if *b { "true" } else { "false" }),
            _ => quote(""),
        },
    }
}

fn assignment_template(template: &Template) -> bool {
    template.namespace.is_none()
        && template.procedure.is_none()
        && matches!(
            template.opcode.as_str(),
            "data_setvariableto" | "data_changevariableby"
        )
}

fn extension_control(template: &Template) -> bool {
    template.shape == Shape::Control && template.namespace.is_some()
}

fn block_text(template: &Template) -> (String, String) {
    let prefix = template
        .namespace
        .as_ref()
        .map(|ns| format!("{}.", namespace_text(ns)))
        .unwrap_or_default();
    let mut label = if template.shape == Shape::Boolean {
        format!("<{prefix}")
    } else {
        prefix
    };
    let mut snippet = escape_snippet(&label);
    let mut index = 1;
    for token in &template.tokens {
        match token {
            Token::Literal(text) => {
                let text = if sugar_template(template) {
                    text.as_str()
                } else {
                    text.trim()
                };
                label.push_str(text);
                snippet.push_str(&escape_snippet(text));
            }
            Token::Slot(slot) => {
                let default = template_slot_default(template, slot);
                label.push_str(&default);
                snippet.push_str(&format!("${{{index}:{}}}", escape_snippet(&default)));
                index += 1;
            }
        }
    }
    if template.shape == Shape::Boolean {
        label.push('>');
        snippet.push('>');
    }
    (label, snippet)
}

fn sugar_template(template: &Template) -> bool {
    assignment_template(template)
        || (template.namespace.is_none() && template.shape == Shape::Control)
}

fn template_slot_default(template: &Template, slot: &Slot) -> String {
    let value = slot_default(slot);
    if sugar_template(template) || !crate::registry::slot_requires_parentheses(slot) {
        value
    } else {
        format!("({value})")
    }
}

fn procedure_label(template: &Template) -> String {
    template
        .tokens
        .iter()
        .map(|token| match token {
            Token::Literal(text) => text.clone(),
            Token::Slot(slot) if slot.arg_type == ArgType::Boolean => {
                format!("<@{}>", name_text(&slot.name, false))
            }
            Token::Slot(slot) => format!("@{}", name_text(&slot.name, false)),
        })
        .collect()
}

fn name_text(name: &str, quoted: bool) -> String {
    if !quoted && crate::parser::parse_name(name).is_ok() && !name.starts_with('"') {
        name.to_owned()
    } else {
        quote(name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferenceKind {
    Data,
    Parameter,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DataKind {
    Scalar,
    List,
    Either,
}

struct Reference {
    start: usize,
    end: usize,
    name: usize,
    kind: ReferenceKind,
    global: bool,
    expected: DataKind,
    quoted: bool,
    boolean: bool,
    assignment_target: bool,
}

/// Quoted parameter names are distinct from string arguments; data names only
/// use ordinary identifiers, optionally qualified with `global:`.
fn reference_at(
    source: &str,
    start: usize,
    cursor: usize,
    boolean_open: Option<usize>,
    expressions: &[Expression],
) -> Option<Reference> {
    let kind = if source[start..].starts_with('@') {
        ReferenceKind::Parameter
    } else {
        ReferenceKind::Data
    };
    let mut name = start + usize::from(kind == ReferenceKind::Parameter);
    let mut global = false;
    if kind == ReferenceKind::Data && source[name..cursor].starts_with("global:") {
        global = true;
        name += "global:".len();
    }
    let quoted = source[name..].starts_with('"');
    if (quoted && source[name..cursor].contains(['\n', '\r']))
        || (kind == ReferenceKind::Data && quoted)
        || source[name..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '$')
    {
        return None;
    }
    let end = if quoted {
        let candidate = quoted_end(source, name);
        let close =
            (candidate > name + 1 && source[name..candidate].ends_with('"')).then_some(candidate);
        if let Some(end) = close.filter(|&end| {
            let next = source[end..].chars().next();
            let boundary = next.is_none_or(|c| c.is_whitespace() || "]>)},#<+-=*/%".contains(c));
            // Delimited values need no separating space. Ask the lexer about
            // the next atom, including quoted/qualified values and numbers.
            // A quote after trailing whitespace followed by bare text is more
            // likely the opening quote of the next argument in unfinished code.
            boundary
                || (next.is_none_or(|c| !(c.is_alphabetic() || c == '_'))
                    && crate::parser::scan_atom(source, end).is_ok())
                || !source[name + 1..end - 1].ends_with(char::is_whitespace)
        }) {
            end
        } else {
            // Without a closing quote, stop at a syntax delimiter. In
            // particular, don't eat the next argument or a closing bracket.
            cursor
                + source[cursor..]
                    .find(|c: char| c.is_whitespace() || "]>)},#\"".contains(c))
                    .unwrap_or(source.len() - cursor)
        }
    } else {
        name + source[name..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(source.len() - name)
    };
    let following = source[end..].trim_start_matches([' ', '\t', '\r', '\n']);
    let following_position = source.len() - following.len();
    let following_operator = boolean_open.is_some()
        && expression_operator(source, following_position, expressions).is_some();
    let boolean = boolean_open.is_some()
        && (following.is_empty()
            || (following.starts_with('>') && !following_operator)
            || following.starts_with([']', ')', '}']));
    (cursor >= name && cursor <= end).then_some(Reference {
        start,
        end,
        name,
        kind,
        global,
        expected: DataKind::Scalar,
        quoted,
        boolean,
        assignment_target: false,
    })
}

fn reference_completions(
    source: &str,
    cursor: usize,
    reference: &Reference,
    symbols: &VisibleSymbols,
) -> Vec<CompletionItem> {
    if reference.assignment_target && reference.kind == ReferenceKind::Parameter {
        return Vec::new();
    }
    let partial = &source[reference.name..cursor];
    let partial = if reference.quoted {
        let text = partial.strip_prefix('"').unwrap_or("");
        let decoded = crate::parser::parse_string(partial)
            .or_else(|_| crate::parser::parse_string(&format!("\"{text}\"")));
        let Ok(decoded) = decoded else {
            return Vec::new();
        };
        decoded
    } else {
        partial.to_owned()
    };
    let mut result = Vec::new();
    if reference.kind == ReferenceKind::Parameter {
        if reference.expected == DataKind::List {
            return result;
        }
        for slot in &symbols.parameters {
            let boolean = slot.arg_type == ArgType::Boolean;
            if !slot.name.starts_with(&partial) || (reference.boolean && !boolean) {
                continue;
            }
            let name = name_text(&slot.name, reference.quoted);
            let label = if boolean {
                format!("<@{name}>")
            } else {
                format!("@{name}")
            };
            let mut end = reference.end;
            let insertion = if boolean && reference.boolean {
                let rest = &source[end..];
                let trimmed = rest.trim_start_matches([' ', '\t', '\r', '\n']);
                if trimmed.starts_with('>') {
                    end += rest.len() - trimmed.len() + 1;
                }
                format!("@{name}>")
            } else {
                label.clone()
            };
            let mut candidate = item(
                source,
                reference.start,
                end,
                label,
                escape_snippet(&insertion),
                CompletionItemKind::VARIABLE,
                if boolean {
                    "布尔参数"
                } else {
                    "字符串 / 数值参数"
                }
                .into(),
            );
            candidate.filter_text = Some(format!("@{name}"));
            result.push(candidate);
        }
        return result;
    }

    let entered_prefix = &source[reference.start..reference.name];
    let mut visible = BTreeMap::new();
    for (is_list, values) in [(false, &symbols.variables), (true, &symbols.lists)] {
        for symbol in values {
            if (reference.global && !symbol.global) || !symbol.name.starts_with(&partial) {
                continue;
            }
            if let Some((_, previous)) = visible.get(&symbol.name) {
                let previous: &&super::symbols::DataSymbol = previous;
                if !previous.global || symbol.global {
                    continue;
                }
            }
            visible.insert(&symbol.name, (is_list, symbol));
        }
    }
    for (name, (is_list, symbol)) in visible {
        if (is_list && reference.expected == DataKind::Scalar)
            || (!is_list && reference.expected == DataKind::List)
        {
            continue;
        }
        let label = format!("{}{name}", if reference.global { "global:" } else { "" });
        let mut candidate = item(
            source,
            reference.start,
            reference.end,
            label.clone(),
            escape_snippet(&label),
            if is_list {
                CompletionItemKind::FIELD
            } else {
                CompletionItemKind::VARIABLE
            },
            format!(
                "{}{}",
                if symbol.global { "全局" } else { "角色" },
                if is_list { "列表" } else { "变量" }
            ),
        );
        candidate.filter_text = Some(format!("{entered_prefix}{name}"));
        result.push(candidate);
    }
    if !reference.global && "global:".starts_with(&partial) {
        let end = reference.end + usize::from(source[reference.end..].starts_with(':'));
        let mut candidate = item(
            source,
            reference.start,
            end,
            "global:".into(),
            "global:".into(),
            CompletionItemKind::KEYWORD,
            "全局数据引用".into(),
        );
        candidate.command = Some(lsp_types::Command {
            title: "补全数据名称".into(),
            command: "editor.action.triggerSuggest".into(),
            arguments: None,
        });
        result.push(candidate);
    }
    result
}

fn template_default_types(template: &Template) -> BTreeMap<(String, bool), bool> {
    template
        .slots()
        .filter_map(|slot| {
            matches!(slot.arg_type, ArgType::Variable | ArgType::List)
                .then(|| crate::parser::parse_reference(&slot_default(slot)).ok())
                .flatten()
                .map(|(name, global)| ((name, global), slot.arg_type == ArgType::List))
        })
        .collect()
}

fn symbol_data_types(symbols: &VisibleSymbols) -> BTreeMap<(String, bool), bool> {
    let mut types = BTreeMap::new();
    for global in [true, false] {
        for (list, values) in [(false, &symbols.variables), (true, &symbols.lists)] {
            for symbol in values.iter().filter(|symbol| symbol.global == global) {
                types.insert((symbol.name.clone(), false), list);
                if global {
                    types.insert((symbol.name.clone(), true), list);
                }
            }
        }
    }
    types
}

fn completion_text_visible(
    template: &Template,
    registry: &Registry,
    visible: &[Template],
) -> Option<(String, String)> {
    let data_types = template_default_types(template);
    let resolves_to_template = |text: &str| {
        registry
            .parse_typed(text, template.shape, visible, &data_types)
            .is_ok_and(|call| {
                call.template.opcode == template.opcode
                    && call.template.namespace == template.namespace
                    && call.template.procedure == template.procedure
            })
    };
    let ordinary = block_text(template);
    if resolves_to_template(&ordinary.0) {
        return Some(ordinary);
    }
    let namespace = template.namespace.as_ref()?;
    let opcode = template.local_opcode.as_ref()?;
    let mut label = format!(
        "{}{}::{}(",
        if template.shape == Shape::Boolean {
            "<"
        } else {
            ""
        },
        namespace_text(namespace),
        quote(opcode)
    );
    let mut snippet = escape_snippet(&label);
    for (index, slot) in template.slots().enumerate() {
        if index > 0 {
            label.push_str(", ");
            snippet.push_str(", ");
        }
        let default = template_slot_default(template, slot);
        label.push_str(&default);
        snippet.push_str(&format!("${{{}:{}}}", index + 1, escape_snippet(&default)));
    }
    label.push(')');
    snippet.push(')');
    if template.shape == Shape::Boolean {
        label.push('>');
        snippet.push('>');
    }
    resolves_to_template(&label).then_some((label, snippet))
}

fn at_block_boundary(source: &str, cursor: usize) -> bool {
    let rest = &source[cursor..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    trimmed.is_empty() || trimmed.starts_with([':', '\n', '\r', ')', '>'])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Stage,
    Sprite,
}

enum Context {
    Blocked,
    Reference(Reference),
    Init {
        start: usize,
    },
    Blocks {
        start: usize,
        content: usize,
        shape: Option<Shape>,
        namespace: Option<String>,
        target: Option<Target>,
        script: Option<bool>,
        expected: DataKind,
        identity: bool,
    },
}

struct Expression {
    open: char,
    content: usize,
    namespace: Option<String>,
    opcode: Option<String>,
    slot: Option<ArgType>,
}

fn comment_end(source: &str, start: usize) -> usize {
    crate::notation::comment_end(source, start).unwrap_or(source.len())
}

/// A quoted namespace is one atom, even when its filename contains spaces.
fn namespace_name_before(text: &str) -> Option<String> {
    if text.ends_with('"') {
        for (start, c) in text.char_indices().rev() {
            if c == '"'
                && !text[..start].ends_with(':')
                && let Ok(name) = crate::parser::parse_string(&text[start..])
            {
                return Some(name);
            }
        }
    } else {
        let start = text
            .char_indices()
            .rev()
            .find(|(_, c)| !crate::parser::namespace_continue(*c))
            .map_or(0, |(p, c)| p + c.len_utf8());
        // `global:score-ext::"read"()` has a reference before the minus.
        // Bare `score-ext::"read"()` instead uses a hyphenated namespace.
        if text[..start].ends_with(['@', ':'])
            && let Some(reference) = text[..start]
                .char_indices()
                .rev()
                .find(|(_, c)| !(c.is_alphanumeric() || "_:@".contains(*c)))
                .map(|(offset, c)| offset + c.len_utf8())
                .or(Some(0))
            && let Ok(end) = crate::parser::scan_atom(text, reference)
            && end >= start
            && let Some(namespace) = text[end..].strip_prefix('-')
            && crate::parser::is_bare_namespace(namespace)
        {
            return Some(namespace.into());
        }
        let name = &text[start..];
        if crate::parser::is_bare_namespace(name) {
            return Some(name.into());
        }
        // A numeric left operand can directly precede a minus, as in
        // `(1-ext::"read"())`. Preserve it when replacing the namespace call.
        for (offset, c) in name.char_indices() {
            if c == '-' && crate::parser::is_bare_namespace(&name[offset + 1..]) {
                return Some(name[offset + 1..].into());
            }
        }
    }
    None
}

fn explicit_namespace_before(source: &str, open: usize) -> Option<(String, String)> {
    let text = &source[..open];
    if !text.ends_with('"') {
        return None;
    }
    for (opcode_start, c) in text.char_indices().rev() {
        if c == '"'
            && let Ok(opcode) = crate::parser::parse_string(&text[opcode_start..])
            && let Some(namespace) = text[..opcode_start].strip_suffix("::")
        {
            return Some((namespace_name_before(namespace)?, opcode));
        }
    }
    None
}

fn declaration_header(header: &str) -> bool {
    crate::parser::declaration_header(header)
        || crate::parser::keyword_rest(header, "pass").is_some()
}

fn context(source: &str, cursor: usize, registry: &Registry, symbols: &VisibleSymbols) -> Context {
    let owned;
    let frames = match &symbols.scopes {
        Some(scopes) => scopes.as_slice(),
        None => {
            owned = super::symbols::layout_scopes(source, cursor);
            &owned
        }
    };
    if let Some(init) = frames.iter().find(|scope| scope.header == "init") {
        return Context::Init {
            start: init.start - 1,
        };
    }
    let target = frames.iter().rev().find_map(|scope| {
        let header = scope.header.as_str();
        if header.starts_with("sprite ") {
            Some(Target::Sprite)
        } else if header == "stage" {
            Some(Target::Stage)
        } else {
            None
        }
    });
    let script = frames.last().is_some_and(|scope| {
        let header = scope.header.as_str();
        header != "stage" && !header.starts_with("sprite ")
    });
    let start = frames.last().map_or(0, |scope| scope.start);
    context_from(source, cursor, start, target, script, registry, symbols)
}

fn context_from(
    source: &str,
    cursor: usize,
    start: usize,
    target: Option<Target>,
    is_script: bool,
    registry: &Registry,
    symbols: &VisibleSymbols,
) -> Context {
    let mut pos = start;
    let mut statement = start;
    let mut expressions: Vec<Expression> = Vec::new();
    while pos < cursor {
        let c = source[pos..].chars().next().unwrap();
        if c == '#' {
            let end = comment_end(source, pos);
            if cursor < end || (cursor == end && !(end > pos + 1 && source[..end].ends_with('#'))) {
                return Context::Blocked;
            }
            if expressions.is_empty()
                && (source[pos..end].contains('\n') || source[statement..pos].trim().is_empty())
            {
                statement = end;
            }
            pos = end;
            continue;
        }
        let boundary = source[..pos]
            .chars()
            .next_back()
            .is_none_or(|previous| !(previous.is_alphanumeric() || "_:@$".contains(previous)));
        if c == '@' || (boundary && (c.is_alphabetic() || c == '_')) {
            let header = source[statement..pos].trim_start();
            let in_value = value_position(
                source,
                statement,
                pos,
                &expressions,
                registry,
                symbols,
                is_script,
            ) || (!expressions.is_empty()
                && (c == '@'
                    || (source[pos..].starts_with("global:")
                        && !fixed_reference_prefix(
                            source,
                            cursor,
                            &expressions,
                            registry,
                            symbols,
                        ))));
            let expression_start = expressions.last().is_some_and(|expression| {
                expression.namespace.is_none() && source[expression.content..pos].trim().is_empty()
            });
            if !(is_script && expressions.is_empty() && header.is_empty())
                && !declaration_header(header)
                && in_value
                && (!expression_start || c == '@' || source[pos..].starts_with("global:"))
            {
                let boolean_open = expressions
                    .last()
                    .filter(|expression| {
                        expression.open == '<'
                            && expression.namespace.is_none()
                            && source[expression.content..pos].trim().is_empty()
                    })
                    .map(|expression| expression.content - 1);
                if let Some(mut reference) =
                    reference_at(source, pos, cursor, boolean_open, &expressions)
                {
                    if reference.kind == ReferenceKind::Parameter
                        && expressions.last().is_some_and(|expression| {
                            matches!(expression.slot, Some(ArgType::Variable | ArgType::List))
                        })
                    {
                        return Context::Blocked;
                    }
                    reference.expected = expected_data_kind(&expressions);
                    if !reference_completions(source, cursor, &reference, symbols).is_empty() {
                        return Context::Reference(reference);
                    }
                }
            }
        }
        if c == '"' {
            let end = quoted_end(source, pos);
            if cursor < end || (cursor == end && !source[pos..end].ends_with('"')) {
                return Context::Blocked;
            }
            pos = end;
            continue;
        }
        if c == '\'' {
            let (end, closed) = menu_end(source, pos);
            if cursor < end || (cursor == end && !closed) {
                return Context::Blocked;
            }
            pos = end;
            continue;
        }
        if matches!(c, '<' | '>' | '/')
            && let Some(end) = expression_operator(source, pos, &expressions)
        {
            pos = end;
            continue;
        }
        match c {
            '[' => {
                let end = literal_region_end(source, pos);
                if end <= cursor && source[..end].ends_with(']') {
                    pos = end;
                    continue;
                }
                return Context::Blocked;
            }
            ']' => return Context::Blocked,
            '(' | '<' => {
                let qualified = explicit_namespace_before(source, pos);
                let slot = slot_before(
                    source,
                    statement,
                    pos,
                    &expressions,
                    registry,
                    symbols,
                    is_script,
                );
                if source[..pos].ends_with('.') && slot.is_none() {
                    return Context::Blocked;
                }
                expressions.push(Expression {
                    open: c,
                    content: pos + 1,
                    namespace: qualified.as_ref().map(|(ns, _)| ns.clone()),
                    opcode: qualified.map(|(_, opcode)| opcode),
                    slot,
                });
            }
            ')' | '>' => {
                let expected = match c {
                    ')' => '(',
                    _ => '<',
                };
                if expressions.last().is_some_and(|e| e.open == expected) {
                    expressions.pop();
                } else {
                    return Context::Blocked;
                }
            }
            '{' if expressions.is_empty() => {
                let end = literal_region_end(source, pos);
                if cursor <= end {
                    return Context::Blocked;
                }
                pos = end;
                continue;
            }
            '}' if expressions.is_empty() => return Context::Blocked,
            '\n' if expressions.is_empty() => {
                statement = pos + 1;
            }
            _ => {}
        }
        pos += c.len_utf8();
    }
    let header = source[statement..cursor].trim_start();
    if declaration_header(header) && !(is_script && crate::parser::parse_reference(header).is_ok())
    {
        return Context::Blocked;
    }
    if value_position(
        source,
        statement,
        cursor,
        &expressions,
        registry,
        symbols,
        is_script,
    ) && expressions.last().is_none_or(|expression| {
        expression.namespace.is_some() || !source[expression.content..cursor].trim().is_empty()
    }) && source[..cursor]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace() || "([<,+-*/%=<>".contains(c))
        && let Some(mut reference) = reference_at(source, cursor, cursor, None, &expressions)
    {
        reference.expected = expected_data_kind(&expressions);
        return Context::Reference(reference);
    }
    if let Some(e) = expressions.last() {
        if e.namespace.is_some() {
            return Context::Blocked;
        }
        let (content, namespace) = call_prefix(source, e.content, cursor, registry);
        return Context::Blocks {
            start: e.content,
            content,
            shape: Some(match e.open {
                '(' => Shape::Reporter,
                _ => Shape::Boolean,
            }),
            namespace,
            target,
            script: None,
            expected: expected_data_kind(&expressions),
            identity: matches!(e.slot, Some(ArgType::Variable | ArgType::List)),
        };
    }
    let start =
        statement + source[statement..cursor].len() - source[statement..cursor].trim_start().len();
    let (content, namespace) = call_prefix(source, start, cursor, registry);
    Context::Blocks {
        start,
        content,
        shape: None,
        namespace,
        target,
        script: Some(is_script),
        expected: crate::parser::assignment_parts(source[start..].split('\n').next().unwrap_or(""))
            .map_or(DataKind::Either, |assignment| {
                if assignment.value.starts_with('{') {
                    DataKind::List
                } else {
                    DataKind::Scalar
                }
            }),
        identity: false,
    }
}

fn scalar_target(text: &str, symbols: &VisibleSymbols) -> bool {
    crate::parser::parse_reference(text).is_ok_and(|(name, global)| {
        !symbols
            .lists
            .iter()
            .any(|symbol| symbol.name == name && (!global || symbol.global))
    })
}

fn expected_data_kind(expressions: &[Expression]) -> DataKind {
    if expressions
        .last()
        .is_some_and(|expression| expression.slot == Some(ArgType::List))
    {
        DataKind::List
    } else {
        DataKind::Scalar
    }
}

/// Parentheses delimit inputs, so their data kind comes from the enclosing
/// template rather than the shape of a block inside those parentheses.
fn slot_before(
    source: &str,
    statement: usize,
    position: usize,
    expressions: &[Expression],
    registry: &Registry,
    symbols: &VisibleSymbols,
    is_script: bool,
) -> Option<ArgType> {
    let parent = expressions.last();
    if let Some(parent) = parent.filter(|parent| parent.namespace.is_some()) {
        let mut index = 0;
        let mut at = parent.content;
        while at < position {
            at += source[at..position].len() - source[at..position].trim_start().len();
            if at == position {
                break;
            }
            let Ok(end) = crate::parser::scan_atom(&source[..position], at) else {
                break;
            };
            at = end;
            at += source[at..position].len() - source[at..position].trim_start().len();
            if source[at..position].starts_with(',') {
                index += 1;
                at += 1;
            } else {
                break;
            }
        }
        return registry
            .templates
            .iter()
            .chain(symbols.procedures.iter())
            .find(|template| {
                template.namespace == parent.namespace && template.local_opcode == parent.opcode
            })
            .and_then(|template| template.slots().nth(index))
            .map(|slot| slot.arg_type);
    }
    let start = parent.map_or(statement, |parent| parent.content);
    let start = start + source[start..position].len() - source[start..position].trim_start().len();
    let (content, namespace) = call_prefix(source, start, position, registry);
    let shape = parent.map(|parent| {
        if parent.open == '<' {
            Shape::Boolean
        } else {
            Shape::Reporter
        }
    });
    let suite = parent.is_none()
        && source[start..]
            .split('\n')
            .next()
            .is_some_and(|line| line.trim_end().ends_with(':'));
    let assignment = crate::parser::assignment_parts(&source[statement..position]).is_some();
    let allowed = |template: &&Template| {
        template.namespace == namespace
            && (!assignment_template(template) || assignment)
            && (!suite
                || template.shape
                    == if is_script {
                        Shape::Control
                    } else {
                        Shape::Hat
                    })
            && shape.map_or(
                matches!(template.shape, Shape::Command | Shape::Control | Shape::Hat),
                |shape| template.shape == shape,
            )
    };
    let prefix = &source[content..position];
    let mut types = symbol_data_types(symbols);
    let current = crate::parser::scan_atom(source, position)
        .ok()
        .and_then(|end| {
            source[position..end]
                .strip_prefix('(')
                .and_then(|value| value.strip_suffix(')'))
        })
        .and_then(|value| crate::parser::parse_reference(value.trim()).ok());
    let end = parent
        .and_then(|parent| crate::parser::scan_atom(source, parent.content - 1).ok())
        .map(|end| end - 1)
        .unwrap_or_else(|| {
            source[start..]
                .find(['\n', '\r'])
                .map_or(source.len(), |at| start + at)
        });
    if end >= position {
        let raw = source[start..end].trim().trim_end_matches(':').trim_end();
        for inferred in [None, Some(false), Some(true)] {
            if let Some(list) = inferred {
                let Some((name, global)) = &current else {
                    continue;
                };
                types.insert((name.clone(), *global), list);
            }
            for candidate_shape in [
                Shape::Command,
                Shape::Control,
                Shape::Hat,
                Shape::Reporter,
                Shape::Boolean,
            ] {
                if shape.is_some_and(|shape| shape != candidate_shape) {
                    continue;
                }
                let header = if candidate_shape == Shape::Boolean {
                    format!("<{raw}>")
                } else {
                    raw.to_owned()
                };
                if let Ok(call) =
                    registry.parse_typed(&header, candidate_shape, &symbols.procedures, &types)
                    && allowed(&&call.template)
                    && let Some(kind) = template_prefix_type(prefix, &call.template.tokens, false)
                {
                    return Some(kind);
                }
            }
        }
    }
    let mut kinds = Vec::new();
    for template in registry
        .templates
        .iter()
        .chain(symbols.procedures.iter())
        .filter(allowed)
    {
        if let Some(kind) = template_prefix_type(prefix, &template.tokens, false) {
            kinds.push(kind);
        }
    }
    let first = *kinds.first()?;
    Some(if kinds.iter().all(|kind| *kind == first) {
        first
    } else {
        ArgType::Any
    })
}

fn call_prefix(
    source: &str,
    start: usize,
    end: usize,
    registry: &Registry,
) -> (usize, Option<String>) {
    let text = &source[start..end];
    registry
        .templates
        .iter()
        .filter_map(|template| template.namespace.as_ref())
        .flat_map(|namespace| {
            [namespace.clone(), quote(namespace)]
                .into_iter()
                .map(move |name| (namespace, format!("{name}.")))
        })
        .filter(|(_, prefix)| text.starts_with(prefix))
        .max_by_key(|(_, prefix)| prefix.len())
        .map_or((start, None), |(namespace, prefix)| {
            (start + prefix.len(), Some(namespace.clone()))
        })
}

fn assignment_operator_completions(
    source: &str,
    start: usize,
    cursor: usize,
    symbols: &VisibleSymbols,
) -> Vec<CompletionItem> {
    let prefix = &source[start..cursor];
    let Ok(target_end) = crate::parser::scan_atom(prefix, 0) else {
        return Vec::new();
    };
    if target_end == prefix.len() || crate::parser::parse_reference(&prefix[..target_end]).is_err()
    {
        return Vec::new();
    }
    let list = !scalar_target(&prefix[..target_end], symbols);
    let entered = prefix[target_end..].trim_start();
    let operator_start = cursor - entered.len();
    let end = operator_start
        + source[operator_start..]
            .chars()
            .take_while(|c| "=+-*/%".contains(*c))
            .map(char::len_utf8)
            .sum::<usize>();
    ["=", "+=", "-=", "*=", "/=", "%="]
        .into_iter()
        .filter(|operator| operator.starts_with(entered) && (!list || *operator == "="))
        .map(|operator| {
            let following = source[end..].split('\n').next().unwrap_or("").trim();
            let insertion = if following.is_empty() {
                format!(
                    "{operator} ${{1:{}}}",
                    if list {
                        "{\\}"
                    } else if operator == "=" {
                        "0"
                    } else {
                        "1"
                    }
                )
            } else {
                operator.to_owned()
            };
            item(
                source,
                operator_start,
                end,
                operator.into(),
                insertion,
                CompletionItemKind::OPERATOR,
                "变量赋值".into(),
            )
        })
        .collect()
}

/// Check the fixed words before a candidate, so a variable completion only
/// replaces the current value and never consumes a block's command text.
fn value_position(
    source: &str,
    statement: usize,
    position: usize,
    expressions: &[Expression],
    registry: &Registry,
    symbols: &VisibleSymbols,
    is_script: bool,
) -> bool {
    let expression = expressions.last();
    if is_script
        && expression.is_none()
        && let Some(assignment) = crate::parser::assignment_parts(&source[statement..position])
        && scalar_target(assignment.target, symbols)
    {
        let value = assignment.value.trim_end();
        return value.is_empty() || value.ends_with(['+', '-', '*', '/', '%']);
    }
    let (content, shape) = if let Some(expression) = expression {
        if expression.namespace.is_some() {
            return false;
        }
        (
            expression.content,
            Some(match expression.open {
                '(' => Shape::Reporter,
                _ => Shape::Boolean,
            }),
        )
    } else {
        (
            statement + source[statement..position].len()
                - source[statement..position].trim_start().len(),
            None,
        )
    };
    let prefix = &source[content..position];
    if expression.is_some()
        && prefix
            .trim_end()
            .ends_with(['+', '-', '*', '/', '%', '=', '<', '>'])
    {
        return true;
    }
    registry
        .templates
        .iter()
        .chain(symbols.procedures.iter())
        .any(|template| {
            shape.map_or(template.shape == Shape::Control, |shape| {
                template.shape == shape
            }) && template.namespace.is_none()
                && template_prefix_type(prefix, &template.tokens, false).is_some()
        })
}

/// An explicit-looking prefix can also be a procedure's fixed label, such as
/// `检查 list: @n`. Only offer reference edits when that text is an input.
fn fixed_reference_prefix(
    source: &str,
    cursor: usize,
    expressions: &[Expression],
    registry: &Registry,
    symbols: &VisibleSymbols,
) -> bool {
    let Some(expression) = expressions
        .last()
        .filter(|expression| expression.namespace.is_none())
    else {
        return false;
    };
    let shape = match expression.open {
        '(' => Shape::Reporter,
        _ => Shape::Boolean,
    };
    let source = &source[expression.content..cursor];
    registry
        .templates
        .iter()
        .chain(symbols.procedures.iter())
        .any(|template| {
            template.shape == shape
                && template.namespace == expression.namespace
                && template_prefix_type(source, &template.tokens, true).is_some()
        })
}

fn template_prefix_type(source: &str, tokens: &[Token], literal_prefix: bool) -> Option<ArgType> {
    let mut positions = vec![0];
    for token in tokens {
        let mut next = Vec::new();
        for mut position in positions {
            match token {
                Token::Literal(literal) => {
                    let mut matched = true;
                    for expected in literal.chars() {
                        if expected == ' ' {
                            let end = position + source[position..].len()
                                - source[position..].trim_start().len();
                            if end == position
                                && position < source.len()
                                && !source[position..].starts_with(['(', '<', '\''])
                                && !source[..position].ends_with([')', '>', '\''])
                            {
                                matched = false;
                                break;
                            }
                            position = end;
                        } else if position == source.len() {
                            if literal_prefix {
                                return Some(ArgType::Any);
                            }
                            matched = false;
                            break;
                        } else if source[position..].starts_with(expected) {
                            position += expected.len_utf8();
                        } else {
                            matched = false;
                            break;
                        }
                    }
                    if matched {
                        if literal_prefix && position == source.len() {
                            return Some(ArgType::Any);
                        }
                        next.push(position);
                    }
                }
                Token::Slot(slot) => {
                    position += source[position..].len() - source[position..].trim_start().len();
                    if !literal_prefix && position == source.len() {
                        return Some(slot.arg_type);
                    }
                    if let Ok(ends) = crate::parser::scan_atom_candidates(source, position) {
                        next.extend(ends);
                    }
                }
            }
        }
        next.sort_unstable();
        next.dedup();
        // Incomplete source must not create unbounded template alternatives.
        next.truncate(256);
        positions = next;
        if positions.is_empty() {
            return None;
        }
    }
    None
}

fn expression_operator(source: &str, position: usize, expressions: &[Expression]) -> Option<usize> {
    let expression = expressions.last()?;
    if expression.namespace.is_some() {
        return None;
    }
    let groups: Vec<_> = expressions
        .iter()
        .map(|expression| {
            (
                match expression.open {
                    '(' => ')',
                    _ => '>',
                },
                expression.content - 1,
            )
        })
        .collect();
    super::symbols::operator_end(source, position, &groups).or_else(|| {
        // At an empty comparison input, the next > is the group's closer.
        // Keep this > as an operator while the user fills that input.
        (expression.open == '<'
            && source[position..].starts_with('>')
            && source[position + 1..]
                .trim_start()
                .strip_prefix('>')
                .is_some_and(|rest| {
                    let rest = rest.trim_start();
                    rest.is_empty() || rest.starts_with([']', ')', '>', ':', ','])
                })
            && crate::parser::scan_atom(&source[..position], expression.content)
                .ok()
                .is_some_and(|end| source[end..position].trim().is_empty()))
        .then_some(position + 1)
    })
}

/// Skip a preceding list initializer or embedded data object without treating
/// its braces as script scope. Completion inside data uses its own schema.
fn literal_region_end(source: &str, start: usize) -> usize {
    let mut groups = Vec::new();
    let mut position = start;
    while position < source.len() {
        let c = source[position..].chars().next().unwrap();
        match c {
            '"' => {
                position = quoted_end(source, position);
                continue;
            }
            '\'' => {
                position = menu_end(source, position).0;
                continue;
            }
            '#' => {
                position = comment_end(source, position);
                continue;
            }
            '{' => groups.push('}'),
            '[' => groups.push(']'),
            '}' | ']' if groups.last() == Some(&c) => {
                groups.pop();
                if groups.is_empty() {
                    return position + 1;
                }
            }
            _ => {}
        }
        position += c.len_utf8();
    }
    source.len()
}

#[derive(Clone, Copy)]
enum DataSchema {
    Build,
    Runtime,
    Stage,
    Init,
    Costume,
    Sound,
    Unknown,
}
#[derive(Clone, Copy, PartialEq)]
enum DataPhase {
    Key,
    Colon,
    Value,
    After,
}
struct DataFrame {
    schema: DataSchema,
    object: bool,
    phase: DataPhase,
    key: String,
    seen: BTreeSet<String>,
}
impl DataFrame {
    fn new(schema: DataSchema, object: bool) -> Self {
        Self {
            schema,
            object,
            phase: if object {
                DataPhase::Key
            } else {
                DataPhase::Value
            },
            key: String::new(),
            seen: BTreeSet::new(),
        }
    }
}

fn data_fields(schema: DataSchema) -> &'static [(&'static str, &'static str)] {
    match schema {
        DataSchema::Build => &[
            ("entry", "\"./src/main.sbdsl\""),
            ("output", "\"./dist/project.sb3\""),
            ("name", "\"项目\""),
            ("srcDir", "\"./src\""),
            ("extensions", "[]"),
            ("runtime", "{}"),
            ("stage", "{}"),
        ],
        DataSchema::Runtime => &[
            ("fps", "60"),
            ("stageWidth", "480"),
            ("stageHeight", "360"),
            ("maxClones", "300"),
            ("fencing", "true"),
            ("miscLimits", "true"),
            ("hqPen", "false"),
        ],
        DataSchema::Stage => &[("backdrops", "[]"), ("sounds", "[]")],
        DataSchema::Init => &[
            ("x", "0"),
            ("y", "0"),
            ("size", "100"),
            ("direction", "90"),
            ("visible", "true"),
            ("rotationStyle", "\"all around\""),
            ("costumes", "[]"),
            ("sounds", "[]"),
        ],
        DataSchema::Costume => &[
            ("name", "\"造型\""),
            ("path", "\"assets/costume.svg\""),
            ("rotationCenterX", "0"),
            ("rotationCenterY", "0"),
            ("bitmapResolution", "1"),
        ],
        DataSchema::Sound => &[("name", "\"声音\""), ("path", "\"assets/sound.wav\"")],
        DataSchema::Unknown => &[],
    }
}

/// Complete the same data notation used by build files and sprite init.
pub fn complete_config(source: &str, position: Position) -> Vec<CompletionItem> {
    let Some(cursor) = byte_offset(source, position) else {
        return Vec::new();
    };
    data_completions(source, 0, cursor, DataSchema::Build, true)
}

fn data_completions(
    source: &str,
    mut pos: usize,
    cursor: usize,
    schema: DataSchema,
    mut optional_root: bool,
) -> Vec<CompletionItem> {
    let mut stack = vec![DataFrame::new(schema, true)];
    while pos < cursor {
        let c = source[pos..].chars().next().unwrap();
        if c.is_whitespace() {
            if c == '\n' {
                separate_data_field(stack.last_mut());
            }
            pos += c.len_utf8();
            continue;
        }
        if c == '#' {
            let end = comment_end(source, pos);
            if cursor < end || (cursor == end && !(end > pos + 1 && source[..end].ends_with('#'))) {
                return Vec::new();
            }
            if source[pos..end].contains('\n') {
                separate_data_field(stack.last_mut());
            }
            pos = end;
            continue;
        }
        if optional_root {
            optional_root = false;
            if c == '{' {
                pos += 1;
                continue;
            }
        }
        let Some(frame) = stack.last_mut() else {
            return Vec::new();
        };
        if c == '"' {
            // Data strings cannot cross physical lines. A later field's quote
            // must never become this edit's closing quote.
            let end = quoted_end(source, pos);
            if cursor < end || (cursor == end && !source[pos + 1..end].ends_with('"')) {
                return data_candidates(source, cursor, pos, end, true, frame);
            }
            let Ok(value) = serde_json::from_str::<String>(&source[pos..end]) else {
                return Vec::new();
            };
            match frame.phase {
                DataPhase::Key if frame.object => {
                    frame.seen.insert(value.clone());
                    frame.key = value;
                    frame.phase = DataPhase::Colon;
                }
                DataPhase::Value => frame.phase = DataPhase::After,
                _ => return Vec::new(),
            }
            pos = end;
            continue;
        }
        if frame.object && frame.phase == DataPhase::Key && (c.is_ascii_alphabetic() || c == '_') {
            let end = source[pos..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .map_or(source.len(), |p| pos + p);
            if cursor <= end {
                return data_candidates(source, cursor, pos, end, false, frame);
            }
            frame.key = source[pos..end].into();
            frame.seen.insert(frame.key.clone());
            frame.phase = DataPhase::Colon;
            pos = end;
            continue;
        }
        match c {
            ':' if frame.phase == DataPhase::Colon => frame.phase = DataPhase::Value,
            ',' if matches!(frame.phase, DataPhase::After | DataPhase::Key) => {
                frame.phase = if frame.object {
                    DataPhase::Key
                } else {
                    DataPhase::Value
                };
            }
            '{' | '[' if frame.phase == DataPhase::Value => {
                let schema = if frame.object {
                    match frame.key.as_str() {
                        "runtime" if matches!(frame.schema, DataSchema::Build) => {
                            DataSchema::Runtime
                        }
                        "stage" if matches!(frame.schema, DataSchema::Build) => DataSchema::Stage,
                        "costumes" | "backdrops" => DataSchema::Costume,
                        "sounds" => DataSchema::Sound,
                        _ => DataSchema::Unknown,
                    }
                } else {
                    frame.schema
                };
                frame.phase = DataPhase::After;
                stack.push(DataFrame::new(schema, c == '{'));
            }
            '}' | ']' => {
                stack.pop();
            }
            _ if frame.phase == DataPhase::Value => {
                let end = source[pos..]
                    .find(|c: char| c.is_whitespace() || ",]}#".contains(c))
                    .map_or(source.len(), |p| pos + p);
                if cursor <= end {
                    return data_candidates(source, cursor, pos, end, false, frame);
                }
                frame.phase = DataPhase::After;
                pos = end;
                continue;
            }
            _ => return Vec::new(),
        }
        pos += c.len_utf8();
    }
    stack
        .last()
        .map(|f| data_candidates(source, cursor, cursor, cursor, false, f))
        .unwrap_or_default()
}

fn separate_data_field(frame: Option<&mut DataFrame>) {
    if let Some(frame) = frame
        && frame.phase == DataPhase::After
    {
        frame.phase = if frame.object {
            DataPhase::Key
        } else {
            DataPhase::Value
        };
    }
}

fn data_candidates(
    source: &str,
    cursor: usize,
    start: usize,
    end: usize,
    quoted: bool,
    frame: &DataFrame,
) -> Vec<CompletionItem> {
    let partial = &source[start + usize::from(quoted)..cursor];
    // Do not guess inside partially typed JSON escapes.
    if partial.contains('\\') {
        return Vec::new();
    }
    let end = if quoted {
        // An unfinished quote must not consume the rest of the document.
        if end > start + 1 && source[start + 1..end].ends_with('"') {
            end
        } else {
            cursor
        }
    } else {
        end
    };
    let after_trivia = data_trivia_end(source, end);
    let following = &source[after_trivia..];
    // A newline already separates a new field from the following field. On
    // the same line, replacing only the cursor would join two keys or values.
    let separated_key = frame.object
        && frame.phase == DataPhase::Key
        && source[end..after_trivia].contains(['\r', '\n']);
    if start == cursor
        && end == cursor
        && !following.is_empty()
        && !following.starts_with([',', '}', ']'])
        && !separated_key
    {
        return Vec::new();
    }
    if frame.object && frame.phase == DataPhase::Key {
        return data_fields(frame.schema)
            .iter()
            .filter(|(name, _)| name.starts_with(partial) && !frame.seen.contains(*name))
            .map(|(name, default)| {
                let key = if quoted {
                    quote(name)
                } else {
                    name.to_string()
                };
                let snippet = if following.starts_with(':') {
                    key.clone()
                } else {
                    format!("{key}: ${{1:{}}}$0", escape_snippet(default))
                };
                item(
                    source,
                    start,
                    end,
                    name.to_string(),
                    snippet,
                    CompletionItemKind::FIELD,
                    "配置属性".into(),
                )
            })
            .collect();
    }
    if frame.phase != DataPhase::Value {
        return Vec::new();
    }
    let choices: &[&str] = match frame.key.as_str() {
        "rotationStyle" if matches!(frame.schema, DataSchema::Init) => {
            &["all around", "left-right", "don't rotate"]
        }
        "visible" if !quoted && matches!(frame.schema, DataSchema::Init) => &["true", "false"],
        "fencing" | "miscLimits" | "hqPen"
            if !quoted && matches!(frame.schema, DataSchema::Runtime) =>
        {
            &["true", "false"]
        }
        _ => return Vec::new(),
    };
    choices
        .iter()
        .filter(|v| v.starts_with(partial))
        .map(|value| {
            let text = if frame.key == "rotationStyle" {
                quote(value)
            } else {
                value.to_string()
            };
            item(
                source,
                start,
                end,
                value.to_string(),
                escape_snippet(&text),
                CompletionItemKind::ENUM_MEMBER,
                format!("配置 {}", frame.key),
            )
        })
        .collect()
}

fn data_trivia_end(source: &str, mut position: usize) -> usize {
    while position < source.len() {
        let c = source[position..].chars().next().unwrap();
        if c.is_whitespace() {
            position += c.len_utf8();
        } else if c == '#' {
            position = comment_end(source, position);
        } else {
            break;
        }
    }
    position
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(marked: &str) -> (String, Position) {
        let byte = marked.find('|').expect("one cursor marker");
        let source = format!("{}{}", &marked[..byte], &marked[byte + 1..]);
        let position = position_at(&source, byte);
        (source, position)
    }

    /// Apply the default values of our snippets just as accepting completion
    /// without changing any tab stops would. Escaped snippet metacharacters
    /// must be decoded before the resulting DSL is validated.
    fn expand(snippet: &str) -> String {
        let mut out = String::new();
        let mut chars = snippet.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => out.push(chars.next().expect("escaped character")),
                '$' => {
                    if chars.peek() == Some(&'{') {
                        chars.next();
                        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                            chars.next();
                        }
                        assert_eq!(chars.next(), Some(':'));
                        while let Some(c) = chars.next() {
                            if c == '}' {
                                break;
                            }
                            if c == '\\' {
                                out.push(chars.next().unwrap());
                            } else {
                                out.push(c);
                            }
                        }
                    } else {
                        assert!(chars.peek().is_some_and(|c| c.is_ascii_digit()));
                        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                            chars.next();
                        }
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn apply(source: &str, completion: &CompletionItem) -> String {
        assert_eq!(
            completion.insert_text_format,
            Some(InsertTextFormat::SNIPPET)
        );
        let Some(CompletionTextEdit::Edit(edit)) = &completion.text_edit else {
            panic!("text edit required");
        };
        let start = byte_offset(source, edit.range.start).unwrap();
        let end = byte_offset(source, edit.range.end).unwrap();
        format!(
            "{}{}{}",
            &source[..start],
            expand(&edit.new_text),
            &source[end..]
        )
    }

    fn choose(mark: &str, opcode: &str) -> String {
        let (source, cursor) = marked(mark);
        let result = complete(&source, cursor, &Registry::new());
        let selected = result.iter().find(|c| {
            c.detail
                .as_ref()
                .is_some_and(|s| s.starts_with(&format!("{opcode} ·")))
        });
        apply(
            &source,
            selected.unwrap_or_else(|| {
                panic!(
                    "missing {opcode}: {:?}",
                    result.iter().map(|c| &c.label).collect::<Vec<_>>()
                )
            }),
        )
    }

    fn compile_source(source: &str) {
        let temp = tempfile::tempdir().unwrap();
        let entry = temp.path().join("main.sbdsl");
        std::fs::write(&entry, source).unwrap();
        let config = crate::config::discover(Some(&entry), None, temp.path()).unwrap();
        crate::build::compile(&config).unwrap_or_else(|error| panic!("{error}\n{source}"));
    }

    #[test]
    fn python_style_bodies_complete_with_colons_and_indentation() {
        assert_eq!(choose("sta|", "event_whenflagclicked"), "start:\n    \nend");
        assert_eq!(
            choose("start:\n    i|", "control_if"),
            "start:\n    if <>:\n        \n    end"
        );
        assert_eq!(
            choose("start:\n    loo|:\n        pass", "control_repeat"),
            "start:\n    loop 0:\n        pass"
        );
        assert_eq!(
            choose("start:\n    wh|", "control_while"),
            "start:\n    while <>:\n        \n    end"
        );
        assert_eq!(
            choose("start:\n    while t|", "control_forever"),
            "start:\n    while true:\n        \n    end"
        );
        assert_eq!(
            choose("\"自定义拓展\".当完成:\n    loo|", "control_repeat"),
            "\"自定义拓展\".当完成:\n    loop 0:\n        \n    end"
        );
        let (source, cursor) = marked("start:\n    if <true>:\n        pass\n    el|");
        let items = complete(&source, cursor, &Registry::new());
        assert!(items.iter().any(|item| item.label.starts_with("elif ")));
        let otherwise = items.iter().find(|item| item.label == "else").unwrap();
        assert_eq!(
            apply(&source, otherwise),
            "start:\n    if <true>:\n        pass\n    else:\n        "
        );
        let (source, cursor) = marked("|");
        let items = complete(&source, cursor, &Registry::new());
        assert!(items.iter().any(|item| item.label == "start"));
        assert!(!items.iter().any(|item| item.label == "当绿旗被点击"));
    }

    #[test]
    fn indented_init_and_target_context_recover_after_dedent() {
        let (source, cursor) = marked(
            "sprite \"玩家\":\n    init:\n        x: 1\n        rot|\n    start:\n        pass",
        );
        let items = complete(&source, cursor, &Registry::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "rotationStyle");
        assert!(apply(&source, &items[0]).contains("rotationStyle: \"all around\""));
        let (source, cursor) = marked("stage:\n    start:\n        移|");
        assert!(complete(&source, cursor, &Registry::new()).is_empty());
        let (source, cursor) =
            marked("sprite \"玩家\":\n    init:\n        pass\n    start:\n        移|");
        assert!(
            complete(&source, cursor, &Registry::new())
                .iter()
                .any(|item| item.label == "移动(0)步")
        );
        let (source, cursor) = marked("sprite \"玩家\":\n    init:\n        x: 1\n    sta|");
        assert!(
            complete(&source, cursor, &Registry::new())
                .iter()
                .any(|item| item.label == "start")
        );
        let (source, cursor) = marked(
            "sprite \"玩家\":\n    items = {1, 2}\n    init:\n        costumes: [{name: \"猫\", path: \"cat.svg\"}]\n    sta|",
        );
        assert!(
            complete(&source, cursor, &Registry::new())
                .iter()
                .any(|item| item.label == "start")
        );
    }

    #[test]
    fn modern_declarations_and_menus_do_not_offer_value_completions() {
        let symbols = data_symbols();
        for mark in [
            "items = {sc|}",
            "score = sc|",
            "def 示例 @sc|:",
            "sprite \"玩家\":\n    init:\n        x: \"sc|\"",
            "移到 ('sc|')",
            "说 'text # \\' sc|'",
        ] {
            let (_, items) = symbol_candidates(mark, &symbols);
            assert!(items.is_empty(), "{mark}");
        }
        let (source, cursor) = marked(
            "sprite \"玩家\":\n    score = 0\n    def 等待 @seconds:\n        等待 (@sec|) 秒\n    start:\n        pass",
        );
        let items = complete(&source, cursor, &Registry::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "@seconds");
    }

    #[test]
    fn compact_scalar_inputs_nest_reporters_without_command_brackets() {
        let completed = choose("start:\n    说(连|)", "operator_join");
        assert_eq!(completed, "start:\n    说(连接(\"\")和(\"\"))");
        compile_source(&format!(
            "sprite \"玩家\":\n    init:\n        pass\n    end\n{}\n    end\nend",
            completed
                .lines()
                .map(|line| format!("    {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        assert_eq!(
            choose("start:\n    等待|", "control_wait"),
            "start:\n    等待(0)秒"
        );
        for mark in [
            "start:\n    [移|]",
            "start:\n    说 sc|",
            "start:\n    显示变量(@sc|)",
        ] {
            let (_, items) = symbol_candidates(mark, &data_symbols());
            assert!(items.is_empty(), "{mark}: {items:?}");
        }
        let (source, items) = symbol_candidates("start:\n    lo|旧 = {1}", &data_symbols());
        let list = items.iter().find(|item| item.label == "logs").unwrap();
        assert_eq!(apply(&source, list), "start:\n    logs = {1}");
        let (source, items) = symbol_candidates("start:\n    logs |", &data_symbols());
        assert_eq!(items.len(), 1);
        assert_eq!(apply(&source, &items[0]), "start:\n    logs = {}");
    }

    #[test]
    fn incomplete_chinese_command_applies_and_compiles() {
        let source = choose(
            "sprite \"玩家\" :\n    init :\n        pass\n    end\n    start :\n        移|\n    end\nend",
            "motion_movesteps",
        );
        assert!(source.contains("移动(0)步"));
        assert!(!source.contains("]]"));
        compile_source(&source);
        assert_eq!(choose("移动|", "motion_movesteps"), "移动(0)步");
    }

    #[test]
    fn reports_booleans_and_existing_closers() {
        let registry = Registry::new();
        let reporter = choose("(x|)", "motion_xposition");
        assert_eq!(reporter, "(x 坐标)");
        registry.parse(&reporter, Shape::Reporter, &[]).unwrap();
        let boolean = choose("<鼠|>", "sensing_mousedown");
        assert_eq!(boolean, "<鼠标按下?>");
        registry.parse(&boolean, Shape::Boolean, &[]).unwrap();
        assert_eq!(choose("说 (x|)", "motion_xposition"), "说 (x 坐标)");
    }

    #[test]
    fn every_offered_builtin_default_snippet_resolves_to_its_template() {
        let registry = Registry::new();
        let mut omitted = BTreeSet::new();
        for template in &registry.templates {
            let Some((_, snippet)) = completion_text_visible(template, &registry, &[]) else {
                omitted.insert(template.opcode.as_str());
                continue;
            };
            let text = expand(&snippet);
            let call = registry
                .parse_typed(
                    &text,
                    template.shape,
                    &[],
                    &template_default_types(template),
                )
                .unwrap_or_else(|error| panic!("{}: {text}: {error}", template.opcode));
            assert_eq!(call.template.opcode, template.opcode);
        }
        // Known scalar/list defaults disambiguate otherwise identical text.
        assert!(omitted.is_empty(), "{omitted:?}");
        let (source, cursor) = marked("<|");
        let items = complete(&source, cursor, &registry);
        assert!(items.iter().any(|c| {
            c.detail
                .as_ref()
                .is_some_and(|d| d.starts_with("data_listcontainsitem ·"))
        }));
        assert!(items.iter().any(|c| {
            c.detail
                .as_ref()
                .is_some_and(|d| d.starts_with("operator_contains ·"))
        }));
    }

    #[test]
    fn utf16_ranges_preserve_emoji_and_chinese_before_the_edit() {
        let (source, cursor) = marked("# 😀中文 # 移|");
        assert_eq!(cursor.character, 10);
        let candidates = complete(&source, cursor, &Registry::new());
        let selected = candidates.iter().find(|c| c.label == "移动(0)步").unwrap();
        let Some(CompletionTextEdit::Edit(edit)) = &selected.text_edit else {
            panic!();
        };
        assert_eq!(edit.range.start.character, 9);
        assert_eq!(apply(&source, selected), "# 😀中文 # 移动(0)步");
        assert!(byte_offset("😀", Position::new(0, 1)).is_none());
        assert_eq!(byte_offset("😀\r\n中", Position::new(1, 1)), Some(9));
    }

    #[test]
    fn comments_strings_and_menu_contents_do_not_offer_blocks() {
        for source in [
            "# 注释 移|",
            "#\n块注释\n移|\n#",
            "# 行内 移| #",
            "说 (\"移|\")",
            "移到 ('移|')",
            "移到 ('a/# 移|')",
            r"移到 ('a\'/# 移|')",
            r"移到 ('a\\/# 移|')",
            "移到 ('unfinished/# 移|",
            "说 (\"# 移|\")",
        ] {
            let (source, cursor) = marked(source);
            assert!(
                complete(&source, cursor, &Registry::new()).is_empty(),
                "{source}"
            );
        }
        assert_eq!(
            choose("# 注释\n移|", "motion_movesteps"),
            "# 注释\n移动(0)步"
        );
        assert_eq!(
            choose("#\n块\n#\n移|", "motion_movesteps"),
            "#\n块\n#\n移动(0)步"
        );
    }

    #[test]
    fn division_comparison_and_star_menus_keep_expression_context() {
        assert_eq!(choose("(8 / (x|))", "motion_xposition"), "(8 / (x 坐标))");
        assert_eq!(choose("<1 < (x|)>", "motion_xposition"), "<1 < (x 坐标)>");
        assert_eq!(choose("<1 > (x|)>", "motion_xposition"), "<1 > (x 坐标)>");
        assert_eq!(
            choose("移到 ('*')\n移|", "motion_movesteps"),
            "移到 ('*')\n移动(0)步"
        );
        assert_eq!(
            choose("移到 ('')\n移|", "motion_movesteps"),
            "移到 ('')\n移动(0)步"
        );
        assert_eq!(
            choose("移到 'unfinished/#\n移|", "motion_movesteps"),
            "移到 'unfinished/#\n移动(0)步"
        );
    }

    #[test]
    fn infix_right_operands_complete_with_spacing_nesting_and_utf16() {
        let symbols = data_symbols();
        for (marked_source, expected) in [
            ("(score / sc|旧)", "(score / score)"),
            ("(score/sc|旧)", "(score/score)"),
            ("(score\n/\nsc|旧)", "(score\n/\nscore)"),
            ("((score + 1)/sc|旧)", "((score + 1)/score)"),
            ("(score + sc|旧)", "(score + score)"),
            ("(score-sc|旧)", "(score-score)"),
            ("(score*sc|旧)", "(score*score)"),
            ("(score % sc|旧)", "(score % score)"),
            ("<score > sc|旧>", "<score > score>"),
            ("<score>sc|旧>", "<score>score>"),
            ("<score\n>\nsc|旧>", "<score\n>\nscore>"),
            ("<score=sc|旧>", "<score=score>"),
            ("<score<sc|旧>", "<score<score>"),
            ("# 😀 # <score>特殊分|旧>", "# 😀 # <score>特殊分数>"),
            ("# 😀 # (score/特殊分|旧)", "# 😀 # (score/特殊分数)"),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}: {items:?}");
            assert_eq!(apply(&source, &items[0]), expected, "{marked_source}");
        }
        for marked_source in ["<score > |>", "(score / |)", "<score>|>"] {
            let (_, items) = symbol_candidates(marked_source, &symbols);
            assert!(
                items.iter().any(|item| item.label == "score"),
                "{marked_source}"
            );
        }
    }

    #[test]
    fn nested_boolean_slots_do_not_consume_a_later_template_argument() {
        let symbols = data_symbols();
        for prior in [
            "<ext.模板 <@ready> (1)>",
            "<ext.模板 <@ready>\n(1)>",
            "<ext.模板 <true> (1)>",
            "<ext.模板 <score > 1> (2)>",
            "<ext.模板 <ext.模板 <@ready> (1)> (2)>",
            r#"<ext::"check"(<@ready>, (1))>"#,
        ] {
            let marked = format!("说({prior})\n说 (sc|旧)");
            let (source, items) = symbol_candidates(&marked, &symbols);
            assert_eq!(items.len(), 1, "{prior}: {items:?}");
            assert_eq!(
                apply(&source, &items[0]),
                format!("说({prior})\n说 (score)")
            );
        }
    }

    #[test]
    fn init_keys_enums_and_existing_quotes() {
        let registry = Registry::new();
        for marked_source in [
            "sprite \"玩家\" :\n    init :\n        |\n    end\nend",
            "sprite \"玩家\" :\n    init :\n        \"rot|\"\n    end\nend",
        ] {
            let (source, cursor) = marked(marked_source);
            let items = complete(&source, cursor, &registry);
            let selected = items.iter().find(|i| i.label == "rotationStyle").unwrap();
            compile_source(&apply(&source, selected));
        }
        let (source, cursor) =
            marked("sprite \"玩家\" :\n    init :\n        \"rotationStyle\": \"le|\"\n    end\nend");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "left-right");
        compile_source(&apply(&source, &items[0]));
        let (source, cursor) = marked(
            "sprite \"玩家\" :\n    init :\n        \"visible\": f|\n    end\nend",
        );
        let items = complete(&source, cursor, &registry);
        assert_eq!(items[0].label, "false");
        compile_source(&apply(&source, &items[0]));
    }

    #[test]
    fn init_nested_assets_and_duplicate_fields_are_contextual() {
        let registry = Registry::new();
        let (source, cursor) = marked("sprite \"玩家\" :\n    init :\n        \"x\": 1, |");
        let items = complete(&source, cursor, &registry);
        assert!(!items.iter().any(|i| i.label == "x"));
        assert!(items.iter().any(|i| i.label == "y"));
        let (source, cursor) = marked("sprite \"玩家\" :\n    init :\n        \"sounds\": [{ | }]");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i.label == "path"));
        assert!(!items.iter().any(|i| i.label == "rotationCenterX"));
        for source in [
            "sprite \"玩家\" :\n    init :\n        # 注释 |",
            "sprite \"玩家\" :\n    init :\n        \"costumes\": [{ \"path\": \"移|\" }]",
        ] {
            let (source, cursor) = marked(source);
            assert!(complete(&source, cursor, &registry).is_empty());
        }
    }

    #[test]
    fn bare_init_fields_complete_after_newlines_without_adding_quotes() {
        for marked_source in [
            "sprite \"玩家\" :\n    init :\n        rot|\n    end\nend",
            "sprite \"玩家\" :\n    init :\n        x: 1\n        rot|\n    end\nend",
            "sprite \"玩家\" :\n    init :\n        x: 1 # 坐标\n        rot|\n    end\nend",
            "sprite \"玩家\" :\n    init :\n        x: 1\n        #\n        注释\n        #\n        rot|\n    end\nend",
        ] {
            let (source, cursor) = marked(marked_source);
            let items = complete(&source, cursor, &Registry::new());
            let candidate = items
                .iter()
                .find(|item| item.label == "rotationStyle")
                .unwrap();
            let result = apply(&source, candidate);
            assert!(result.contains("rotationStyle:"));
            assert!(!result.contains("\"rotationStyle\""));
            compile_source(&result);
        }
        let (source, cursor) = marked("sprite \"玩家\" :\n    init :\n        x: 1\n        |");
        let items = complete(&source, cursor, &Registry::new());
        assert!(!items.iter().any(|item| item.label == "x"));
        assert!(items.iter().any(|item| item.label == "y"));
        let (source, cursor) = marked(
            "sprite \"玩家\" :\n    init :\n        sounds: [{ name: \"音效\"\n        pa| }]",
        );
        let items = complete(&source, cursor, &Registry::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "path");
    }

    #[test]
    fn build_file_completions_follow_nested_data_schemas() {
        for (marked_source, label) in [
            ("ent|", "entry"),
            ("# 工程\nent|", "entry"),
            ("{ ent| }", "entry"),
            ("entry: \"main.sbdsl\"\n out|", "output"),
            ("runtime: { fps: 60\n sta| }", "stageWidth"),
            ("runtime: { fencing: f| }", "false"),
            ("stage: { backdrops: [{ na| }] }", "name"),
        ] {
            let (source, cursor) = marked(marked_source);
            let items = complete_config(&source, cursor);
            let candidate = items
                .iter()
                .find(|item| item.label == label)
                .unwrap_or_else(|| panic!("missing {label} for {marked_source}: {items:?}"));
            let result = apply(&source, candidate);
            crate::notation::parse_build::<serde_json::Value>(
                std::path::Path::new("sbdsl.buildsb"),
                &result,
            )
            .unwrap_or_else(|error| panic!("{result}: {error}"));
        }
        let (source, cursor) = marked("entry: \"main.sbdsl\"\n|");
        let items = complete_config(&source, cursor);
        assert!(!items.iter().any(|item| item.label == "entry"));
        assert!(items.iter().any(|item| item.label == "output"));
        for blocked in [
            "# comm|ent\n",
            "entry: \"pa|th\"",
            "stage: { sounds: [{ path: \"ab|c\" }] }",
        ] {
            let (source, cursor) = marked(blocked);
            assert!(complete_config(&source, cursor).is_empty(), "{blocked}");
        }
    }

    #[test]
    fn data_completion_preserves_following_fields_and_comment_separated_colons() {
        for (marked_source, label) in [
            ("\"ent|\noutput: \"x.sb3\"", "entry"),
            ("runtime: { fp| # comment # : 60 }", "fps"),
            ("entry: \"main.sbdsl\"\n|\noutput: \"x.sb3\"", "srcDir"),
        ] {
            let (source, cursor) = marked(marked_source);
            let items = complete_config(&source, cursor);
            let candidate = items
                .iter()
                .find(|item| item.label == label)
                .unwrap_or_else(|| panic!("missing {label}: {items:?}"));
            let completed = apply(&source, candidate);
            let config = crate::notation::parse_build::<crate::config::Config>(
                std::path::Path::new("sbdsl.buildsb"),
                &completed,
            )
            .unwrap_or_else(|error| panic!("{completed}: {error}"));
            if source.contains("output:") {
                assert_eq!(
                    config.output.as_deref(),
                    Some(std::path::Path::new("x.sb3"))
                );
            } else {
                assert!(completed.contains("fps # comment # : 60"), "{completed}");
            }
        }
    }

    #[test]
    fn quoted_namespaces_and_snippet_metacharacters_round_trip() {
        let mut registry = Registry::new();
        let mut slot = crate::registry::input("TEXT", ArgType::String);
        slot.default = serde_json::json!("$价格}\\路径");
        registry.templates.push(Template {
            opcode: "custom_show".into(),
            shape: Shape::Command,
            namespace: Some("自定义 拓展".into()),
            extension_id: Some("custom".into()),
            local_opcode: Some("show".into()),
            tokens: crate::registry::tokens_from_text("展示 [TEXT]", &[slot]).unwrap(),
            sprite_only: false,
            stage_only: false,
            procedure: None,
            warp: false,
            branch_count: 0,
        });
        let (source, cursor) = marked("\"自定义 拓展\".展|");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        let applied = apply(&source, &items[0]);
        assert_eq!(applied, "\"自定义 拓展\".展示(\"$价格}\\\\路径\")");
        registry.parse(&applied, Shape::Command, &[]).unwrap();
    }

    #[test]
    fn menu_completion_defaults_quote_and_escape_every_option() {
        for value in [
            "space",
            "",
            "*",
            "/",
            "a'b",
            "a\\b",
            "a\"b",
            "a\nb",
            "$价格}\\路径'/",
        ] {
            let mut registry = Registry::new();
            let mut slot = crate::registry::input("CHOICE", ArgType::String);
            slot.default = serde_json::json!(value);
            slot.menu = Some(crate::model::Menu {
                items: Some(vec![("选项".into(), value.into())]),
                accept_reporters: false,
            });
            registry.templates.push(Template {
                opcode: "custom_choice".into(),
                shape: Shape::Command,
                namespace: Some("自定义拓展".into()),
                extension_id: Some("custom".into()),
                local_opcode: Some("choice".into()),
                tokens: crate::registry::tokens_from_text("选择 [CHOICE]", &[slot]).unwrap(),
                sprite_only: false,
                stage_only: false,
                procedure: None,
                warp: false,
                branch_count: 0,
            });
            let (source, cursor) = marked("自定义拓展.选|");
            let items = complete(&source, cursor, &registry);
            assert_eq!(items.len(), 1, "{value:?}");
            let applied = apply(&source, &items[0]);
            let expected = crate::parser::format_menu(value);
            assert_eq!(applied, format!("自定义拓展.选择{expected}"));
            registry.parse(&applied, Shape::Command, &[]).unwrap();
            assert_eq!(crate::parser::parse_menu(&expected).unwrap(), value);
        }
    }

    #[test]
    fn bare_filename_namespaces_complete_without_consuming_numeric_operands() {
        for namespace in [
            "scratch-tensor-extension",
            "自定义拓展.min",
            "扩展_二2",
            "_local",
        ] {
            let mut registry = Registry::new();
            for (opcode, shape, text) in [
                ("show", Shape::Command, "展示"),
                ("number", Shape::Reporter, "数字"),
            ] {
                registry.templates.push(Template {
                    opcode: format!("custom_{opcode}"),
                    shape,
                    namespace: Some(namespace.into()),
                    extension_id: Some("custom".into()),
                    local_opcode: Some(opcode.into()),
                    tokens: crate::registry::tokens_from_text(text, &[]).unwrap(),
                    sprite_only: false,
                    stage_only: false,
                    procedure: None,
                    warp: false,
                    branch_count: 0,
                });
            }
            for prefix in [namespace.to_owned(), quote(namespace)] {
                let (source, cursor) = marked(&format!("{prefix}.展|"));
                let items = complete(&source, cursor, &registry);
                assert_eq!(items.len(), 1, "{source}: {items:?}");
                let applied = apply(&source, &items[0]);
                assert_eq!(applied, format!("{namespace}.展示"));
                registry.parse(&applied, Shape::Command, &[]).unwrap();
            }
            for number in ["1", "1.2", "-1", "1e-2"] {
                let (source, cursor) = marked(&format!("({number}-({namespace}.数|))"));
                let items = complete(&source, cursor, &registry);
                assert_eq!(items.len(), 1, "{source}: {items:?}");
                let applied = apply(&source, &items[0]);
                assert_eq!(applied, format!("({number}-({namespace}.数字))"));
                registry.parse(&applied, Shape::Reporter, &[]).unwrap();
            }
        }
        assert!(namespace_name_before("1extension").is_none());
        assert!(namespace_name_before("1.extension").is_none());
    }

    #[test]
    fn extension_completion_preserves_reference_operands_before_minus() {
        for (reference, name) in [
            ("score", "score"),
            ("@value", "value"),
            ("global:score", "score"),
            ("items", "items"),
            ("global:items", "items"),
            ("变量", "变量"),
            ("@参数", "参数"),
            ("变量名", "变量名"),
            ("@\"参数 with spaces\"", "参数 with spaces"),
        ] {
            for namespace in ["ext", "scratch-tensor-extension", "自定义拓展.min"] {
                let mut registry = Registry::new();
                let conflicting = format!("{name}-{namespace}");
                for (namespace, opcode) in [(namespace, "number"), (&conflicting, "wrong")] {
                    registry.templates.push(Template {
                        opcode: format!("custom_{opcode}"),
                        shape: Shape::Reporter,
                        namespace: Some(namespace.into()),
                        extension_id: Some("custom".into()),
                        local_opcode: Some(opcode.into()),
                        tokens: crate::registry::tokens_from_text("数字", &[]).unwrap(),
                        sprite_only: false,
                        stage_only: false,
                        procedure: None,
                        warp: false,
                        branch_count: 0,
                    });
                }
                let separator = if reference.contains(':') || reference.starts_with('@') {
                    "-"
                } else {
                    " - "
                };
                let (source, cursor) =
                    marked(&format!("({reference}{separator}({namespace}.数|))"));
                let items = complete(&source, cursor, &registry);
                assert_eq!(items.len(), 1, "{source}: {items:?}");
                assert_eq!(items[0].label, format!("{namespace}.数字"));
                let applied = apply(&source, &items[0]);
                assert_eq!(
                    applied,
                    format!("({reference}{separator}({namespace}.数字))")
                );
                registry.parse(&applied, Shape::Reporter, &[]).unwrap();
            }
        }
    }

    #[test]
    fn ambiguous_extension_templates_use_explicit_opcodes() {
        let mut registry = Registry::new();
        for opcode in ["showA", "showB"] {
            registry.templates.push(Template {
                opcode: format!("custom_{opcode}"),
                shape: Shape::Command,
                namespace: Some("自定义拓展".into()),
                extension_id: Some("custom".into()),
                local_opcode: Some(opcode.into()),
                tokens: crate::registry::tokens_from_text(
                    "展示 [TEXT]",
                    &[crate::registry::input("TEXT", ArgType::String)],
                )
                .unwrap(),
                sprite_only: false,
                stage_only: false,
                procedure: None,
                warp: false,
                branch_count: 0,
            });
        }
        let (source, cursor) = marked("\"自定义拓展\".展|");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 2);
        for selected in &items {
            assert!(selected.filter_text.as_deref().unwrap().contains("展示"));
            let applied = apply(&source, selected);
            assert!(applied.starts_with("自定义拓展::\"show"));
            assert!(!applied.ends_with("]]"));
            let call = registry.parse(&applied, Shape::Command, &[]).unwrap();
            assert!(
                selected
                    .detail
                    .as_ref()
                    .unwrap()
                    .starts_with(&call.template.opcode)
            );
        }
    }

    fn control_template(opcode: &str, branches: usize) -> Template {
        let mut value = crate::registry::input("VALUE", ArgType::Number);
        value.default = serde_json::json!(3);
        let mut list = crate::registry::input("LIST", ArgType::List);
        list.default = serde_json::json!("logs");
        Template {
            opcode: format!("custom_{opcode}"),
            shape: Shape::Control,
            namespace: Some("分支 扩展😀".into()),
            extension_id: Some("custom".into()),
            local_opcode: Some(opcode.into()),
            tokens: crate::registry::tokens_from_text("分支 [VALUE] 记录 [LIST]", &[value, list])
                .unwrap(),
            sprite_only: false,
            stage_only: false,
            procedure: None,
            warp: false,
            branch_count: branches,
        }
    }

    #[test]
    fn extension_controls_complete_every_branch_and_preserve_existing_suites() {
        for branches in [1, 3] {
            let mut registry = Registry::new();
            let template = control_template("choose", branches);
            registry.templates.push(template.clone());
            let (source, cursor) = marked("start:\n    \"分支 扩展😀\".分|");
            let items = complete(&source, cursor, &registry);
            assert_eq!(items.len(), 1, "{items:?}");
            let applied = apply(&source, &items[0]);
            let expected = format!(
                "start:\n    \"分支 扩展😀\".分支(3)记录(logs):\n        pass{}\n    end",
                "\n    else:\n        pass".repeat(branches - 1)
            );
            assert_eq!(applied, expected);
            let header = applied.lines().nth(1).unwrap().trim().trim_end_matches(':');
            let call = registry
                .parse_typed(
                    header,
                    Shape::Control,
                    &[],
                    &template_default_types(&template),
                )
                .unwrap();
            assert_eq!(call.template.branch_count, branches);

            let (source, cursor) =
                marked("start:\n    \"分支 扩展😀\".分|:\n        说 (\"保留\")");
            let items = complete(&source, cursor, &registry);
            assert_eq!(items.len(), 1);
            assert_eq!(
                apply(&source, &items[0]),
                "start:\n    \"分支 扩展😀\".分支(3)记录(logs):\n        说 (\"保留\")"
            );
        }
    }

    #[test]
    fn huge_extension_branch_counts_do_not_allocate_snippets() {
        let mut registry = Registry::new();
        registry
            .templates
            .push(control_template("huge", usize::MAX));
        let (source, cursor) = marked("start:\n    \"分支 扩展😀\".分|");
        assert!(complete(&source, cursor, &registry).is_empty());
        let (source, cursor) = marked("start:\n    \"分支 扩展😀\".分|:\n        pass");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        assert_eq!(
            apply(&source, &items[0]),
            "start:\n    \"分支 扩展😀\".分支(3)记录(logs):\n        pass"
        );
    }

    #[test]
    fn ambiguous_extension_controls_offer_explicit_opcodes_with_all_branches() {
        let mut registry = Registry::new();
        for opcode in ["chooseA", "chooseB"] {
            registry.templates.push(control_template(opcode, 2));
        }
        let (source, cursor) = marked("start:\n    \"分支 扩展😀\".分|");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 2, "{items:?}");
        for item in &items {
            let applied = apply(&source, item);
            assert!(applied.contains("::\"choose"), "{applied}");
            assert!(
                applied.ends_with(":\n        pass\n    else:\n        pass\n    end"),
                "{applied}"
            );
            let header = applied.lines().nth(1).unwrap().trim().trim_end_matches(':');
            let call = registry
                .parse_typed(
                    header,
                    Shape::Control,
                    &[],
                    &template_default_types(registry.templates.last().unwrap()),
                )
                .unwrap();
            assert!(
                item.detail
                    .as_ref()
                    .unwrap()
                    .starts_with(&call.template.opcode)
            );
        }
    }

    #[test]
    fn extension_control_inputs_complete_scalar_and_list_names_by_slot() {
        let mut registry = Registry::new();
        registry.templates.push(control_template("choose", 2));
        let symbols = data_symbols();
        for (marked_source, expected, excluded) in [
            (
                "start:\n    \"分支 扩展😀\".分支 (sc|旧) 记录 (logs):\n        pass",
                "score",
                "logs",
            ),
            (
                "start:\n    \"分支 扩展😀\".分支 (3) 记录 (lo|旧):\n        pass",
                "logs",
                "score",
            ),
            (
                "start:\n    \"分支 扩展😀\".分支 (3) 记录 (|):\n        pass",
                "logs",
                "score",
            ),
            (
                "start:\n    \"分支 扩展😀\"::\"choose\"((sc|旧), (logs)):\n        pass",
                "score",
                "logs",
            ),
            (
                "start:\n    \"分支 扩展😀\"::\"choose\"((3), (|)):\n        pass",
                "logs",
                "score",
            ),
        ] {
            let (source, cursor) = marked(marked_source);
            let items = complete_with_symbols(&source, cursor, &registry, &symbols);
            let chosen = items
                .iter()
                .find(|item| item.label == expected)
                .unwrap_or_else(|| panic!("{marked_source}: {items:?}"));
            assert!(
                !items.iter().any(|item| item.label == excluded),
                "{marked_source}: {items:?}"
            );
            assert!(!apply(&source, chosen).contains('旧'));
            assert!(apply(&source, chosen).ends_with(":\n        pass"));
        }
    }

    #[test]
    fn existing_control_suites_disambiguate_same_text_command_slots() {
        let mut registry = Registry::new();
        let control = control_template("choose", 1);
        let mut command = control.clone();
        command.opcode = "custom_show".into();
        command.local_opcode = Some("show".into());
        command.shape = Shape::Command;
        command.branch_count = 0;
        for token in &mut command.tokens {
            if let Token::Slot(slot) = token
                && slot.arg_type == ArgType::List
            {
                slot.arg_type = ArgType::String;
                slot.default = serde_json::json!("");
            }
        }
        registry.templates.extend([command, control]);
        let (source, cursor) = marked("start:\n    \"分支 扩展😀\".分|:\n        pass");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(
            items[0]
                .detail
                .as_ref()
                .unwrap()
                .starts_with("custom_choose")
        );

        let (source, cursor) =
            marked("start:\n    \"分支 扩展😀\".分支 (3) 记录 (|):\n        pass");
        let items = complete_with_symbols(&source, cursor, &registry, &data_symbols());
        assert!(items.iter().any(|item| item.label == "logs"));
        assert!(!items.iter().any(|item| item.label == "score"), "{items:?}");
    }

    #[test]
    fn explicit_targets_filter_only_when_known() {
        let registry = Registry::new();
        let (source, cursor) = marked("stage :\n    start :\n        移|");
        assert!(complete(&source, cursor, &registry).is_empty());
        let (source, cursor) = marked("sprite \"玩家\" :\n    init :\n        pass\n    当|");
        let items = complete(&source, cursor, &registry);
        assert!(items.iter().any(|i| i.label == "当角色被点击"));
        assert!(!items.iter().any(|i| i.label == "当舞台被点击"));
        let (source, cursor) = marked("移|");
        assert!(!complete(&source, cursor, &registry).is_empty());
    }

    #[test]
    fn existing_arguments_are_not_replaced() {
        let registry = Registry::new();
        for marked_source in ["移动 (12)|", "移|动 (12) 步"] {
            let (source, cursor) = marked(marked_source);
            assert!(complete(&source, cursor, &registry).is_empty());
        }
        for marked_source in [
            "sprite \"玩家\" :\n    init :\n        |\"x\": 1",
            "sprite \"玩家\" :\n    init :\n        \"visible\": |true",
        ] {
            let (source, cursor) = marked(marked_source);
            assert!(complete(&source, cursor, &registry).is_empty());
        }
    }

    fn symbol_candidates(mark: &str, symbols: &VisibleSymbols) -> (String, Vec<CompletionItem>) {
        let (source, cursor) = marked(mark);
        let items = complete_with_symbols(&source, cursor, &Registry::new(), symbols);
        (source, items)
    }

    fn data_symbols() -> VisibleSymbols {
        use super::super::symbols::DataSymbol;
        VisibleSymbols {
            variables: vec![
                DataSymbol {
                    name: "score".into(),
                    global: true,
                },
                DataSymbol {
                    name: "score".into(),
                    global: false,
                },
                DataSymbol {
                    name: "生命值".into(),
                    global: false,
                },
                DataSymbol {
                    name: "特殊分数".into(),
                    global: true,
                },
                DataSymbol {
                    name: "特殊名称".into(),
                    global: true,
                },
            ],
            lists: vec![
                DataSymbol {
                    name: "logs".into(),
                    global: true,
                },
                DataSymbol {
                    name: "logs".into(),
                    global: false,
                },
                DataSymbol {
                    name: "本地记录".into(),
                    global: false,
                },
            ],
            procedures: Vec::new(),
            parameters: Vec::new(),
            scopes: None,
        }
    }

    #[test]
    fn bare_names_complete_value_slots_without_replacing_fixed_words() {
        let mut symbols = data_symbols();
        symbols.variables.push(super::super::symbols::DataSymbol {
            name: "设为".into(),
            global: false,
        });
        for (mark, expected) in [
            ("start:\n    sc|旧 = score+1", "start:\n    score = score+1"),
            ("移动 (sc|旧) 步", "移动 (score) 步"),
            ("说 (sc|旧+1)", "说 (score+1)"),
            (
                "start:\n    loop sc|旧:\n        pass",
                "start:\n    loop score:\n        pass",
            ),
            (
                "start:\n    if <sc|旧 < 4>:\n        pass",
                "start:\n    if <score < 4>:\n        pass",
            ),
            (
                "start:\n    while <sc|旧 < 4>:\n        pass",
                "start:\n    while <score < 4>:\n        pass",
            ),
        ] {
            let (source, items) = symbol_candidates(mark, &symbols);
            let selected = items.iter().find(|item| item.label == "score").expect(mark);
            assert_eq!(apply(&source, selected), expected, "{mark}");
        }
        for mark in [
            "将 (score) 设|为 (12)",
            "[说 $sc|旧]",
            "[说 (score + $sc|旧)]",
        ] {
            let (_, items) = symbol_candidates(mark, &symbols);
            assert!(items.is_empty(), "{mark}: {items:?}");
        }
        let (source, items) = symbol_candidates("说 (\"保留\") (|)", &symbols);
        let selected = items.iter().find(|item| item.label == "score").unwrap();
        assert_eq!(apply(&source, selected), "说 (\"保留\") (score)");
        let (_, items) = symbol_candidates("start:\n    loop |:\n        pass", &symbols);
        assert!(items.iter().any(|item| item.label == "score"));
    }

    #[test]
    fn assignment_targets_rhs_and_operators_complete_without_replacing_other_text() {
        let mut symbols = data_symbols();
        symbols
            .parameters
            .push(crate::registry::input("steps", ArgType::Any));
        for (mark, label, expected) in [
            ("start:\n    sc|旧 = 10", "score", "start:\n    score = 10"),
            (
                "start:\n    # 😀 # 生|旧 += 1",
                "生命值",
                "start:\n    # 😀 # 生命值 += 1",
            ),
            (
                "start:\n    global:sc|旧 *= 2",
                "global:score",
                "start:\n    global:score *= 2",
            ),
            (
                "start:\n    特殊分|旧 -= 1",
                "特殊分数",
                "start:\n    特殊分数 -= 1",
            ),
            (
                "start:\n    global:特殊分|旧 /= 2",
                "global:特殊分数",
                "start:\n    global:特殊分数 /= 2",
            ),
            (
                "start:\n    score = 1+sc|旧*2",
                "score",
                "start:\n    score = 1+score*2",
            ),
            (
                "start:\n    score = @st|旧 + score",
                "@steps",
                "start:\n    score = @steps + score",
            ),
            (
                "start:\n    score = 特殊分|旧",
                "特殊分数",
                "start:\n    score = 特殊分数",
            ),
        ] {
            let (source, items) = symbol_candidates(mark, &symbols);
            let selected = items.iter().find(|item| item.label == label).expect(mark);
            assert_eq!(apply(&source, selected), expected, "{mark}");
        }
        for operator in ["=", "+=", "-=", "*=", "/=", "%="] {
            for suffix in ["sc|旧", "1 + sc|旧", "|"] {
                let mark = format!("start:\n    score {operator} {suffix}");
                let (_, items) = symbol_candidates(&mark, &symbols);
                assert!(
                    items.iter().any(|item| item.label == "score"),
                    "{mark}: {items:?}"
                );
            }
            let (source, items) = symbol_candidates("start:\n    score |", &symbols);
            let selected = items.iter().find(|item| item.label == operator).unwrap();
            let applied = apply(&source, selected);
            assert_eq!(
                applied,
                format!(
                    "start:\n    score {operator} {}",
                    if operator == "=" { 0 } else { 1 }
                )
            );
        }
        let (source, items) = symbol_candidates("start:\n    score +|= 4", &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(apply(&source, &items[0]), "start:\n    score += 4");
        let (_, items) = symbol_candidates("start:\n    |", &symbols);
        assert!(items.iter().any(|item| item.label == "score"));
        assert!(
            !items.iter().any(|item| item.label.starts_with('@')),
            "{items:?}"
        );
        for mark in [
            "start:\n    lo|gs = 1",
            "start:\n    logs +|",
            "start:\n    score = lo|gs",
            "start:\n    score = global:lo|gs",
            "start:\n    list:lo|gs = 1",
            "start:\n    @st|eps = 1",
            "score = sc|旧",
            "var sc|旧 = 1",
            "start:\n    score = \"sc|旧\"",
            "start:\n    score = 'sc|旧'",
            "start:\n    score = 1 # sc|旧",
        ] {
            let (_, items) = symbol_candidates(mark, &symbols);
            assert!(items.is_empty(), "{mark}: {items:?}");
        }
        assert_eq!(
            choose("start:\n    score = (x|)", "motion_xposition"),
            "start:\n    score = (x 坐标)"
        );
    }

    #[test]
    fn variable_write_snippets_are_assignment_statements() {
        for opcode in ["data_setvariableto", "data_changevariableby"] {
            let applied = choose("start:\n    |", opcode);
            assert!(!applied.contains('['), "{applied}");
            assert!(
                applied.contains(if opcode == "data_setvariableto" {
                    "变量 = "
                } else {
                    "变量 += "
                }),
                "{applied}"
            );
            compile_source(&format!("变量 = 0\n{applied}\nend"));
        }
        let (source, cursor) = marked("start:\n    [|]");
        let items = complete(&source, cursor, &Registry::new());
        assert!(
            !items
                .iter()
                .any(|item| item.detail.as_deref().is_some_and(|detail| {
                    detail.starts_with("data_setvariableto ·")
                        || detail.starts_with("data_changevariableby ·")
                }))
        );
    }

    #[test]
    fn reference_prefixes_in_procedure_fixed_labels_are_preserved() {
        for prefix in ["var", "list", "global"] {
            for first in [false, true] {
                let mut symbols = data_symbols();
                let header = format!("检查 {}{prefix}: @n", if first { "@first " } else { "" });
                symbols
                    .procedures
                    .push(crate::registry::procedure_template(&header, "check").unwrap());
                let entered = format!("检查 {}", if first { "(score) " } else { "" });
                for label in [
                    format!("{prefix}:|"),
                    format!("{}|{}:", &prefix[..1], &prefix[1..]),
                ] {
                    let mark = format!("{entered}{label} (3)");
                    let (_, items) = symbol_candidates(&mark, &symbols);
                    assert!(items.is_empty(), "{mark}: {items:?}");
                }
                let mark = format!("{entered}{prefix}: (sc|旧)");
                let (source, items) = symbol_candidates(&mark, &symbols);
                let selected = items
                    .iter()
                    .find(|item| item.label == "score")
                    .expect(&mark);
                let applied = apply(&source, selected);
                assert_eq!(applied, format!("{entered}{prefix}: (score)"));
                Registry::new()
                    .parse(&applied, Shape::Command, &symbols.procedures)
                    .unwrap();
            }
        }
    }

    #[test]
    fn data_completion_filters_scope_kind_and_preserves_arguments() {
        let symbols = data_symbols();
        let (source, items) = symbol_candidates("start:\n    sc|ore = 12", &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "score");
        assert_eq!(items[0].detail.as_deref(), Some("角色变量"));
        assert_eq!(apply(&source, &items[0]), "start:\n    score = 12");
        let (_, items) = symbol_candidates("说 (|)", &symbols);
        for label in ["score", "生命值", "global:"] {
            assert!(items.iter().any(|item| item.label == label), "{label}");
        }
        assert!(!items.iter().any(|item| item.label == "logs"));
        let (_, items) = symbol_candidates("将 (\"x\") 加入 (|)", &symbols);
        for label in ["logs", "本地记录", "global:"] {
            assert!(
                items.iter().any(|item| item.label == label),
                "{label}: {items:?}"
            );
        }
        assert!(!items.iter().any(|item| item.label == "score"));
        let (_, items) = symbol_candidates("说 (global:|)", &symbols);
        assert!(items.iter().any(|item| item.label == "global:score"));
        assert!(
            !items
                .iter()
                .any(|item| item.label.contains("生命") || item.label.contains("logs"))
        );
        let (source, items) = symbol_candidates("将 (\"x\") 加入 (global:lo|gs)", &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].detail.as_deref(), Some("全局列表"));
        assert_eq!(apply(&source, &items[0]), "将 (\"x\") 加入 (global:logs)");
        let (source, items) = symbol_candidates("删除 (lo|旧) 的全部项目", &symbols);
        assert_eq!(items[0].label, "logs");
        assert_eq!(items[0].filter_text.as_deref(), Some("logs"));
        assert_eq!(apply(&source, &items[0]), "删除 (logs) 的全部项目");
        let (source, items) = symbol_candidates("说 (glob|al:score)", &symbols);
        let prefix = items.iter().find(|item| item.label == "global:").unwrap();
        assert_eq!(apply(&source, prefix), "说 (global:score)");
        for mark in [
            "说 var:|",
            "说 list:|",
            "说 global:list:|",
            "将 (\"x\") 加入 list:lo|",
        ] {
            let (_, items) = symbol_candidates(mark, &symbols);
            assert!(items.is_empty(), "{mark}: {items:?}");
        }
    }

    #[test]
    fn completion_uses_declared_data_kinds_and_surrounding_reporter_slots() {
        let symbols = data_symbols();
        for (mark, present, absent) in [
            ("((lo|旧) 的项目数)", "logs", "score"),
            ("((sc|旧) 的项目数)", "", "score"),
            ("(lo|旧 + (1))", "", "logs"),
            ("将 (\"x\") 加入 (lo|旧)", "logs", "score"),
            ("删除 (lo|旧) 的全部项目", "logs", "score"),
            ("显示变量 (sc|旧)", "score", "logs"),
            ("说 (lo|旧)", "", "logs"),
            ("start:\n    score = lo|旧", "", "logs"),
            ("start:\n    score = ((lo|旧) 的项目数)", "logs", "score"),
        ] {
            let (_, items) = symbol_candidates(mark, &symbols);
            if !present.is_empty() {
                assert!(
                    items.iter().any(|item| item.label == present),
                    "{mark}: {items:?}"
                );
            }
            assert!(
                !items.iter().any(|item| item.label == absent),
                "{mark}: {items:?}"
            );
        }
    }

    #[test]
    fn explicit_extension_inputs_filter_data_candidates_by_slot_kind() {
        let symbols = data_symbols();
        let mut registry = Registry::new();
        registry.templates.push(Template {
            opcode: "custom_append".into(),
            shape: Shape::Command,
            namespace: Some("ext".into()),
            extension_id: Some("custom".into()),
            local_opcode: Some("append".into()),
            tokens: crate::registry::tokens_from_text(
                "更新 [VALUE] 到 [LIST]",
                &[
                    crate::registry::input("VALUE", ArgType::Any),
                    crate::registry::input("LIST", ArgType::List),
                ],
            )
            .unwrap(),
            sprite_only: false,
            stage_only: false,
            procedure: None,
            warp: false,
            branch_count: 0,
        });
        for (mark, expected, absent) in [
            (r#"ext::"append"(("x"), (|))"#, "logs", "score"),
            (r#"ext::"append"(("x"), (lo|旧))"#, "logs", "score"),
            (r#"ext::"append"((|), (logs))"#, "score", "logs"),
            (r#"ext.更新 ("x") 到 (lo|旧)"#, "logs", "score"),
        ] {
            let (source, cursor) = marked(mark);
            let items = complete_with_symbols(&source, cursor, &registry, &symbols);
            assert!(
                items.iter().any(|item| item.label == expected),
                "{mark}: {items:?}"
            );
            assert!(
                !items.iter().any(|item| item.label == absent),
                "{mark}: {items:?}"
            );
        }
    }

    #[test]
    fn ordinary_reference_edits_preserve_utf16_and_reject_quoted_data_names() {
        let symbols = data_symbols();
        for (mark, expected) in [
            ("# 😀 # 说(生|命旧)", "# 😀 # 说(生命值)"),
            ("# 😀 # 说(特殊分|旧)", "# 😀 # 说(特殊分数)"),
            (
                "# 😀 # 删除(本|旧)的全部项目",
                "# 😀 # 删除(本地记录)的全部项目",
            ),
        ] {
            let (source, items) = symbol_candidates(mark, &symbols);
            assert_eq!(items.len(), 1, "{mark}");
            assert_eq!(apply(&source, &items[0]), expected);
        }
        for mark in [
            "说 var:(\"sc|旧\")",
            "说 global:(\"sc|旧\")",
            "说 list:(\"lo|旧\")",
            "start:\n    var:\"sc|旧\" = 1",
            "说 (\"sc|旧\")",
        ] {
            let (_, items) = symbol_candidates(mark, &symbols);
            assert!(items.is_empty(), "{mark}: {items:?}");
        }
    }

    #[test]
    fn data_completion_works_in_nested_values_but_not_literals_or_declarations() {
        let symbols = data_symbols();
        for (marked_source, expected) in [
            ("移动(sc|ore + 1)步", "移动(score + 1)步"),
            ("<8 > (sc|ore / 2)>", "<8 > (score / 2)>"),
            (
                "sprite \"玩家\" :\n    init :\n        pass\n    start :\n        if <sc|ore < 2> :\n            pass",
                "sprite \"玩家\" :\n    init :\n        pass\n    start :\n        if <score < 2> :\n            pass",
            ),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}");
            assert_eq!(apply(&source, &items[0]), expected);
        }
        for marked_source in [
            "说 (\"sc|\")",
            "移到 ('sc|')",
            "说 ((8) 'sc|')",
            "说 ((8) '\"sc|\"')",
            r"移到 ('a\'/sc|')",
            r"移到 ('a\\/sc|')",
            "移到 ('unfinished/sc|",
            "# 注释 sc|",
            "#\n sc|\n#",
            "sprite \"玩家\" :\n    init :\n        \"x\": \"sc|\"",
            "list names = {sc|}",
            "def sc| :\n    pass",
            "def 示例 <@sc|> :\n    pass",
            "import helpers.sc|",
        ] {
            let (_, items) = symbol_candidates(marked_source, &symbols);
            assert!(items.is_empty(), "{marked_source}");
        }
    }

    #[test]
    fn data_references_complete_in_hat_arguments_before_script_bodies() {
        let symbols = data_symbols();
        let mut registry = Registry::new();
        registry.templates.push(Template {
            opcode: "custom_when_number".into(),
            shape: Shape::Hat,
            namespace: Some("ext".into()),
            extension_id: Some("custom".into()),
            local_opcode: Some("when_number".into()),
            tokens: crate::registry::tokens_from_text(
                "当数值 [VALUE] 达标",
                &[crate::registry::input("VALUE", ArgType::Any)],
            )
            .unwrap(),
            sprite_only: false,
            stage_only: false,
            procedure: None,
            warp: false,
            branch_count: 0,
        });
        for (marked_source, expected) in [
            (
                "ext.当数值 (sc|ore) 达标:\n    pass",
                "ext.当数值 (score) 达标:\n    pass",
            ),
            (
                "sprite \"玩家\" :\n    init :\n        pass\n    ext.当数值 (sc|ore) 达标 :\n        pass",
                "sprite \"玩家\" :\n    init :\n        pass\n    ext.当数值 (score) 达标 :\n        pass",
            ),
        ] {
            let (source, cursor) = marked(marked_source);
            let items = complete_with_symbols(&source, cursor, &registry, &symbols);
            assert_eq!(items.len(), 1);
            assert_eq!(apply(&source, &items[0]), expected);
        }
    }

    #[test]
    fn explicit_opcode_arguments_allow_reference_completion_only() {
        let mut symbols = data_symbols();
        symbols.parameters = vec![
            crate::registry::input("seconds", ArgType::Any),
            crate::registry::input("enabled", ArgType::Boolean),
        ];
        for (marked_source, expected) in [
            (r#"ext::"send"((sc|))"#, r#"ext::"send"((score))"#),
            (
                r#"(ext::"read"((sc|), (1)))"#,
                r#"(ext::"read"((score), (1)))"#,
            ),
            (
                r#"<ext::"test"((sc|), (1))>"#,
                r#"<ext::"test"((score), (1))>"#,
            ),
            (r#"ext::"event"((sc|)):"#, r#"ext::"event"((score)):"#),
            (
                r#""自定义拓展"::"send"((sc|), ("保留"))"#,
                r#""自定义拓展"::"send"((score), ("保留"))"#,
            ),
            (r#"ext::"send"((@sec|))"#, r#"ext::"send"((@seconds))"#),
            (r#"(ext::"read"((@sec|)))"#, r#"(ext::"read"((@seconds)))"#),
            (r#"<ext::"test"((@sec|))>"#, r#"<ext::"test"((@seconds))>"#),
            (r#"ext::"event"((@sec|)):"#, r#"ext::"event"((@seconds)):"#),
            (r#"<ext::"test"(<@en|>)>"#, r#"<ext::"test"(<@enabled>)>"#),
            (
                r#"ext::"send"(((ext::"read"((sc|))) + (1)))"#,
                r#"ext::"send"(((ext::"read"((score))) + (1)))"#,
            ),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}");
            assert_eq!(apply(&source, &items[0]), expected, "{marked_source}");
        }
        for marked_source in [
            r#"ext::"send"((|))"#,
            r#"(ext::"read"((|)))"#,
            r#"<ext::"test"((|))>"#,
            r#"ext::"event"((|)):"#,
        ] {
            let (_, items) = symbol_candidates(marked_source, &symbols);
            assert!(
                items.iter().any(|item| item.label == "score"),
                "{marked_source}"
            );
        }
        let (source, cursor) = marked(r#"(ext::"read"((x|)))"#);
        let items = complete_with_symbols(&source, cursor, &Registry::new(), &symbols);
        assert!(items.iter().any(|item| item.label == "x 坐标"));
    }

    #[test]
    fn quoted_reference_edits_preserve_adjacent_atoms() {
        let mut symbols = data_symbols();
        symbols
            .parameters
            .push(crate::registry::input("name with spaces", ArgType::Any));
        for next in [
            r#""x""#,
            r#"(x 坐标)"#,
            r#"<1 < 2>"#,
            r#"生命值"#,
            "特殊分数",
            "@seconds",
            "12",
            "-3",
            r#"(ext.读取值)"#,
            r#"(ext::"read"((1)))"#,
            r#"<"自定义拓展".有效?>"#,
        ] {
            let marked_source = format!(r#"示例(@"na|")({next})"#);
            let (source, items) = symbol_candidates(&marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}");
            assert_eq!(
                apply(&source, &items[0]),
                format!(r#"示例(@"name with spaces")({next})"#),
                "{marked_source}",
            );
        }
        let (source, items) = symbol_candidates(r#"测试(@"na|) "后续参数""#, &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(
            apply(&source, &items[0]),
            r#"测试(@"name with spaces") "后续参数""#
        );
    }

    #[test]
    fn parameters_offer_boolean_wrapping_without_duplicating_delimiters() {
        let mut symbols = data_symbols();
        symbols.parameters = vec![
            crate::registry::input("步数", ArgType::Any),
            crate::registry::input("是否继续", ArgType::Boolean),
            crate::registry::input("😀 名字", ArgType::Any),
        ];
        for (marked_source, label, expected) in [
            ("移动(@步|旧)步", "@步数", "移动(@步数)步"),
            ("说(@是|)", "<@是否继续>", "说(<@是否继续>)"),
            ("<@是|>", "<@是否继续>", "<@是否继续>"),
            ("<@是|", "<@是否继续>", "<@是否继续>"),
            ("<@步|旧 < 10>", "@步数", "<@步数 < 10>"),
            ("<@步|旧 > 10>", "@步数", "<@步数 > 10>"),
            ("<@步|旧>10>", "@步数", "<@步数>10>"),
            ("<@步|旧 > sc>", "@步数", "<@步数 > sc>"),
            ("<@步|旧\n>\n10>", "@步数", "<@步数\n>\n10>"),
            ("<@是|旧 > 10>", "<@是否继续>", "<<@是否继续> > 10>"),
            ("<@是|旧 > sc>", "<@是否继续>", "<<@是否继续> > sc>"),
            ("<<@是|旧> > 10>", "<@是否继续>", "<<@是否继续> > 10>"),
            ("<<@是|旧>>10>", "<@是否继续>", "<<@是否继续>>10>"),
            ("<@\"😀 |旧\">10>", "@\"😀 名字\"", "<@\"😀 名字\">10>"),
            ("说(@\"😀 |旧\")", "@\"😀 名字\"", "说(@\"😀 名字\")"),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            let selected = items
                .iter()
                .find(|item| item.label == label)
                .expect(marked_source);
            assert_eq!(apply(&source, selected), expected);
        }
        let (_, items) = symbol_candidates("<@|>", &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "<@是否继续>");
    }

    #[test]
    fn procedure_snippets_include_typed_slots_and_compile() {
        let procedure = crate::registry::procedure_template(
            "测试 步数: @steps 条件: <@enabled> 内容: @text",
            "example",
        )
        .unwrap();
        let mut symbols = data_symbols();
        symbols.procedures.push(procedure.clone());
        let (source, items) = symbol_candidates("测|", &symbols);
        let selected = items
            .iter()
            .find(|item| {
                item.detail
                    .as_deref()
                    .is_some_and(|d| d.starts_with("自制积木"))
            })
            .unwrap();
        let text = apply(&source, selected);
        let call = Registry::new()
            .parse(&text, Shape::Command, &symbols.procedures)
            .unwrap();
        assert_eq!(call.template.procedure, procedure.procedure);
        assert!(text.contains("条件:<>"));
        compile_source(&format!(
            "def 测试 步数: @steps 条件: <@enabled> 内容: @text :\n    等待 (@steps) 秒\nend\nstart :\n    {text}\nend"
        ));
    }

    #[test]
    fn parameter_first_procedures_complete_at_statement_and_input_positions() {
        let mut symbols = data_symbols();
        let procedure =
            crate::registry::procedure_template("@steps 步", "parameter_first").unwrap();
        symbols.procedures.push(procedure);
        let (source, items) = symbol_candidates("start:\n    |", &symbols);
        let selected = items.iter().find(|item| item.label == "(\"\")步").unwrap();
        assert_eq!(apply(&source, selected), "start:\n    (\"\")步");
        let (source, items) = symbol_candidates("start:\n    (sco|旧)步", &symbols);
        let selected = items.iter().find(|item| item.label == "score").unwrap();
        assert_eq!(apply(&source, selected), "start:\n    (score)步");
    }

    #[test]
    fn builtin_procedure_collision_never_offers_an_ambiguous_or_wrong_call() {
        let registry = Registry::new();
        let mut symbols = data_symbols();
        symbols
            .procedures
            .push(crate::registry::procedure_template("移动 @steps 步", "movement").unwrap());
        let (_, items) = symbol_candidates("移|", &symbols);
        assert!(!items.iter().any(|item| item.label.starts_with("移动(")));
        for template in registry.templates.iter().chain(symbols.procedures.iter()) {
            if let Some((label, _)) =
                completion_text_visible(template, &registry, &symbols.procedures)
            {
                let call = registry
                    .parse_typed(
                        &label,
                        template.shape,
                        &symbols.procedures,
                        &template_default_types(template),
                    )
                    .unwrap();
                assert_eq!(call.template.opcode, template.opcode);
                assert_eq!(call.template.procedure, template.procedure);
            }
        }
    }

    #[test]
    fn document_wrapper_collects_declarations_even_with_incomplete_calls() {
        let registry = Registry::new();
        let (source, cursor) = marked(
            "score = 0\ndef 等一等 @seconds :\n    等待 (@sec|) 秒\nend\nstart :\n    等一\nend",
        );
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "@seconds");
        let (source, cursor) = marked(
            "score = 0\ndef 等一等 @seconds :\n    等待 (@seconds) 秒\nend\nstart :\n    等一|\nend",
        );
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        let applied = apply(&source, &items[0]);
        compile_source(&applied);
    }
}
