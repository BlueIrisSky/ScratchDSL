use crate::error::{Error, Result};
use crate::model::*;
use crate::parser::{
    parse_menu, parse_name, parse_number, parse_string, scan_atom, scan_atom_candidates,
    strip_comments,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct Registry {
    pub templates: Vec<Template>,
    /// Runtime extension ID -> original local JavaScript source.
    pub extension_sources: BTreeMap<String, String>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            templates: crate::builtins::templates(),
            extension_sources: BTreeMap::new(),
        }
    }

    pub fn parse(&self, source: &str, shape: Shape, visible: &[Template]) -> Result<Invocation> {
        Resolution::new(self, visible).parse(source, shape)
    }

    pub fn value(&self, source: &str, visible: &[Template]) -> Result<Expr> {
        Resolution::new(self, visible).value(source)
    }
}

/// Memoization belongs to one invocation and one immutable visibility scope.
/// It must never survive into another sprite, project, or editor revision.
struct Resolution<'a> {
    registry: &'a Registry,
    visible: &'a [Template],
    calls: BTreeMap<(u8, String), Result<Invocation>>,
    values: BTreeMap<String, Result<Expr>>,
    work: usize,
    depth: usize,
    fatal: Option<Error>,
}

impl<'a> Resolution<'a> {
    fn new(registry: &'a Registry, visible: &'a [Template]) -> Self {
        Self {
            registry,
            visible,
            calls: BTreeMap::new(),
            values: BTreeMap::new(),
            work: 0,
            depth: 0,
            fatal: None,
        }
    }

    fn limit_error(&mut self, message: &str) -> Error {
        let error = Error::new(message);
        self.fatal = Some(error.clone());
        error
    }

    fn charge(&mut self) -> Result<()> {
        if let Some(error) = &self.fatal {
            return Err(error.clone());
        }
        self.work += 1;
        if self.work > 100_000 {
            return Err(
                self.limit_error("积木匹配超出工作量限制，请简化表达式或使用显式 opcode 调用")
            );
        }
        Ok(())
    }

    fn parse(&mut self, source: &str, shape: Shape) -> Result<Invocation> {
        self.charge()?;
        let key = (shape as u8, source.trim().to_owned());
        if let Some(result) = self.calls.get(&key) {
            return result.clone();
        }
        if self.depth >= 128 {
            return Err(self.limit_error("积木表达式嵌套超过 128 层"));
        }
        self.depth += 1;
        let result = self.parse_uncached(source, shape);
        self.depth -= 1;
        let result = self.fatal.clone().map_or(result, Err);
        self.calls.insert(key, result.clone());
        result
    }

    fn value(&mut self, source: &str) -> Result<Expr> {
        self.charge()?;
        let key = source.trim().to_owned();
        if let Some(result) = self.values.get(&key) {
            return result.clone();
        }
        let result = self.value_uncached(source);
        let result = self.fatal.clone().map_or(result, Err);
        self.values.insert(key, result.clone());
        result
    }

