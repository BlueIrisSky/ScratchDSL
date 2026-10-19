//! Tolerant, UTF-16 completion for a document which is usually not parseable yet.
//!
//! This scanner only determines safe insertion contexts. The registry remains the
//! authority for block text, argument defaults, namespaces, and target support.
use super::symbols::{VisibleSymbols, menu_end};
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
    match context(source, cursor) {
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
        } => {
            let prefix = normalize(&source[content..cursor]);
            // Once a value has been entered, replacing an entire invocation is
            // surprising. Only complete its initial fixed words in this version.
            let mut result = Vec::new();
            let mut seen = BTreeSet::new();
            for template in registry.templates.iter().chain(symbols.procedures.iter()) {
                if shape.is_some_and(|s| s != template.shape)
                    || (shape.is_none()
                        && !match template.shape {
                            Shape::Hat => script != Some(true),
                            Shape::Control => script != Some(false),
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
                let Some(end) = replacement_end(source, cursor, template.shape) else {
                    continue;
                };
                let Some((label, mut snippet)) =
                    completion_text_visible(template, registry, &symbols.procedures)
                else {
                    continue;
                };
                if !seen.insert(label.clone()) {
                    continue;
                }
                if matches!(template.shape, Shape::Hat | Shape::Control) {
                    if !source[cursor..].trim_start().starts_with('{') {
                        let indent = indentation(source, start);
                        snippet.push_str(&format!(" {{\n{indent}    $0\n{indent}\\}}"));
                    }
                } else {
                    snippet.push_str("$0");
                }
                let mut candidate = item(
                    source,
                    start,
                    end,
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

pub(crate) fn byte_offset(source: &str, position: Position) -> Option<usize> {
    let mut start = 0;
    for _ in 0..position.line {
        start += source[start..].find('\n')? + 1;
    }
    let line = source[start..].split('\n').next()?.trim_end_matches('\r');
    let mut units = 0;
    for (offset, c) in line.char_indices() {
        if units == position.character {
            return Some(start + offset);
        }
        units += c.len_utf16() as u32;
        if units > position.character {
            return None;
        }
    }
    (units == position.character).then_some(start + line.len())
}

fn position_at(source: &str, byte: usize) -> Position {
    let prefix = &source[..byte];
    Position::new(
        prefix.bytes().filter(|&b| b == b'\n').count() as u32,
        prefix
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .count() as u32,
    )
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
        ArgType::Variable | ArgType::List => {
            let name = slot.default.as_str().filter(|s| !s.is_empty()).unwrap_or(
                if slot.arg_type == ArgType::List {
                    "列表"
                } else {
                    "变量"
                },
            );
            let name = if crate::parser::parse_name(name).is_ok() {
                name.to_owned()
            } else {
                quote(name)
            };
            format!(
                "${}{name}",
                if slot.arg_type == ArgType::List {
                    "list:"
                } else {
                    ""
                }
            )
        }
        ArgType::Color => quote(slot.default.as_str().unwrap_or("#000000")),
        _ => match &slot.default {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => quote(s),
            serde_json::Value::Bool(b) => quote(if *b { "true" } else { "false" }),
            _ => quote(""),
        },
    }
}

fn block_text(template: &Template) -> (String, String) {
    let prefix = template
        .namespace
        .as_ref()
        .map(|ns| format!("{}.", namespace_text(ns)))
        .unwrap_or_default();
    let (open, close) = match template.shape {
        Shape::Command => ("[", "]"),
        Shape::Reporter => ("(", ")"),
        Shape::Boolean => ("<", ">"),
        _ => ("", ""),
    };
    let mut label = format!("{prefix}{open}");
    let mut snippet = escape_snippet(&label);
    let mut index = 1;
    for token in &template.tokens {
        match token {
            Token::Literal(text) => {
                label.push_str(text);
                snippet.push_str(&escape_snippet(text));
            }
            Token::Slot(slot) => {
                let default = slot_default(slot);
                label.push_str(&default);
                snippet.push_str(&format!("${{{index}:{}}}", escape_snippet(&default)));
                index += 1;
            }
        }
    }
    label.push_str(close);
    snippet.push_str(close);
    (label, snippet)
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

struct Reference {
    start: usize,
    end: usize,
    name: usize,
    kind: ReferenceKind,
    global: bool,
    list: bool,
    quoted: bool,
    boolean: bool,
}

/// A reference is scanned separately from ordinary strings so that `$"a b"`
/// can be completed without enabling suggestions in a block's text argument.
fn reference_at(
    source: &str,
    start: usize,
    cursor: usize,
    boolean_open: Option<usize>,
    expressions: &[Expression],
) -> Option<Reference> {
    let kind = if source[start..].starts_with('$') {
        ReferenceKind::Data
    } else {
        ReferenceKind::Parameter
    };
    let mut name = start + 1;
    let mut global = false;
    let mut list = false;
    if kind == ReferenceKind::Data {
        if source[name..cursor].starts_with("global:") {
            global = true;
            name += "global:".len();
        }
        if source[name..cursor].starts_with("list:") {
            list = true;
            name += "list:".len();
        }
    }
    let quoted = source[name..].starts_with('"');
    if quoted && source[name..cursor].contains(['\n', '\r']) {
        return None;
    }
    let end = if quoted {
        let mut escaped = false;
        let mut close = None;
        for (offset, c) in source[name + 1..].char_indices() {
            if matches!(c, '\n' | '\r') {
                break;
            }
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                close = Some(name + 1 + offset + 1);
                break;
            }
        }
        if let Some(end) = close.filter(|&end| {
            let next = source[end..].chars().next();
            let boundary = next.is_none_or(|c| c.is_whitespace() || "]>)},#<+-=*/".contains(c));
            // Delimited values need no separating space. Ask the lexer about
            // the next atom, including quoted/qualified values and numbers.
            // A quote after trailing whitespace followed by bare text is more
            // likely the opening quote of the next argument in unfinished code.
            boundary
                || crate::parser::scan_atom(source, end).is_ok()
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
        list,
        quoted,
        boolean,
    })
}

fn reference_completions(
    source: &str,
    cursor: usize,
    reference: &Reference,
    symbols: &VisibleSymbols,
) -> Vec<CompletionItem> {
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
    for (is_list, symbols) in [(false, &symbols.variables), (true, &symbols.lists)] {
        if (reference.list && !is_list) || (reference.global && !reference.list && is_list) {
            continue;
        }
        let mut visible = BTreeMap::new();
        for symbol in symbols {
            if (reference.global && !symbol.global) || !symbol.name.starts_with(&partial) {
                continue;
            }
            let existing = visible.get(&symbol.name);
            if existing.is_none_or(|previous: &&super::symbols::DataSymbol| {
                previous.global && !symbol.global
            }) {
                visible.insert(&symbol.name, symbol);
            }
        }
        for symbol in visible.into_values() {
            let name = name_text(&symbol.name, reference.quoted);
            let label = format!(
                "${}{}{name}",
                if reference.global { "global:" } else { "" },
                if is_list { "list:" } else { "" },
            );
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
                    if is_list { "列表" } else { "变量" },
                ),
            );
            candidate.filter_text = Some(format!("{entered_prefix}{name}"));
            result.push(candidate);
        }
    }
    if !reference.list && !reference.quoted {
        for (prefix, detail) in [("list:", "列表引用"), ("global:", "全局数据引用")] {
            if (prefix == "global:" && reference.global) || !prefix.starts_with(&partial) {
                continue;
            }
            let label = format!("{entered_prefix}{prefix}");
            let end = reference.end + usize::from(source[reference.end..].starts_with(':'));
            let mut candidate = item(
                source,
                reference.start,
                end,
                label.clone(),
                escape_snippet(&label),
                CompletionItemKind::KEYWORD,
                detail.into(),
            );
            candidate.command = Some(lsp_types::Command {
                title: "补全数据名称".into(),
                command: "editor.action.triggerSuggest".into(),
                arguments: None,
            });
            result.push(candidate);
        }
    }
    result
}

/// A valid schema can still have display text ambiguous with another template.
/// Never offer a default snippet that invokes a different block or fails to
/// parse. Extensions have the language's explicit-opcode escape hatch; builtin
/// ambiguities are omitted because inventing a builtin namespace changes DSL.
#[cfg(test)]
fn completion_text(template: &Template, registry: &Registry) -> Option<(String, String)> {
    completion_text_visible(template, registry, &[])
}

fn completion_text_visible(
    template: &Template,
    registry: &Registry,
    visible: &[Template],
) -> Option<(String, String)> {
    let resolves_to_template = |text: &str| {
        registry
            .parse(text, template.shape, visible)
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
    let (open, close) = match template.shape {
        Shape::Command => ("[", "]"),
        Shape::Reporter => ("(", ")"),
        Shape::Boolean => ("<", ">"),
        Shape::Hat => ("hat(", ")"),
        Shape::Control => return None,
    };
    let mut label = format!("{}::{}.{open}", namespace_text(namespace), quote(opcode));
    let mut snippet = escape_snippet(&label);
    for (index, slot) in template.slots().enumerate() {
        if index > 0 {
            label.push_str(", ");
            snippet.push_str(", ");
        }
        let default = slot_default(slot);
        label.push_str(&default);
        snippet.push_str(&format!("${{{}:{}}}", index + 1, escape_snippet(&default)));
    }
    label.push_str(close);
    snippet.push_str(close);
    resolves_to_template(&label).then_some((label, snippet))
}

fn replacement_end(source: &str, cursor: usize, shape: Shape) -> Option<usize> {
    let rest = &source[cursor..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let close = match shape {
        Shape::Command => ']',
        Shape::Reporter => ')',
        Shape::Boolean => '>',
        _ => {
            return (trimmed.is_empty() || trimmed.starts_with(['{', '}', '\n', '\r']))
                .then_some(cursor);
        }
    };
    if trimmed.starts_with(close) {
        Some(cursor + rest.len() - trimmed.len() + 1)
    } else if trimmed.is_empty() || trimmed.starts_with(['\n', '\r', '}', ']', ')', '>']) {
        Some(cursor)
    } else {
        None
    }
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
    },
}

struct Expression {
    open: char,
    start: usize,
    content: usize,
    namespace: Option<String>,
    explicit: bool,
}

/// Returns the end of a string, or EOF for an unfinished string. Escapes are
/// skipped here, not validated: JSON and ordinary DSL strings differ.
fn string_end(source: &str, start: usize) -> usize {
    let mut escaped = false;
    for (offset, c) in source[start + 1..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return start + 1 + offset + 1;
        }
    }
    source.len()
}

fn comment_end(source: &str, start: usize) -> usize {
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |p| start + p);
    if let Some(end) = source[start + 1..line_end].find('#') {
        return start + 1 + end + 1;
    }
    let line_start = source[..start].rfind('\n').map_or(0, |p| p + 1);
    if source[line_start..start].trim().is_empty() && source[start + 1..line_end].trim().is_empty()
    {
        source[line_end..]
            .find('#')
            .map_or(source.len(), |p| line_end + p + 1)
    } else {
        line_end
    }
}

/// Find a directly adjacent namespace. In particular, a quoted namespace is
/// one atom, even when its filename contains spaces or punctuation.
fn namespace_before(source: &str, open: usize) -> Option<(usize, String)> {
    let text = source[..open].strip_suffix('.')?;
    namespace_name_before(text)
}

fn namespace_name_before(text: &str) -> Option<(usize, String)> {
    if text.ends_with('"') {
        for (start, c) in text.char_indices().rev() {
            if c == '"'
                && !text[..start].ends_with(':')
                && let Ok(name) = crate::parser::parse_string(&text[start..])
            {
                return Some((start, name));
            }
        }
    } else {
        let start = text
            .char_indices()
            .rev()
            .find(|(_, c)| !crate::parser::namespace_continue(*c))
            .map_or(0, |(p, c)| p + c.len_utf8());
        // References use a narrower name grammar than namespaces. In
        // `$score-ext.(数字)` the minus follows the variable, even if an
        // extension named `score-ext` also exists. Let the parser locate the
        // complete reference, including `$global:list:` prefixes.
        if text[..start].ends_with(['$', '@', ':'])
            && let Some(reference) = text[..start].rfind(['$', '@'])
            && let Ok(end) = crate::parser::scan_atom(text, reference)
            && end >= start
            && let Some(namespace) = text[end..].strip_prefix('-')
            && crate::parser::is_bare_namespace(namespace)
        {
            return Some((end + 1, namespace.into()));
        }
        let name = &text[start..];
        if crate::parser::is_bare_namespace(name) {
            return Some((start, name.into()));
        }
        // A numeric left operand can directly precede a minus, as in
        // `(1-ext.(数字))`. Preserve it when replacing the namespace call.
        for (offset, c) in name.char_indices() {
            if c == '-' && crate::parser::is_bare_namespace(&name[offset + 1..]) {
                return Some((start + offset + 1, name[offset + 1..].into()));
            }
        }
    }
    None
}

fn explicit_namespace_before(source: &str, open: usize) -> Option<(usize, String)> {
    let text = if source[open..].starts_with('(') {
        source[..open]
            .strip_suffix(".hat")
            .or_else(|| source[..open].strip_suffix('.'))?
    } else {
        source[..open].strip_suffix('.')?
    };
    if !text.ends_with('"') {
        return None;
    }
    for (opcode_start, c) in text.char_indices().rev() {
        if c == '"'
            && crate::parser::parse_string(&text[opcode_start..]).is_ok()
            && let Some(namespace) = text[..opcode_start].strip_suffix("::")
        {
            return namespace_name_before(namespace);
        }
    }
    None
}

fn declaration_header(header: &str) -> bool {
    header.starts_with('@')
        || header.starts_with("定义自制积木")
        || header
            .strip_prefix("import")
            .is_some_and(|rest| rest.chars().next().is_none_or(char::is_whitespace))
}

fn context(source: &str, cursor: usize) -> Context {
    let mut pos = 0;
    let mut statement = 0;
    let mut expressions: Vec<Expression> = Vec::new();
    // Each brace retains the enclosing target, while identifying script bodies.
    let mut scopes: Vec<(Option<Target>, bool)> = Vec::new();
    while pos < cursor {
        let c = source[pos..].chars().next().unwrap();
        if c == '#' {
            let end = comment_end(source, pos);
            if cursor < end || (cursor == end && !(end > pos + 1 && source[..end].ends_with('#'))) {
                return Context::Blocked;
            }
            if expressions.is_empty() && source[pos..end].contains('\n') {
                statement = end;
            }
            pos = end;
            continue;
        }
        if matches!(c, '$' | '@') {
            let header = source[statement..pos].trim_start();
            // Hat arguments occur before a script's opening brace. Data
            // references are also valid there, while declarations aren't.
            let in_value =
                c == '$' || !expressions.is_empty() || scopes.last().is_some_and(|scope| scope.1);
            if !declaration_header(header) && in_value {
                let boolean_open = expressions
                    .last()
                    .filter(|expression| {
                        expression.open == '<'
                            && !expression.explicit
                            && source[expression.content..pos].trim().is_empty()
                    })
                    .map(|expression| expression.content - 1);
                if let Some(reference) =
                    reference_at(source, pos, cursor, boolean_open, &expressions)
                {
                    return Context::Reference(reference);
                }
            }
        }
        if c == '"' {
            let end = string_end(source, pos);
            if cursor < end || (cursor == end && !source[pos..end].ends_with('"')) {
                return Context::Blocked;
            }
            pos = end;
            continue;
        }
        if c == '/' {
            if !source[pos..].starts_with("/'")
                && let Some(end) = expression_operator(source, pos, &expressions)
            {
                pos = end;
                continue;
            }
            let (end, closed) = menu_end(source, pos);
            if cursor < end || (cursor == end && !closed) {
                return Context::Blocked;
            }
            pos = end;
            continue;
        }
        if matches!(c, '<' | '>')
            && let Some(end) = expression_operator(source, pos, &expressions)
        {
            pos = end;
            continue;
        }
        match c {
            '[' | '(' | '<' => {
                let explicit = explicit_namespace_before(source, pos);
                let qualified = explicit.clone().or_else(|| namespace_before(source, pos));
                if source[..pos].ends_with('.') && qualified.is_none() {
                    return Context::Blocked;
                }
                expressions.push(Expression {
                    open: c,
                    start: qualified.as_ref().map_or(pos, |(start, _)| *start),
                    content: pos + 1,
                    namespace: qualified.map(|(_, ns)| ns),
                    explicit: explicit.is_some(),
                });
            }
            ']' | ')' | '>' => {
                let expected = match c {
                    ']' => '[',
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
                let header = source[statement..pos].trim();
                if header == "@init" {
                    let end = json_region_end(source, pos);
                    if cursor < end || (cursor == end && !source[pos..end].ends_with('}')) {
                        return Context::Init { start: pos };
                    }
                    pos = end;
                    statement = pos;
                    continue;
                }
                let inherited = scopes.last().and_then(|s| s.0);
                let (target, script) = if header.starts_with("@sprite ") {
                    (Some(Target::Sprite), false)
                } else if header == "@stage" {
                    (Some(Target::Stage), false)
                } else {
                    (inherited, true)
                };
                scopes.push((target, script));
                statement = pos + 1;
            }
            '}' if expressions.is_empty() => {
                scopes.pop();
                statement = pos + 1;
            }
            '\n' if expressions.is_empty() => {
                statement = pos + 1;
            }
            _ => {}
        }
        pos += c.len_utf8();
    }
    let target = scopes.last().and_then(|s| s.0);
    let header = source[statement..cursor].trim_start();
    if declaration_header(header) {
        return Context::Blocked;
    }
    if let Some(e) = expressions.last() {
        if e.explicit {
            return Context::Blocked;
        }
        return Context::Blocks {
            start: e.start,
            content: e.content,
            shape: Some(match e.open {
                '[' => Shape::Command,
                '(' => Shape::Reporter,
                _ => Shape::Boolean,
            }),
            namespace: e.namespace.clone(),
            target,
            script: None,
        };
    }
    let start =
        statement + source[statement..cursor].len() - source[statement..cursor].trim_start().len();
    let text = &source[start..cursor];
    if text.starts_with('@') {
        return Context::Blocked;
    }
    let mut content = start;
    let mut namespace = None;
    // A header may also have a namespace but has no wrapping brackets.
    for (p, c) in text.char_indices() {
        if c == '.'
            && let Some((ns_start, ns)) = namespace_before(source, start + p + 1)
            && ns_start == start
        {
            content = start + p + 1;
            namespace = Some(ns);
            break;
        }
    }
    Context::Blocks {
        start,
        content,
        shape: None,
        namespace,
        target,
        script: scopes
            .last()
            .map(|s| s.1)
            .or_else(|| source.contains("@sprite").then_some(false)),
    }
}

fn expression_operator(source: &str, position: usize, expressions: &[Expression]) -> Option<usize> {
    let expression = expressions.last()?;
    if expression.explicit || !matches!(expression.open, '(' | '<') {
        return None;
    }
    let groups: Vec<_> = expressions
        .iter()
        .map(|expression| {
            (
                match expression.open {
                    '(' => ')',
                    '[' => ']',
                    _ => '>',
                },
                expression.content - 1,
            )
        })
        .collect();
    super::symbols::operator_end(source, position, &groups)
}

fn json_region_end(source: &str, start: usize) -> usize {
    let mut depth = 0;
    let mut pos = start;
    while pos < source.len() {
        match source.as_bytes()[pos] {
            b'"' => {
                pos = string_end(source, pos);
                continue;
            }
            b'#' => {
                pos = comment_end(source, pos);
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return pos + 1;
                }
            }
            _ => {}
        }
        pos += source[pos..].chars().next().unwrap().len_utf8();
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

/// Complete the same data notation used by build files and sprite @init.
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
            let line_end = source[pos..]
                .find(['\r', '\n'])
                .map_or(source.len(), |offset| pos + offset);
            let end = string_end(source, pos).min(line_end);
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
    fn incomplete_chinese_command_applies_and_compiles() {
        let source = choose(
            "@sprite \"玩家\" {\n @init {}\n 当绿旗被点击 {\n  [移|]\n }\n}",
            "motion_movesteps",
        );
        assert!(source.contains("[移动 0 步]"));
        assert!(!source.contains("]]"));
        compile_source(&source);
        assert_eq!(choose("[移动|", "motion_movesteps"), "[移动 0 步]");
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
        assert_eq!(choose("[说 (x|)]", "motion_xposition"), "[说 (x 坐标)]");
    }

    #[test]
    fn every_offered_builtin_default_snippet_resolves_to_its_template() {
        let registry = Registry::new();
        let mut omitted = BTreeSet::new();
        for template in &registry.templates {
            let Some((_, snippet)) = completion_text(template, &registry) else {
                omitted.insert(template.opcode.as_str());
                continue;
            };
            let text = expand(&snippet);
            let call = registry
                .parse(&text, template.shape, &[])
                .unwrap_or_else(|error| panic!("{}: {text}: {error}", template.opcode));
            assert_eq!(call.template.opcode, template.opcode);
        }
        // Lists are also general values: this particular display text matches
        // both list membership and the ordinary string-contains operator.
        assert_eq!(omitted, BTreeSet::from(["data_listcontainsitem"]));
        let (source, cursor) = marked("<|");
        let items = complete(&source, cursor, &registry);
        assert!(!items.iter().any(|c| {
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
    fn hats_controls_and_existing_body() {
        let source = choose(
            "@sprite \"玩家\" {\n @init {}\n 当绿|\n}",
            "event_whenflagclicked",
        );
        compile_source(&source);
        let source = choose(
            "@sprite \"玩家\" {\n @init {}\n 当绿旗被点击 {\n  如果|\n }\n}",
            "control_if",
        );
        assert!(source.contains("如果 <> 那么 {"));
        compile_source(&source);
        let source = choose("当绿| { [等待 1 秒] }", "event_whenflagclicked");
        assert_eq!(source, "当绿旗被点击 { [等待 1 秒] }");
        compile_source(&source);
    }

    #[test]
    fn utf16_ranges_preserve_emoji_and_chinese_before_the_edit() {
        let (source, cursor) = marked("# 😀中文 # [移|]");
        assert_eq!(cursor.character, 11);
        let candidates = complete(&source, cursor, &Registry::new());
        let selected = candidates
            .iter()
            .find(|c| c.label == "[移动 0 步]")
            .unwrap();
        let Some(CompletionTextEdit::Edit(edit)) = &selected.text_edit else {
            panic!();
        };
        assert_eq!(edit.range.start.character, 9);
        assert_eq!(apply(&source, selected), "# 😀中文 # [移动 0 步]");
        assert!(byte_offset("😀", Position::new(0, 1)).is_none());
        assert_eq!(byte_offset("😀\r\n中", Position::new(1, 1)), Some(9));
    }

    #[test]
    fn comments_strings_and_menu_contents_do_not_offer_blocks() {
        for source in [
            "# 注释 [移|",
            "#\n块注释\n[移|\n#",
            "# 行内 [移| #",
            "[说 \"[移|\"]",
            "[移到 /'[移|'/]",
            "[移到 /'a/# [移|'/]",
            r"[移到 /'a\'/# [移|'/]",
            r"[移到 /'a\\/# [移|'/]",
            "[移到 /'unfinished/# [移|",
            "[说 \"# [移|\"]",
        ] {
            let (source, cursor) = marked(source);
            assert!(
                complete(&source, cursor, &Registry::new()).is_empty(),
                "{source}"
            );
        }
        assert_eq!(
            choose("# 注释\n[移|]", "motion_movesteps"),
            "# 注释\n[移动 0 步]"
        );
        assert_eq!(
            choose("#\n块\n#\n[移|]", "motion_movesteps"),
            "#\n块\n#\n[移动 0 步]"
        );
    }

    #[test]
    fn division_comparison_and_star_menus_keep_expression_context() {
        assert_eq!(choose("(8 / (x|))", "motion_xposition"), "(8 / (x 坐标))");
        assert_eq!(choose("<1 < (x|)>", "motion_xposition"), "<1 < (x 坐标)>");
        assert_eq!(choose("<1 > (x|)>", "motion_xposition"), "<1 > (x 坐标)>");
        assert_eq!(
            choose("[移到 /'*'/]\n[移|]", "motion_movesteps"),
            "[移到 /'*'/]\n[移动 0 步]"
        );
        assert_eq!(
            choose("[移到 /''/]\n[移|]", "motion_movesteps"),
            "[移到 /''/]\n[移动 0 步]"
        );
        assert_eq!(
            choose("[移到 /'unfinished/#\n[移|]", "motion_movesteps"),
            "[移到 /'unfinished/#\n[移动 0 步]"
        );
    }

    #[test]
    fn infix_right_operands_complete_with_spacing_nesting_and_utf16() {
        let symbols = data_symbols();
        for (marked_source, expected) in [
            ("($score / $sc|旧)", "($score / $score)"),
            ("($score/$sc|旧)", "($score/$score)"),
            ("($score\n/\n$sc|旧)", "($score\n/\n$score)"),
            ("(($score + 1)/$sc|旧)", "(($score + 1)/$score)"),
            ("($score + $sc|旧)", "($score + $score)"),
            ("($score-$sc|旧)", "($score-$score)"),
            ("($score*$sc|旧)", "($score*$score)"),
            ("($score mod $sc|旧)", "($score mod $score)"),
            ("<$score > $sc|旧>", "<$score > $score>"),
            ("<$score>$sc|旧>", "<$score>$score>"),
            ("<$score\n>\n$sc|旧>", "<$score\n>\n$score>"),
            ("<$score=$sc|旧>", "<$score=$score>"),
            ("<$score<$sc|旧>", "<$score<$score>"),
            (
                "# 😀 # <$score>$\"😀 |旧\">",
                "# 😀 # <$score>$\"😀 分数\">",
            ),
            (
                "# 😀 # ($score/$\"😀 |旧\")",
                "# 😀 # ($score/$\"😀 分数\")",
            ),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}: {items:?}");
            assert_eq!(apply(&source, &items[0]), expected, "{marked_source}");
        }
        for marked_source in ["<$score > $|>", "($score / $|)", "<$score>$|"] {
            let (_, items) = symbol_candidates(marked_source, &symbols);
            assert!(
                items.iter().any(|item| item.label == "$score"),
                "{marked_source}"
            );
        }
    }

    #[test]
    fn nested_boolean_slots_do_not_consume_a_later_template_argument() {
        let symbols = data_symbols();
        for prior in [
            "ext.<模板 <@ready> 1>",
            "ext.<模板 <@ready>\n1>",
            "ext.<模板 <true> 1>",
            "ext.<模板 <$score>1> 2>",
            "ext.<模板 ext.<模板 <@ready> 1> 2>",
            r#"ext::"check".<<@ready>, 1>"#,
        ] {
            let marked = format!("[说 {prior}]\n[说 $sc|旧]");
            let (source, items) = symbol_candidates(&marked, &symbols);
            assert_eq!(items.len(), 1, "{prior}: {items:?}");
            assert_eq!(
                apply(&source, &items[0]),
                format!("[说 {prior}]\n[说 $score]")
            );
        }
    }

    #[test]
    fn init_keys_enums_and_existing_quotes() {
        let registry = Registry::new();
        for marked_source in [
            "@sprite \"玩家\" { @init { | } }",
            "@sprite \"玩家\" { @init { \"rot|\" } }",
        ] {
            let (source, cursor) = marked(marked_source);
            let items = complete(&source, cursor, &registry);
            let selected = items.iter().find(|i| i.label == "rotationStyle").unwrap();
            compile_source(&apply(&source, selected));
        }
        let (source, cursor) = marked("@sprite \"玩家\" { @init { \"rotationStyle\": \"le|\" } }");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "left-right");
        compile_source(&apply(&source, &items[0]));
        let (source, cursor) = marked("@sprite \"玩家\" { @init { \"visible\": f| } }");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items[0].label, "false");
        compile_source(&apply(&source, &items[0]));
    }

    #[test]
    fn init_nested_assets_and_duplicate_fields_are_contextual() {
        let registry = Registry::new();
        let (source, cursor) = marked("@sprite \"玩家\" { @init { \"x\": 1, | } }");
        let items = complete(&source, cursor, &registry);
        assert!(!items.iter().any(|i| i.label == "x"));
        assert!(items.iter().any(|i| i.label == "y"));
        let (source, cursor) = marked("@sprite \"玩家\" { @init { \"sounds\": [{ | }] } }");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i.label == "path"));
        assert!(!items.iter().any(|i| i.label == "rotationCenterX"));
        for source in [
            "@sprite \"玩家\" { @init { # 注释 |\n } }",
            "@sprite \"玩家\" { @init { \"costumes\": [{ \"path\": \"移|\" }] } }",
        ] {
            let (source, cursor) = marked(source);
            assert!(complete(&source, cursor, &registry).is_empty());
        }
    }

    #[test]
    fn bare_init_fields_complete_after_newlines_without_adding_quotes() {
        for marked_source in [
            "@sprite \"玩家\" { @init { rot| } }",
            "@sprite \"玩家\" { @init { x: 1\n rot| } }",
            "@sprite \"玩家\" { @init { x: 1 # 坐标\n rot| } }",
            "@sprite \"玩家\" { @init { x: 1\n #\n 注释\n #\n rot| } }",
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
        let (source, cursor) = marked("@sprite \"玩家\" { @init { x: 1\n | } }");
        let items = complete(&source, cursor, &Registry::new());
        assert!(!items.iter().any(|item| item.label == "x"));
        assert!(items.iter().any(|item| item.label == "y"));
        let (source, cursor) =
            marked("@sprite \"玩家\" { @init { sounds: [{ name: \"音效\"\n pa| }] } }");
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
        });
        let (source, cursor) = marked("\"自定义 拓展\".[展|]");
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        let applied = apply(&source, &items[0]);
        assert_eq!(applied, "\"自定义 拓展\".[展示 \"$价格}\\\\路径\"]");
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
            });
            let (source, cursor) = marked("自定义拓展.[选|]");
            let items = complete(&source, cursor, &registry);
            assert_eq!(items.len(), 1, "{value:?}");
            let applied = apply(&source, &items[0]);
            let expected = crate::parser::format_menu(value);
            assert_eq!(applied, format!("自定义拓展.[选择 {expected}]"));
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
                });
            }
            for prefix in [namespace.to_owned(), quote(namespace)] {
                let (source, cursor) = marked(&format!("{prefix}.[展|]"));
                let items = complete(&source, cursor, &registry);
                assert_eq!(items.len(), 1, "{source}: {items:?}");
                let applied = apply(&source, &items[0]);
                assert_eq!(applied, format!("{namespace}.[展示]"));
                registry.parse(&applied, Shape::Command, &[]).unwrap();
            }
            for number in ["1", "1.2", "-1", "1e-2"] {
                let (source, cursor) = marked(&format!("({number}-{namespace}.(数|))"));
                let items = complete(&source, cursor, &registry);
                assert_eq!(items.len(), 1, "{source}: {items:?}");
                let applied = apply(&source, &items[0]);
                assert_eq!(applied, format!("({number}-{namespace}.(数字))"));
                registry.parse(&applied, Shape::Reporter, &[]).unwrap();
            }
        }
        assert!(namespace_name_before("1extension").is_none());
        assert!(namespace_name_before("1.extension").is_none());
    }

    #[test]
    fn extension_completion_preserves_reference_operands_before_minus() {
        for (reference, name) in [
            ("$score", "score"),
            ("@value", "value"),
            ("$global:score", "score"),
            ("$list:items", "items"),
            ("$global:list:items", "items"),
            ("$变量", "变量"),
            ("@参数", "参数"),
            ("$\"变量 with spaces\"", "变量 with spaces"),
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
                    });
                }
                let (source, cursor) = marked(&format!("({reference}-{namespace}.(数|))"));
                let items = complete(&source, cursor, &registry);
                assert_eq!(items.len(), 1, "{source}: {items:?}");
                assert_eq!(items[0].label, format!("{namespace}.(数字)"));
                let applied = apply(&source, &items[0]);
                assert_eq!(applied, format!("({reference}-{namespace}.(数字))"));
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
            });
        }
        let (source, cursor) = marked("\"自定义拓展\".[展|]");
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

    #[test]
    fn explicit_targets_filter_only_when_known() {
        let registry = Registry::new();
        let (source, cursor) = marked("@stage { 当绿旗被点击 { [移|] } }");
        assert!(complete(&source, cursor, &registry).is_empty());
        let (source, cursor) = marked("@sprite \"玩家\" { @init {}\n 当| }");
        let items = complete(&source, cursor, &registry);
        assert!(items.iter().any(|i| i.label == "当角色被点击"));
        assert!(!items.iter().any(|i| i.label == "当舞台被点击"));
        let (source, cursor) = marked("[移|");
        assert!(!complete(&source, cursor, &registry).is_empty());
    }

    #[test]
    fn existing_arguments_are_not_replaced() {
        let registry = Registry::new();
        for marked_source in ["[移动 12|]", "[说 \"保留\" |]", "[移|动 12 步]"] {
            let (source, cursor) = marked(marked_source);
            assert!(complete(&source, cursor, &registry).is_empty());
        }
        for marked_source in [
            "@sprite \"玩家\" { @init { |\"x\": 1 } }",
            "@sprite \"玩家\" { @init { \"visible\": |true } }",
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
                    name: "😀 分数".into(),
                    global: true,
                },
                DataSymbol {
                    name: "a\"b\\c$}".into(),
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
        }
    }

    #[test]
    fn data_completion_filters_scope_kind_and_preserves_arguments() {
        let symbols = data_symbols();
        let (source, items) = symbol_candidates("[将 $sc|ore 设为 12]", &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "$score");
        assert_eq!(items[0].detail.as_deref(), Some("角色变量"));
        assert_eq!(apply(&source, &items[0]), "[将 $score 设为 12]");
        let (_, items) = symbol_candidates("[说 $|]", &symbols);
        for label in ["$score", "$生命值", "$list:logs", "$list:", "$global:"] {
            assert!(items.iter().any(|item| item.label == label), "{label}");
        }
        let (_, items) = symbol_candidates("[说 $list:|]", &symbols);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item.label.starts_with("$list:")));
        let (_, items) = symbol_candidates("[说 $global:|]", &symbols);
        assert!(items.iter().any(|item| item.label == "$global:list:"));
        assert!(items.iter().any(|item| item.label == "$global:score"));
        assert!(
            !items
                .iter()
                .any(|item| item.label.contains("生命") || item.label.contains("logs"))
        );
        let (source, items) = symbol_candidates("[说 $global:list:lo|gs]", &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].detail.as_deref(), Some("全局列表"));
        assert_eq!(apply(&source, &items[0]), "[说 $global:list:logs]");
        let (_, items) = symbol_candidates("[说 $lo|]", &symbols);
        assert_eq!(items[0].label, "$list:logs");
        assert_eq!(items[0].filter_text.as_deref(), Some("$logs"));
        let (source, items) = symbol_candidates("[说 $glob|al:score]", &symbols);
        let prefix = items.iter().find(|item| item.label == "$global:").unwrap();
        assert_eq!(apply(&source, prefix), "[说 $global:score]");
    }

    #[test]
    fn reference_completion_replaces_quoted_suffix_with_utf16_positions() {
        let symbols = data_symbols();
        for marked_source in ["[说 $\"😀 |旧后缀\"]", "[说 $\"😀 |]", "[说 $\"😀 |\"]"]
        {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}");
            assert_eq!(apply(&source, &items[0]), "[说 $\"😀 分数\"]");
        }
        let (source, items) = symbol_candidates("# 😀 # [说 $生|命旧]", &symbols);
        assert_eq!(apply(&source, &items[0]), "# 😀 # [说 $生命值]");
        let (source, items) = symbol_candidates("<$\"sc|旧\"<1>", &symbols);
        assert_eq!(apply(&source, &items[0]), "<$\"score\"<1>");
        let (source, items) = symbol_candidates(r#"[说 $"a\"b|old"]"#, &symbols);
        assert_eq!(items.len(), 1);
        let applied = apply(&source, &items[0]);
        assert_eq!(applied, r#"[说 $"a\"b\\c$}"]"#);
        Registry::new()
            .parse(&applied, Shape::Command, &[])
            .unwrap();
        let (source, items) = symbol_candidates(r#"[说 $"a\"|old"]"#, &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(apply(&source, &items[0]), r#"[说 $"a\"b\\c$}"]"#);
        let (source, items) = symbol_candidates("[测试 $\"sc| \"后续参数\"]", &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(apply(&source, &items[0]), "[测试 $\"score\" \"后续参数\"]");
    }

    #[test]
    fn data_completion_works_in_nested_values_but_not_literals_or_declarations() {
        let symbols = data_symbols();
        for (marked_source, expected) in [
            ("[移动 ($sc|ore + 1) 步]", "[移动 ($score + 1) 步]"),
            ("<8 > ($sc|ore / 2)>", "<8 > ($score / 2)>"),
            (
                "@sprite \"玩家\" { @init {}\n 当绿旗被点击 { 如果 <$sc|ore < 2> 那么 {} } }",
                "@sprite \"玩家\" { @init {}\n 当绿旗被点击 { 如果 <$score < 2> 那么 {} } }",
            ),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}");
            assert_eq!(apply(&source, &items[0]), expected);
        }
        for marked_source in [
            "[说 \"$sc|\"]",
            "[移到 /'$sc|'/]",
            "[说 (8 /'$sc|'/)]",
            "[说 (8 /'\"$sc|\"'/)]",
            r"[移到 /'a\'/$sc|'/]",
            r"[移到 /'a\\/$sc|'/]",
            "[移到 /'unfinished/$sc|",
            "# 注释 $sc|",
            "#\n $sc|\n#",
            "@sprite \"玩家\" { @init { \"x\": \"$sc|\" } }",
            "@list names = [$sc|]",
            "定义自制积木 $sc| {}",
            "定义自制积木 示例 <@sc|> {}",
            "import helpers.$sc|",
        ] {
            let (_, items) = symbol_candidates(marked_source, &symbols);
            assert!(items.is_empty(), "{marked_source}");
        }
    }

    #[test]
    fn data_references_complete_in_hat_arguments_before_script_bodies() {
        let symbols = data_symbols();
        for (marked_source, expected) in [
            ("ext.当数值 $sc|ore 达标 {}", "ext.当数值 $score 达标 {}"),
            (
                "@sprite \"玩家\" { @init {}\n ext.当数值 $sc|ore 达标 {} }",
                "@sprite \"玩家\" { @init {}\n ext.当数值 $score 达标 {} }",
            ),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
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
            (r#"ext::"send".[$sc|]"#, r#"ext::"send".[$score]"#),
            (r#"ext::"read".($sc|, 1)"#, r#"ext::"read".($score, 1)"#),
            (r#"ext::"test".<$sc|, 1>"#, r#"ext::"test".<$score, 1>"#),
            (
                r#"ext::"event".hat($sc|) {}"#,
                r#"ext::"event".hat($score) {}"#,
            ),
            (
                r#""自定义拓展"::"send".[$sc|, "保留"]"#,
                r#""自定义拓展"::"send".[$score, "保留"]"#,
            ),
            (r#"ext::"send".[@sec|]"#, r#"ext::"send".[@seconds]"#),
            (r#"ext::"read".(@sec|)"#, r#"ext::"read".(@seconds)"#),
            (r#"ext::"test".<@sec|>"#, r#"ext::"test".<@seconds>"#),
            (
                r#"ext::"event".hat(@sec|) {}"#,
                r#"ext::"event".hat(@seconds) {}"#,
            ),
            (r#"ext::"test".<@en|>"#, r#"ext::"test".<<@enabled>>"#),
            (
                r#"ext::"send".[(ext::"read".($sc|) + 1)]"#,
                r#"ext::"send".[(ext::"read".($score) + 1)]"#,
            ),
        ] {
            let (source, items) = symbol_candidates(marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}");
            assert_eq!(apply(&source, &items[0]), expected, "{marked_source}");
        }
        for marked_source in [
            r#"ext::"send".[|]"#,
            r#"ext::"read".(|)"#,
            r#"ext::"test".<|>"#,
            r#"ext::"event".hat(|) {}"#,
        ] {
            let (_, items) = symbol_candidates(marked_source, &symbols);
            assert!(items.is_empty(), "{marked_source}");
        }
        let (source, cursor) = marked(r#"ext::"read".((x|))"#);
        let items = complete_with_symbols(&source, cursor, &Registry::new(), &symbols);
        assert!(items.iter().any(|item| item.label == "(x 坐标)"));
    }

    #[test]
    fn quoted_reference_edits_preserve_adjacent_atoms() {
        let symbols = data_symbols();
        for next in [
            r#""x""#,
            r#"(x 坐标)"#,
            r#"<1 < 2>"#,
            r#"$生命值"#,
            r#"$"😀 分数""#,
            "@seconds",
            "12",
            "-3",
            r#"ext.(读取值)"#,
            r#"ext::"read".(1)"#,
            r#""自定义拓展".<有效?>"#,
        ] {
            let marked_source = format!(r#"[示例 $"sc|"{next}]"#);
            let (source, items) = symbol_candidates(&marked_source, &symbols);
            assert_eq!(items.len(), 1, "{marked_source}");
            assert_eq!(
                apply(&source, &items[0]),
                format!(r#"[示例 $"score"{next}]"#),
                "{marked_source}",
            );
        }
        let (source, items) = symbol_candidates(r#"[测试 $"sc| "后续参数"]"#, &symbols);
        assert_eq!(items.len(), 1);
        assert_eq!(apply(&source, &items[0]), r#"[测试 $"score" "后续参数"]"#);
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
            ("[移动 @步|旧 步]", "@步数", "[移动 @步数 步]"),
            ("[说 @是|]", "<@是否继续>", "[说 <@是否继续>]"),
            ("<@是|>", "<@是否继续>", "<@是否继续>"),
            ("<@是|", "<@是否继续>", "<@是否继续>"),
            ("<@步|旧 < 10>", "@步数", "<@步数 < 10>"),
            ("<@步|旧 > 10>", "@步数", "<@步数 > 10>"),
            ("<@步|旧>10>", "@步数", "<@步数>10>"),
            ("<@步|旧 > $>", "@步数", "<@步数 > $>"),
            ("<@步|旧\n>\n10>", "@步数", "<@步数\n>\n10>"),
            ("<@是|旧 > 10>", "<@是否继续>", "<<@是否继续> > 10>"),
            ("<@是|旧 > $>", "<@是否继续>", "<<@是否继续> > $>"),
            ("<<@是|旧> > 10>", "<@是否继续>", "<<@是否继续> > 10>"),
            ("<<@是|旧>>10>", "<@是否继续>", "<<@是否继续>>10>"),
            ("<@\"😀 |旧\">10>", "@\"😀 名字\"", "<@\"😀 名字\">10>"),
            ("[说 @\"😀 |旧\"]", "@\"😀 名字\"", "[说 @\"😀 名字\"]"),
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
        let (source, items) = symbol_candidates("[测|]", &symbols);
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
        assert!(text.contains("条件: <>"));
        compile_source(&format!(
            "定义自制积木 测试 步数: @steps 条件: <@enabled> 内容: @text {{ [等待 @steps 秒] }}\n当绿旗被点击 {{ {text} }}"
        ));
    }

    #[test]
    fn builtin_procedure_collision_never_offers_an_ambiguous_or_wrong_call() {
        let registry = Registry::new();
        let mut symbols = data_symbols();
        symbols
            .procedures
            .push(crate::registry::procedure_template("移动 @steps 步", "movement").unwrap());
        let (_, items) = symbol_candidates("[移|]", &symbols);
        assert!(!items.iter().any(|item| item.label.starts_with("[移动 ")));
        for template in registry.templates.iter().chain(symbols.procedures.iter()) {
            if let Some((label, _)) =
                completion_text_visible(template, &registry, &symbols.procedures)
            {
                let call = registry
                    .parse(&label, template.shape, &symbols.procedures)
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
            "@var score = 0\n定义自制积木 等一等 @seconds { [等待 @sec| 秒] }\n当绿旗被点击 { [等一] }",
        );
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "@seconds");
        let (source, cursor) = marked(
            "@var score = 0\n定义自制积木 等一等 @seconds { [等待 @seconds 秒] }\n当绿旗被点击 { [等一|] }",
        );
        let items = complete(&source, cursor, &registry);
        assert_eq!(items.len(), 1);
        let applied = apply(&source, &items[0]);
        compile_source(&applied);
    }
}