    fn parse_uncached(&mut self, source: &str, shape: Shape) -> Result<Invocation> {
        let clean = strip_comments(source)?;
        let source = clean.trim();
        let (namespace, local_opcode, rest) = prefix(source)?;
        if let Some(ns) = &namespace
            && !self
                .registry
                .templates
                .iter()
                .any(|t| t.namespace.as_ref() == Some(ns))
        {
            return Err(Error::new(format!("未知扩展命名空间 {ns:?}")));
        }
        let registry = self.registry;
        let visible = self.visible;
        let candidates: Vec<&Template> = registry
            .templates
            .iter()
            .chain(visible.iter())
            .filter(|t| t.shape == shape && t.namespace == namespace)
            .filter(|t| {
                local_opcode
                    .as_ref()
                    .is_none_or(|op| t.local_opcode.as_ref() == Some(op))
            })
            .collect();
        if local_opcode.is_some() {
            if candidates.len() != 1 {
                return Err(Error::new(format!(
                    "显式 opcode 不存在、重复或形状不符: {source}"
                )));
            }
            let inner = if shape == Shape::Hat {
                wrapped(
                    rest.strip_prefix("hat")
                        .ok_or_else(|| Error::new("显式帽子必须使用 .hat(...)"))?,
                    '(',
                    ')',
                )?
            } else {
                content(rest, shape)?
            };
            let values = self.explicit_arguments(inner)?;
            let template = candidates[0];
            let slots: Vec<&Slot> = template.slots().collect();
            if slots.len() != values.len() {
                return Err(Error::new(format!(
                    "{} 需要 {} 个参数，实际 {} 个",
                    template.opcode,
                    slots.len(),
                    values.len()
                )));
            }
            let args = slots
                .iter()
                .zip(values)
                .map(|(s, v)| validate_slot(s, v))
                .collect::<Result<Vec<_>>>()?;
            return Ok(Invocation {
                template: template.clone(),
                args,
            });
        }
        let inner = content(rest, shape)?;
        let mut matches: Vec<(usize, Invocation)> = Vec::new();
        let mut details = Vec::new();
        for template in candidates {
            match self.match_template(inner, template) {
                Ok(bindings) => {
                    let score = template
                        .tokens
                        .iter()
                        .map(|t| match t {
                            Token::Literal(s) => s.chars().count(),
                            _ => 0,
                        })
                        .sum();
                    for args in bindings {
                        let call = Invocation {
                            template: template.clone(),
                            args,
                        };
                        if let Some(existing) = matches.iter_mut().find(|(_, m)| {
                            m.template.opcode == call.template.opcode
                                && m.template.procedure == call.template.procedure
                                && m.args == call.args
                        }) {
                            if score > existing.0 {
                                *existing = (score, call);
                            }
                        } else {
                            matches.push((score, call));
                        }
                    }
                }
                Err(e) => {
                    if let Some(error) = &self.fatal {
                        return Err(error.clone());
                    }
                    if details.len() < 3 && !e.0.starts_with("固定文字") {
                        let detail = diagnostic_excerpt(&e.0, 240);
                        if !details.contains(&detail) {
                            details.push(detail);
                        }
                    }
                }
            }
        }
        let Some(max) = matches.iter().map(|(n, _)| *n).max() else {
            return Err(Error::new(format!(
                "没有完整匹配的 {:?} 积木: {}{}",
                shape,
                diagnostic_excerpt(source, 300),
                if details.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", details.join("; "))
                }
            )));
        };
        matches.retain(|(n, _)| *n == max);
        if matches.len() > 1 {
            return Err(Error::new(format!(
                "积木调用有歧义: {}；候选 {}",
                source,
                matches
                    .iter()
                    .map(|(_, c)| c
                        .template
                        .procedure
                        .as_deref()
                        .unwrap_or(&c.template.opcode))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(matches.remove(0).1)
    }

    fn value_uncached(&mut self, source: &str) -> Result<Expr> {
        let clean = strip_comments(source)?;
        let s = clean.trim();
        if s.is_empty() {
            return Err(Error::new("缺少参数值"));
        }
        if !scan_atom_candidates(s, 0)?.contains(&s.len()) {
            return Err(Error::new(format!("参数必须是一个完整值: {s}")));
        }
        if let Some(n) = parse_number(s) {
            return Ok(Expr::Number(n));
        }
        if s.starts_with('<') && s.ends_with('>') {
            let inner = s[1..s.len() - 1].trim();
            if inner == "true" {
                return Ok(Expr::Bool(true));
            }
            if inner == "false" {
                return Ok(Expr::Bool(false));
            }
            if inner.is_empty() {
                return Ok(Expr::EmptyBool);
            }
            if let Some(name) = inner.strip_prefix('@')
                && let Ok(name) = parse_name(name)
            {
                return Ok(Expr::Param {
                    name,
                    boolean: true,
                });
            }
        }
        if let Some(t) = s.strip_prefix('$') {
            let (t, global) = t.strip_prefix("global:").map_or((t, false), |v| (v, true));
            let (t, list) = t.strip_prefix("list:").map_or((t, false), |v| (v, true));
            return Ok(Expr::Symbol {
                name: parse_name(t)?,
                list,
                global,
            });
        }
        if let Some(t) = s.strip_prefix('@') {
            return Ok(Expr::Param {
                name: parse_name(t)?,
                boolean: false,
            });
        }
        if s.starts_with('/') {
            return parse_menu(s).map(Expr::Menu);
        }
        let (ns, op, rest) = prefix(s)?;
        if s.starts_with('"') && ns.is_none() && op.is_none() {
            return Ok(Expr::String(parse_string(s)?));
        }
        let shape = if rest.starts_with('(') {
            Shape::Reporter
        } else if rest.starts_with('<') {
            Shape::Boolean
        } else {
            return Err(Error::new(format!("不是合法槽位值: {s}")));
        };
        Ok(Expr::Call(Box::new(self.parse(s, shape)?)))
    }

    fn explicit_arguments(&mut self, s: &str) -> Result<Vec<Expr>> {
        let mut args = Vec::new();
        let mut p = skip_ws(s, 0);
        while p < s.len() {
            let ends = scan_atom_candidates(s, p)?;
            let ends: Vec<_> = ends
                .into_iter()
                .filter(|end| {
                    let next = skip_ws(s, *end);
                    next == s.len() || s[next..].starts_with(',')
                })
                .collect();
            let [end] = ends.as_slice() else {
                return Err(Error::new(
                    "显式调用参数须用逗号分隔，且每个参数必须是完整值",
                ));
            };
            let end = *end;
            args.push(self.value(&s[p..end])?);
            p = skip_ws(s, end);
            if p == s.len() {
                break;
            }
            if !s[p..].starts_with(',') {
                return Err(Error::new("显式调用参数须用逗号分隔"));
            }
            p = skip_ws(s, p + 1);
            if p == s.len() {
                return Err(Error::new("显式调用不允许尾逗号"));
            }
        }
        Ok(args)
    }

    fn match_template(&mut self, s: &str, template: &Template) -> Result<Vec<Vec<Expr>>> {
        // A plain `>` can close a Boolean input or compare its left value.
        // Keep lexical alternatives until the remaining template and slot types
        // decide. Do not discard a valid shorter atom merely because a longer
        // comparison is lexically possible.
        #[derive(Clone, PartialEq)]
        struct State {
            position: usize,
            args: Vec<String>,
            previous_slot: Option<(usize, bool)>,
        }
        let s = s.trim();
        let mut states = vec![State {
            position: 0,
            args: Vec::new(),
            previous_slot: None,
        }];
        let mut attempts = 0usize;
        let mut last_error = Error::new("固定文字不匹配");
        for token in &template.tokens {
            let mut next = Vec::new();
            for mut state in states {
                self.charge()?;
                match token {
                    Token::Literal(text) => {
                        let operator = text.trim();
                        let compact_operator = template.namespace.is_none()
                            && matches!(
                                template.opcode.as_str(),
                                "operator_add"
                                    | "operator_subtract"
                                    | "operator_multiply"
                                    | "operator_divide"
                                    | "operator_mod"
                                    | "operator_lt"
                                    | "operator_gt"
                                    | "operator_equals"
                            )
                            && matches!(operator, "+" | "-" | "*" | "/" | "mod" | "<" | ">" | "=");
                        let matched = if compact_operator {
                            match_literal(s, skip_ws(s, state.position), operator)
                        } else {
                            match_literal(s, state.position, text)
                        };
                        if let Some(position) = matched {
                            state.position = position;
                            if !text.is_empty() {
                                state.previous_slot = None;
                            }
                            next.push(state);
                        }
                    }
                    Token::Slot(slot) => {
                        let start = skip_ws(s, state.position);
                        if state
                            .previous_slot
                            .is_some_and(|(end, delimited)| end == start && !delimited)
                        {
                            last_error = Error::new("相邻参数缺少词法边界");
                            continue;
                        }
                        if start == s.len() {
                            last_error = Error::new(format!("缺少参数 {}", slot.name));
                            continue;
                        }
                        let ends = match scan_atom_candidates(s, start) {
                            Ok(ends) => ends,
                            Err(error) => {
                                if let Some(fatal) = &self.fatal {
                                    return Err(fatal.clone());
                                }
                                last_error = error;
                                continue;
                            }
                        };
                        for end in ends {
                            self.charge()?;
                            attempts += 1;
                            if attempts > 4096 {
                                return Err(
                                    self.limit_error("积木参数切分过多，请使用显式 opcode 调用")
                                );
                            }
                            let atom = &s[start..end];
                            let mut branch = state.clone();
                            branch.position = end;
                            branch.args.push(atom.to_owned());
                            branch.previous_slot =
                                Some((end, atom.ends_with(['"', ')', '>', '/', ']'])));
                            if !next.contains(&branch) {
                                next.push(branch);
                            }
                            if next.len() > 256 {
                                return Err(
                                    self.limit_error("积木参数切分过多，请使用显式 opcode 调用")
                                );
                            }
                        }
                    }
                }
            }
            if next.is_empty() {
                return Err(last_error);
            }
            states = next;
        }
        let mut bindings = Vec::new();
        let mut binding_error = None;
        for state in states {
            if skip_ws(s, state.position) != s.len() {
                continue;
            }
            // Resolve values only after all fixed text and argument boundaries
            // match. An unrelated operator must not recursively resolve a large
            // left operand before discovering that its own operator is absent.
            let args: Result<Vec<_>> = template
                .slots()
                .zip(&state.args)
                .map(|(slot, atom)| {
                    self.value(atom)
                        .and_then(|value| validate_slot(slot, value))
                })
                .collect();
            match args {
                Ok(args) => {
                    if !bindings.contains(&args) {
                        bindings.push(args);
                    }
                }
                Err(error) => {
                    if let Some(fatal) = &self.fatal {
                        return Err(fatal.clone());
                    }
                    binding_error = Some(error);
                }
            }
        }
        if bindings.is_empty() {
            return Err(binding_error.unwrap_or_else(|| Error::new("固定文字后仍有多余输入")));
        }
        Ok(bindings)
    }
}

fn diagnostic_excerpt(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

pub(crate) fn syntax_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}
fn skip_ws(s: &str, p: usize) -> usize {
    p + s[p..]
        .chars()
        .take_while(|c| syntax_ws(*c))
        .map(char::len_utf8)
        .sum::<usize>()
}

fn match_literal(s: &str, mut p: usize, literal: &str) -> Option<usize> {
    for ch in literal.chars() {
        if ch == ' ' {
            let end = skip_ws(s, p);
            if end == p {
                return None;
            }
            p = end;
        } else {
            let next = s.get(p..)?.chars().next()?;
            if next != ch {
                return None;
            }
            p += next.len_utf8();
        }
    }
    Some(p)
}

pub(crate) fn quoted_end(s: &str, start: usize) -> Result<usize> {
    if !s[start..].starts_with('"') {
        return Err(Error::new("需要双引号字符串"));
    }
    let mut escaped = false;
    for (off, c) in s[start + 1..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Ok(start + 1 + off + 1);
        }
    }
    Err(Error::new("字符串未闭合"))
}

fn prefix(s: &str) -> Result<(Option<String>, Option<String>, &str)> {
    let end = if s.starts_with('"') {
        quoted_end(s, 0)?
    } else {
        match crate::parser::bare_namespace_end(s, 0) {
            Some(end) => end,
            None => return Ok((None, None, s)),
        }
    };
    let suffix = &s[end..];
    if !suffix.starts_with('.') && !suffix.starts_with("::") {
        return Ok((None, None, s));
    }
    let ns = if s.starts_with('"') {
        parse_string(&s[..end])?
    } else {
        s[..end].into()
    };
    if suffix.starts_with("::") {
        let op_start = end + 2;
        let op_end = quoted_end(s, op_start)?;
        let op = parse_string(&s[op_start..op_end])?;
        if !s[op_end..].starts_with('.') {
            return Err(Error::new("显式 opcode 后需要紧接 ."));
        }
        Ok((Some(ns), Some(op), &s[op_end + 1..]))
    } else {
        Ok((Some(ns), None, &s[end + 1..]))
    }
}

fn wrapped(s: &str, open: char, close: char) -> Result<&str> {
    if !s.starts_with(open)
        || !s.ends_with(close)
        || !scan_atom_candidates(s, 0)?.contains(&s.len())
    {
        return Err(Error::new(format!("积木形状需要 {open}...{close}: {s}")));
    }
    Ok(&s[open.len_utf8()..s.len() - close.len_utf8()])
}
fn content(s: &str, shape: Shape) -> Result<&str> {
    match shape {
        Shape::Command => wrapped(s, '[', ']'),
        Shape::Reporter => wrapped(s, '(', ')'),
        Shape::Boolean => wrapped(s, '<', '>'),
        Shape::Hat | Shape::Control => Ok(s),
    }
}

pub(crate) fn validate_slot(slot: &Slot, value: Expr) -> Result<Expr> {
    let fail = || {
        Error::new(format!(
            "参数 {} 的值不符合 {:?} 槽位",
            slot.name, slot.arg_type
        ))
    };
    if let Some(menu) = &slot.menu {
        if let Expr::Menu(text) = value {
            if let Some(items) = &menu.items {
                if items.iter().any(|(_, val)| val == &text) {
                    return Ok(Expr::Menu(text));
                }
                let values: BTreeSet<_> = items
                    .iter()
                    .filter(|(label, _)| label == &text)
                    .map(|(_, val)| val.clone())
                    .collect();
                if values.len() == 1 {
                    return Ok(Expr::Menu(values.into_iter().next().unwrap()));
                }
                return Err(Error::new(format!(
                    "菜单 {} 的选项 {:?} {}",
                    slot.name,
                    text,
                    if values.is_empty() {
                        "不存在"
                    } else {
                        "有歧义"
                    }
                )));
            }
            // Builtin dynamic target menus also have fixed translated sentinels.
            let mapped = match (
                slot.shadow.as_ref().map(|s| s.opcode.as_str()),
                text.as_str(),
            ) {
                (
                    Some(
                        "motion_goto_menu"
                        | "sensing_touchingobjectmenu"
                        | "sensing_distancetomenu",
                    ),
                    "鼠标指针",
                ) => "_mouse_",
                (Some("motion_goto_menu"), "随机位置") => "_random_",
                (Some("sensing_touchingobjectmenu"), "边缘") => "_edge_",
                (Some("control_create_clone_of_menu"), "自己" | "自己本身") => "_myself_",
                _ => &text,
            };
            return Ok(Expr::Menu(mapped.to_owned()));
        }
        if !menu.accept_reporters || matches!(value, Expr::EmptyBool) {
            return Err(fail());
        }
        return Ok(value);
    }
    match slot.arg_type {
        ArgType::Boolean => {
            if !matches!(
                &value,
                Expr::Bool(_) | Expr::EmptyBool | Expr::Param { boolean: true, .. }
            ) && !matches!(&value, Expr::Call(c) if c.template.shape == Shape::Boolean)
            {
                return Err(fail());
            }
        }
        ArgType::Variable => {
            if !matches!(&value, Expr::Symbol { list: false, .. }) {
                return Err(fail());
            }
        }
        ArgType::List => {
            if !matches!(&value, Expr::Symbol { list: true, .. }) {
                return Err(fail());
            }
        }
        ArgType::Broadcast if slot.storage == Storage::Field => {
            if !matches!(value, Expr::String(_)) {
                return Err(fail());
            }
        }
        ArgType::Color => match &value {
            Expr::String(s)
                if s.len() == 7
                    && s.starts_with('#')
                    && s[1..].chars().all(|c| c.is_ascii_hexdigit()) => {}
            Expr::Call(_) | Expr::Symbol { .. } | Expr::Param { .. } | Expr::Bool(_) => {}
            _ => return Err(fail()),
        },
        _ => {
            if matches!(value, Expr::Menu(_) | Expr::EmptyBool) {
                return Err(fail());
            }
        }
    }
    Ok(value)
}

pub(crate) fn input(name: &str, arg_type: ArgType) -> Slot {
    let (opcode, field, primitive, default) = match arg_type {
        ArgType::Number => ("math_number", "NUM", 4, json!(0)),
        ArgType::Angle => ("math_angle", "NUM", 8, json!(90)),
        ArgType::Color => ("colour_picker", "COLOUR", 9, json!("#000000")),
        _ => ("text", "TEXT", 10, json!("")),
    };
    Slot {
        name: name.into(),
        arg_type,
        storage: Storage::Input,
        default,
        shadow: if arg_type == ArgType::Boolean {
            None
        } else {
            Some(Shadow {
                opcode: opcode.into(),
                field: field.into(),
                primitive: Some(primitive),
            })
        },
        menu: None,
    }
}

pub(crate) fn tokens_from_text(text: &str, slots: &[Slot]) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut p = 0;
    let mut used = BTreeSet::new();
    while let Some(off) = text[p..].find('[') {
        let start = p + off;
        if start > p {
            tokens.push(Token::Literal(text[p..start].into()));
        }
        let end = text[start + 1..]
            .find(']')
            .map(|v| start + 1 + v)
            .ok_or_else(|| Error::new("模板占位符未闭合"))?;
        let name = &text[start + 1..end];
        if name.is_empty()
            || !name
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
        {
            return Err(Error::new(format!("非法占位符 {name:?}")));
        }
        let slot = slots
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| Error::new(format!("未声明的模板参数 {name}")))?;
        if !used.insert(name.to_owned()) {
            return Err(Error::new(format!("重复模板参数 {name}")));
        }
        tokens.push(Token::Slot(slot.clone()));
        p = end + 1;
    }
    if p < text.len() {
        tokens.push(Token::Literal(text[p..].into()));
    }
    if used.len() != slots.len() {
        return Err(Error::new("模板遗漏声明的参数"));
    }
    normalize_tokens(&mut tokens);
    Ok(tokens)
}

fn normalize_tokens(tokens: &mut Vec<Token>) {
    for token in tokens.iter_mut() {
        if let Token::Literal(text) = token {
            let mut out = String::new();
            let mut space = false;
            for c in text.chars() {
                if syntax_ws(c) {
                    if !space {
                        out.push(' ');
                    }
                    space = true;
                } else {
                    out.push(c);
                    space = false;
                }
            }
            *text = out;
        }
    }
    if let Some(Token::Literal(s)) = tokens.first_mut() {
        *s = s.trim_start().into();
    }
    if let Some(Token::Literal(s)) = tokens.last_mut() {
        *s = s.trim_end().into();
    }
    tokens.retain(|t| !matches!(t, Token::Literal(s) if s.is_empty()));
}

pub fn procedure_template(header: &str, id: &str) -> Result<Template> {
    let clean = strip_comments(header)?;
    let mut header = clean.trim();
    let mut warp = false;
    if let Some(body) = header.strip_suffix("@warp")
        && (body.is_empty() || body.chars().last().is_some_and(syntax_ws))
    {
        header = body.trim_end();
        warp = true;
    }
    let mut tokens = Vec::new();
    let mut names = BTreeSet::new();
    let mut p = 0;
    let mut lit = 0;
    while p < header.len() {
        let boolean = header[p..].starts_with("<@");
        if boolean || header[p..].starts_with('@') {
            if p > lit {
                tokens.push(Token::Literal(header[lit..p].into()));
            }
            let end = if boolean {
                scan_atom_candidates(header, p)?
                    .into_iter()
                    .find(|end| *end > p + 2 && parse_name(&header[p + 2..*end - 1]).is_ok())
                    .ok_or_else(|| Error::new("布尔参数声明需要 <@参数名>"))?
            } else {
                scan_atom(header, p)?
            };
            let name_source = if boolean {
                &header[p + 2..end - 1]
            } else {
                &header[p + 1..end]
            };
            let name = parse_name(name_source)?;
            if name == "warp" {
                return Err(Error::new("@warp 只能作为定义头末尾的单次属性"));
            }
            if !names.insert(name.clone()) {
                return Err(Error::new(format!("重复过程参数 {name}")));
            }
            tokens.push(Token::Slot(input(
                &name,
                if boolean {
                    ArgType::Boolean
                } else {
                    ArgType::Any
                },
            )));
            p = end;
            lit = p;
        } else {
            p += header[p..].chars().next().unwrap().len_utf8();
        }
    }
    if lit < header.len() {
        tokens.push(Token::Literal(header[lit..].into()));
    }
    normalize_tokens(&mut tokens);
    if tokens.is_empty() {
        return Err(Error::new("自制积木定义头不能为空"));
    }
    Ok(Template {
        opcode: "procedures_call".into(),
        shape: Shape::Command,
        namespace: None,
        extension_id: None,
        local_opcode: None,
        tokens,
        sprite_only: false,
        stage_only: false,
        procedure: Some(id.into()),
        warp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_values_and_boolean_types() {
        let r = Registry::new();
        let call = r
            .parse("[将 $score 设为 (1 + (2 * 3))]", Shape::Command, &[])
            .unwrap();
        assert_eq!(call.template.opcode, "data_setvariableto");
        assert!(matches!(call.args[1], Expr::Call(_)));
        assert!(r.value("(1 + 2 * 3)", &[]).is_err());
        assert!(r.parse("[等待直到 1]", Shape::Command, &[]).is_err());
        assert!(r.parse("[等待直到 <>]", Shape::Command, &[]).is_ok());
        assert!(r.parse("[说 <>]", Shape::Command, &[]).is_err());
        assert!(r.parse("[说 <true>]", Shape::Command, &[]).is_ok());
    }
    #[test]
    fn binary_operators_accept_spaces_or_compact_operands_without_precedence() {
        let registry = Registry::new();
        for (source, opcode) in [
            ("(1 + 2)", "operator_add"),
            ("(1+2)", "operator_add"),
            ("(1 - 2)", "operator_subtract"),
            ("(1-2)", "operator_subtract"),
            ("(2 * 3)", "operator_multiply"),
            ("(2*3)", "operator_multiply"),
            ("(8 / 2)", "operator_divide"),
            ("(8/2)", "operator_divide"),
            ("(7 mod 3)", "operator_mod"),
            ("(7mod3)", "operator_mod"),
            ("<$hp < 1>", "operator_lt"),
            ("<$hp<1>", "operator_lt"),
            ("<$score > 100>", "operator_gt"),
            ("<$score>100>", "operator_gt"),
            ("<$score = 100>", "operator_equals"),
            ("<$score=100>", "operator_equals"),
            ("(1e-5--2)", "operator_subtract"),
            ("(-1+-2)", "operator_add"),
            ("(.5*-.5)", "operator_multiply"),
            ("(1e+5/2E-1)", "operator_divide"),
            ("((8/2)/(4/2))", "operator_divide"),
            ("<<$hp<2> 与 <$score>100>>", "operator_and"),
            ("<(8/2)>(3-2)>", "operator_gt"),
            ("(8 / # division comment\n 2)", "operator_divide"),
        ] {
            let Expr::Call(call) = registry
                .value(source, &[])
                .unwrap_or_else(|error| panic!("{source}: {error}"))
            else {
                panic!("{source}")
            };
            assert_eq!(call.template.opcode, opcode, "{source}");
        }
        for source in [
            "(1+*2)",
            "(1-*2)",
            "(1**2)",
            "(8/*2)",
            "<1=*2>",
            "<1>*2>",
            "<1<*2>",
            "(1+2*3)",
            "(1+2+3)",
            "(8/2/2)",
            "(7mod3mod2)",
            "<1<2<3>",
            "<1>2=3>",
            "(1++2)",
            "(1//2)",
            "(7%3)",
        ] {
            assert!(registry.value(source, &[]).is_err(), "{source}");
        }
    }

    #[test]
    fn operators_do_not_change_menu_strings_or_explicit_argument_lists() {
        let mut registry = Registry::new();
        assert_eq!(
            registry.value("/'*'/", &[]).unwrap(),
            Expr::Menu("*".into())
        );
        assert_eq!(
            registry.value("/'**'/", &[]).unwrap(),
            Expr::Menu("**".into())
        );
        assert_eq!(
            registry.value("/'a#b'/", &[]).unwrap(),
            Expr::Menu("a#b".into())
        );
        assert_eq!(
            registry.value(r#""+* -* ** /* > =*""#, &[]).unwrap(),
            Expr::String("+* -* ** /* > =*".into())
        );
        let mut extension =
            procedure_template("处理 @number 菜单 @menu 判断 <@ok>", "unused").unwrap();
        extension.namespace = Some("ext".into());
        extension.local_opcode = Some("handle".into());
        extension.opcode = "ext_handle".into();
        extension.procedure = None;
        for token in &mut extension.tokens {
            if let Token::Slot(slot) = token
                && slot.name == "menu"
            {
                slot.menu = Some(Menu {
                    items: None,
                    accept_reporters: false,
                });
            }
        }
        registry.templates.push(extension);
        let call = registry
            .parse(
                r#"ext::"handle".[(8/2), /'*'/, <$score>100>]"#,
                Shape::Command,
                &[],
            )
            .unwrap();
        assert_eq!(call.args[1], Expr::Menu("*".into()));
        assert!(
            registry
                .parse(r#"ext::"handle".[8 /2/, /'*'/, <>]"#, Shape::Command, &[])
                .is_err()
        );
        let procedure = procedure_template("检查 <@ok> @number", "check").unwrap();
        assert!(
            registry
                .parse("[检查 <@ready> 1]", Shape::Command, &[procedure])
                .is_ok()
        );
    }
    #[test]
    fn procedure_signature_ignores_parameter_names_and_normalizes_spaces() {
        let a = procedure_template("移动  步数: @steps 判断: <@yes> @warp", "a").unwrap();
        let b = procedure_template("移动 步数: @x 判断: <@y>", "b").unwrap();
        assert_eq!(a.signature(), b.signature());
        assert!(a.warp);
        assert!(procedure_template("重复 @x @x", "bad").is_err());
        assert!(procedure_template("F @warp @x", "bad").is_err());
        let r = Registry::new();
        assert!(
            r.parse("[移动 步数: 3 判断: <true>]", Shape::Command, &[a])
                .is_ok()
        );
    }
    #[test]
    fn menus_map_labels_and_reject_invalid_fields() {
        let r = Registry::new();
        let call = r.parse("当按下 /'空格'/ 键", Shape::Hat, &[]).unwrap();
        assert_eq!(call.args, vec![Expr::Menu("space".into())]);
        assert!(r.parse("当按下 \"space\" 键", Shape::Hat, &[]).is_err());
        assert!(r.parse("当按下 /'not-a-key'/ 键", Shape::Hat, &[]).is_err());
    }
    #[test]
    fn nested_boolean_values_do_not_consume_the_following_slot() {
        let mut registry = Registry::new();
        let mut template = procedure_template("选择 <@flag> @number", "select").unwrap();
        template.shape = Shape::Boolean;
        registry.templates.push(template);
        let call = registry
            .parse("<选择 <@ready> 1>", Shape::Boolean, &[])
            .unwrap();
        assert_eq!(
            call.args,
            vec![
                Expr::Param {
                    name: "ready".into(),
                    boolean: true
                },
                Expr::Number(1.0)
            ]
        );
        let call = registry
            .parse("<选择 <@ready> <2 > 1>>", Shape::Boolean, &[])
            .unwrap();
        assert_eq!(call.args.len(), 2);
        assert!(matches!(&call.args[1], Expr::Call(call) if call.template.opcode == "operator_gt"));
        let compared = registry.value("<<@ready> > 1>", &[]).unwrap();
        assert!(matches!(compared, Expr::Call(call) if call.template.opcode == "operator_gt"));
    }

    #[test]
    fn comparison_arguments_do_not_hide_a_complete_template_ambiguity() {
        let mut registry = Registry::new();
        for opcode in ["first", "second"] {
            let mut template = procedure_template("选择 @value", opcode).unwrap();
            template.opcode = format!("ext_{opcode}");
            template.namespace = Some("ext".into());
            template.local_opcode = Some(opcode.into());
            template.procedure = None;
            registry.templates.push(template);
        }
        let error = registry
            .parse("ext.[选择 <@number > 1>]", Shape::Command, &[])
            .unwrap_err();
        assert!(error.to_string().contains("歧义"), "{error}");
        assert!(
            registry
                .parse(r#"ext::"first".[<@number > 1>]"#, Shape::Command, &[])
                .is_ok()
        );
    }

    #[test]
    fn boolean_parameter_declarations_do_not_consume_following_literal_text() {
        let template = procedure_template("测试 <@ready> 1> @number", "literal").unwrap();
        let slots: Vec<_> = template.slots().map(|slot| slot.name.as_str()).collect();
        assert_eq!(slots, ["ready", "number"]);
    }

    #[test]
    fn nested_comparisons_resolve_with_bounded_work_in_both_directions() {
        let registry = Registry::new();
        for left_nested in [false, true] {
            let mut source = "<1 > 0>".to_owned();
            for _ in 1..24 {
                source = if left_nested {
                    format!("<{source} > 0>")
                } else {
                    format!("<1 > {source}>")
                };
            }
            let mut resolution = Resolution::new(&registry, &[]);
            let expression = resolution.value(&source).unwrap();
            let mut current = &expression;
            for _ in 0..24 {
                let Expr::Call(call) = current else {
                    panic!("comparison was lost")
                };
                assert_eq!(call.template.opcode, "operator_gt");
                current = &call.args[usize::from(!left_nested)];
            }
            assert!(matches!(current, Expr::Number(_)));
            assert!(
                resolution.work < 10_000,
                "{} matching steps",
                resolution.work
            );
        }
    }

    #[test]
    fn deeply_nested_failures_keep_diagnostics_bounded() {
        let registry = Registry::new();
        let mut source = "<1>".to_owned();
        for _ in 0..24 {
            source = format!("<{source} > 0>");
        }
        let mut resolution = Resolution::new(&registry, &[]);
        let error = resolution.value(&source).unwrap_err();
        assert!(error.0.contains("没有完整匹配"));
        assert!(
            error.0.chars().count() < 1100,
            "diagnostic grew to {} bytes",
            error.0.len()
        );
        assert!(
            resolution.work < 10_000,
            "{} matching steps",
            resolution.work
        );
    }

    #[test]
    fn resolution_caches_never_cross_visibility_scopes_or_registry_edits() {
        let mut registry = Registry::new();
        let mut first = procedure_template("读取 @n", "first").unwrap();
        first.shape = Shape::Reporter;
        let mut second = first.clone();
        second.procedure = Some("second".into());
        for template in [&first, &second] {
            let Expr::Call(call) = registry
                .value("(读取 1)", std::slice::from_ref(template))
                .unwrap()
            else {
                panic!("expected custom reporter")
            };
            assert_eq!(call.template.procedure, template.procedure);
        }
        assert!(registry.value("(读取 1)", &[]).is_err());
        registry.templates.push(first);
        assert!(registry.value("(读取 1)", &[]).is_ok());
    }

    #[test]
    fn exhausted_resolution_budget_is_fatal_even_with_a_valid_fallback() {
        let registry = Registry::new();
        let mut resolution = Resolution::new(&registry, &[]);
        resolution.work = 100_000;
        let error = resolution.parse("[说 1]", Shape::Command).unwrap_err();
        assert!(error.0.contains("工作量限制"));
        assert_eq!(resolution.value("1").unwrap_err().0, error.0);
    }

    #[test]
    fn explicit_arguments_keep_comparison_boundaries_and_require_commas() {
        let mut registry = Registry::new();
        let mut template = procedure_template("选择 <@flag> @number", "unused").unwrap();
        template.namespace = Some("ext".into());
        template.local_opcode = Some("choose".into());
        template.procedure = None;
        registry.templates.push(template);
        for source in [
            "ext::\"choose\".[<@flag>, 1]",
            "ext::\"choose\".[<@flag > 1>, 2]",
            "ext::\"choose\".[<@flag> # before comma # , <1 > 0>]",
        ] {
            assert_eq!(
                registry
                    .parse(source, Shape::Command, &[])
                    .unwrap()
                    .args
                    .len(),
                2,
                "{source}"
            );
        }
        for source in [
            "ext::\"choose\".[<@flag> 1]",
            "ext::\"choose\".[<@flag>, 1,]",
            "ext::\"choose\".[<@flag>,, 1]",
        ] {
            assert!(
                registry.parse(source, Shape::Command, &[]).is_err(),
                "{source}"
            );
        }
    }
}
