//! Structural ScratchDSL parser and the lexical primitives shared by the registry.
//!
//! Templates are resolved later. This module preserves their text while checking
//! source structure, declaration contexts, comments and literal boundaries.
use crate::model::{Item, Span, SpriteInit, Statement};
use crate::notation::syntax_ws as whitespace;
use crate::{Error, Result};
use serde_json::{Number, Value};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::Path,
    rc::Rc,
};
use unicode_general_category::{GeneralCategory, get_general_category};

mod indentation;
pub use indentation::{ParseReport, parse_recovering};

#[derive(Clone, Debug)]
struct LexError {
    offset: usize,
    message: String,
}
type LexResult<T> = std::result::Result<T, LexError>;

fn lex_error(offset: usize, message: impl Into<String>) -> LexError {
    LexError {
        offset,
        message: message.into(),
    }
}
fn public_error(e: LexError) -> Error {
    Error::new(format!("{} (byte {})", e.message, e.offset))
}
fn next_char(source: &str, pos: usize) -> Option<char> {
    source.get(pos..)?.chars().next()
}
fn advance(source: &str, pos: usize) -> usize {
    pos + next_char(source, pos).map_or(0, char::len_utf8)
}
fn letter(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}
fn name_start(c: char) -> bool {
    c == '_' || letter(c)
}
fn name_continue(c: char) -> bool {
    name_start(c) || get_general_category(c) == GeneralCategory::DecimalNumber
}

pub(crate) fn namespace_continue(c: char) -> bool {
    name_continue(c) || matches!(c, '.' | '-')
}

pub(crate) fn is_bare_namespace(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(name_start) && chars.all(namespace_continue)
}

/// Find the namespace before the final dot in an uninterrupted name, or before
/// an explicit opcode separator. Dots inside filenames are not qualification
/// levels. Requiring a letter/underscore first keeps numeric literals separate.
pub(crate) fn bare_namespace_end(source: &str, start: usize) -> Option<usize> {
    if !next_char(source, start).is_some_and(name_start) {
        return None;
    }
    let mut end = advance(source, start);
    while next_char(source, end).is_some_and(namespace_continue) {
        end = advance(source, end);
    }
    if source[end..].starts_with("::") {
        return Some(end);
    }
    source[start..end].rfind('.').map(|offset| start + offset)
}
pub(crate) fn bare_name_end(source: &str, start: usize) -> Option<usize> {
    if !next_char(source, start).is_some_and(name_start) {
        return None;
    }
    let mut end = advance(source, start);
    while next_char(source, end).is_some_and(name_continue) {
        end = advance(source, end);
    }
    Some(end)
}
fn skip_space(source: &str, mut pos: usize) -> usize {
    while next_char(source, pos).is_some_and(whitespace) {
        pos = advance(source, pos);
    }
    pos
}

fn string_end(source: &str, start: usize) -> LexResult<usize> {
    if next_char(source, start) != Some('"') {
        return Err(lex_error(start, "expected a double-quoted string"));
    }
    let mut pos = start + 1;
    while let Some(c) = next_char(source, pos) {
        match c {
            '"' => return Ok(pos + 1),
            '\n' | '\r' => {
                return Err(lex_error(
                    pos,
                    "strings cannot cross physical lines; use \\n or \\r",
                ));
            }
            '\\' => {
                let escaped = pos + 1;
                match next_char(source, escaped) {
                    Some('"' | '\\' | 'n' | 'r') => pos = escaped + 1,
                    Some(_) => {
                        return Err(lex_error(
                            pos,
                            "unsupported string escape; allowed: \\\", \\\\, \\n, \\r",
                        ));
                    }
                    None => return Err(lex_error(pos, "unterminated string escape")),
                }
            }
            _ => pos += c.len_utf8(),
        }
    }
    Err(lex_error(start, "unterminated string"))
}

/// Locate a JSON string without imposing DSL escape restrictions. serde_json
/// validates its complete escape sequence and Unicode surrogate pairs later.
fn json_string_end(source: &str, start: usize) -> LexResult<usize> {
    let mut pos = start + 1;
    while let Some(c) = next_char(source, pos) {
        match c {
            '"' => return Ok(pos + 1),
            '\n' | '\r' => {
                return Err(lex_error(pos, "JSON strings cannot cross physical lines"));
            }
            '\\' => {
                pos += 1;
                match next_char(source, pos) {
                    None => return Err(lex_error(pos, "unterminated JSON string escape")),
                    Some('\n' | '\r') => {
                        return Err(lex_error(pos, "JSON strings cannot cross physical lines"));
                    }
                    Some(escaped) => pos += escaped.len_utf8(),
                }
            }
            _ => pos += c.len_utf8(),
        }
    }
    Err(lex_error(start, "unterminated JSON string"))
}

/// Decode one complete DSL string. Only `\"`, `\\`, `\n`, and `\r` are escapes.
pub fn parse_string(source: &str) -> Result<String> {
    let end = string_end(source, 0).map_err(public_error)?;
    if end != source.len() {
        return Err(Error::new("unexpected text after string"));
    }
    let mut out = String::new();
    let mut chars = source[1..end - 1].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(match chars.next().expect("string_end checked escape") {
                '"' => '"',
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                _ => unreachable!(),
            });
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

/// Scan and decode a single-quoted menu using JSON's escape and Unicode rules.
/// Only the quote delimiter differs: `\'` is also accepted, and literal double
/// quotes are escaped before serde_json validates the normalized string.
fn menu_token(source: &str, start: usize) -> LexResult<(usize, String)> {
    if next_char(source, start) != Some('\'') {
        return Err(lex_error(start, "menu values must use 'value'"));
    }
    let mut pos = start + 1;
    let mut json = String::from("\"");
    let mut origins = vec![start];
    while let Some(c) = next_char(source, pos) {
        match c {
            '\'' => {
                json.push('"');
                origins.push(pos);
                let value = serde_json::from_str::<String>(&json).map_err(|error| {
                    let offset = origins
                        .get(error.column().saturating_sub(1))
                        .copied()
                        .unwrap_or(pos);
                    lex_error(offset, format!("invalid menu string: {error}"))
                })?;
                return Ok((pos + 1, value));
            }
            '\n' | '\r' => {
                return Err(lex_error(pos, "menu values cannot cross physical lines"));
            }
            '\\' => {
                let escaped = pos + 1;
                match next_char(source, escaped) {
                    Some('\'') => {
                        json.push('\'');
                        origins.push(escaped);
                    }
                    Some('\n' | '\r') => {
                        return Err(lex_error(
                            escaped,
                            "menu values cannot cross physical lines",
                        ));
                    }
                    Some(c) => {
                        json.push('\\');
                        origins.push(pos);
                        json.push(c);
                        origins.extend(std::iter::repeat_n(escaped, c.len_utf8()));
                    }
                    None => return Err(lex_error(pos, "unterminated menu escape")),
                }
                pos = advance(source, escaped);
            }
            '"' => {
                json.push_str("\\\"");
                origins.extend([pos, pos]);
                pos += 1;
            }
            _ => {
                json.push(c);
                origins.extend(std::iter::repeat_n(pos, c.len_utf8()));
                pos += c.len_utf8();
            }
        }
    }
    Err(lex_error(start, "unterminated menu; expected '"))
}

/// Decode a complete `'value'` menu.
pub fn parse_menu(source: &str) -> Result<String> {
    let (end, value) = menu_token(source, 0).map_err(public_error)?;
    if end != source.len() {
        return Err(Error::new("unexpected text after menu"));
    }
    Ok(value)
}

/// Encode one menu value for completion and other generated ScratchDSL text.
pub fn format_menu(value: &str) -> String {
    let json = serde_json::to_string(value).expect("serializing a string cannot fail");
    format!("'{}'", json[1..json.len() - 1].replace('\'', "\\'"))
}

fn number_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut pos = start;
    if bytes.get(pos) == Some(&b'-') {
        pos += 1;
    }
    let before = pos;
    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
        pos += 1;
    }
    let integer = pos > before;
    if bytes.get(pos) == Some(&b'.') {
        let dot = pos;
        pos += 1;
        let fractional = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if pos == fractional {
            if !integer {
                return None;
            }
            pos = dot;
        }
    } else if !integer {
        return None;
    }
    if matches!(bytes.get(pos), Some(b'e' | b'E')) {
        let exponent = pos;
        pos += 1;
        if matches!(bytes.get(pos), Some(b'+' | b'-')) {
            pos += 1;
        }
        let digits = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if digits == pos {
            pos = exponent;
        }
    }
    Some(pos)
}

/// Parse one finite, complete decimal/scientific literal (no surrounding whitespace).
pub fn parse_number(source: &str) -> Option<f64> {
    if number_end(source, 0)? != source.len() {
        return None;
    }
    source.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn name_end(source: &str, start: usize) -> LexResult<usize> {
    if next_char(source, start) == Some('"') {
        string_end(source, start)
    } else {
        bare_name_end(source, start)
            .ok_or_else(|| lex_error(start, "expected a Unicode identifier or quoted name"))
    }
}

/// Decode a complete ordinary or quoted name, preserving case and Unicode spelling.
pub fn parse_name(source: &str) -> Result<String> {
    let end = name_end(source, 0).map_err(public_error)?;
    if end != source.len() {
        return Err(Error::new("invalid characters in name"));
    }
    if source.starts_with('"') {
        parse_string(source)
    } else {
        Ok(source.into())
    }
}

/// Data names use ordinary Unicode identifiers; quoting is reserved for
/// strings, parameter names, namespaces, and other non-data syntax.
pub fn is_data_name(source: &str) -> bool {
    bare_name_end(source, 0) == Some(source.len())
}

fn reference_parts(source: &str) -> (&str, usize) {
    source
        .strip_prefix("global:")
        .map_or((source, 0), |tail| (tail, 7))
}

fn reference_end(source: &str, start: usize) -> LexResult<usize> {
    let (_, prefix) = reference_parts(&source[start..]);
    bare_name_end(source, start + prefix)
        .ok_or_else(|| lex_error(start, "data references require an ordinary identifier"))
}

/// Decode a complete bare or global:name reference as (name, global).
/// Declaration-aware resolution determines whether the shared name refers to a
/// scalar variable or a list.
pub fn parse_reference(source: &str) -> Result<(String, bool)> {
    let (name, prefix) = reference_parts(source);
    if !is_data_name(name) {
        return Err(Error::new("invalid characters in variable reference"));
    }
    Ok((name.into(), prefix != 0))
}

#[derive(Clone, Copy, Debug)]
pub struct Assignment<'a> {
    pub target: &'a str,
    pub operator: &'a str,
    pub value: &'a str,
}

/// Split an assignment without resolving its target or RHS. An empty RHS is
/// retained so editors can offer value completions while a statement is typed.
pub fn assignment_parts(source: &str) -> Option<Assignment<'_>> {
    let source = source.trim();
    let end = scan_atom(source, 0).ok()?;
    let tail = source[end..].trim_start();
    let operator = ["+=", "-=", "*=", "/=", "%=", "="]
        .into_iter()
        .find(|operator| tail.starts_with(operator))?;
    Some(Assignment {
        target: &source[..end],
        operator,
        value: tail[operator.len()..].trim(),
    })
}

/// A scan records comment spans so stripping preserves byte offsets and line numbers.
#[derive(Clone)]
struct Scanner<'a> {
    cache: Rc<RefCell<GroupCache>>,
    work: Rc<Cell<usize>>,
    hard_error: Rc<RefCell<Option<LexError>>>,
    depth: usize,
    structural_group: Option<(usize, StructuralBoundary)>,
    source: &'a str,
    comments: Vec<(usize, usize)>,
}

type GroupCache =
    BTreeMap<(usize, bool, Option<StructuralBoundary>), LexResult<Vec<AtomCandidate>>>;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StructuralBoundary {
    Suite,
    Statement,
}

#[derive(Clone)]
struct AtomCandidate {
    end: usize,
    comments: Vec<(usize, usize)>,
}

impl<'a> Scanner<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            cache: Rc::new(RefCell::new(BTreeMap::new())),
            work: Rc::new(Cell::new(0)),
            hard_error: Rc::new(RefCell::new(None)),
            depth: 0,
            structural_group: None,
            source,
            comments: Vec::new(),
        }
    }
    fn starts(&self, pos: usize, text: &str) -> bool {
        self.source
            .get(pos..)
            .is_some_and(|tail| tail.starts_with(text))
    }
    fn comment(&mut self, start: usize) -> LexResult<usize> {
        let end = crate::notation::comment_end(self.source, start)
            .ok_or_else(|| lex_error(start, "unterminated block comment"))?;
        self.comments.push((start, end));
        Ok(end)
    }
    fn trivia(&mut self, mut pos: usize) -> LexResult<usize> {
        loop {
            pos = skip_space(self.source, pos);
            if next_char(self.source, pos) == Some('#') {
                pos = self.comment(pos)?;
            } else {
                return Ok(pos);
            }
        }
    }
    /// Initialization data objects use JSON strings and nested arrays.
    fn json_object(&mut self, start: usize) -> LexResult<usize> {
        let mut closes = vec!['}'];
        let mut pos = start + 1;
        while let Some(c) = next_char(self.source, pos) {
            match c {
                '"' => pos = json_string_end(self.source, pos)?,
                '#' => pos = self.comment(pos)?,
                '{' | '[' => {
                    closes.push(if c == '{' { '}' } else { ']' });
                    pos += 1;
                }
                '}' | ']' => {
                    if closes.pop() != Some(c) {
                        return Err(lex_error(pos, "mismatched delimiter in init object"));
                    }
                    pos += 1;
                    if closes.is_empty() {
                        return Ok(pos);
                    }
                }
                _ => pos += c.len_utf8(),
            }
        }
        Err(lex_error(start, "unclosed init object; expected }"))
    }
    /// Locate the end of an init suite while keeping its JSON escape rules
    /// separate from ordinary DSL strings in following sibling statements.
    fn json_indented_body(&mut self, start: usize, indentation: usize) -> LexResult<usize> {
        let mut pos = start;
        let mut line_start = false;
        while let Some(c) = next_char(self.source, pos) {
            if matches!(c, '\n' | '\r') {
                line_start = true;
                pos += 1;
                continue;
            }
            if matches!(c, ' ' | '\t') {
                pos += 1;
                continue;
            }
            if c == '#' {
                let end = self.comment(pos)?;
                if self.source[pos..end].contains(['\n', '\r']) {
                    line_start = true;
                }
                pos = end;
                continue;
            }
            if line_start {
                let prefix = self.source[..pos].rsplit('\n').next().unwrap_or("");
                if prefix
                    .bytes()
                    .take_while(|c| matches!(c, b' ' | b'\t'))
                    .count()
                    <= indentation
                {
                    return Ok(pos);
                }
            }
            line_start = false;
            pos = if c == '"' {
                json_string_end(self.source, pos)?
            } else {
                pos + c.len_utf8()
            };
        }
        Ok(pos)
    }
    fn menu(&self, start: usize) -> LexResult<usize> {
        menu_token(self.source, start).map(|(end, _)| end)
    }
    /// Locate the opening group of a syntactically qualified call.
    fn qualified_open(&self, start: usize) -> LexResult<Option<usize>> {
        let quoted = next_char(self.source, start) == Some('"');
        let name_end = if quoted {
            string_end(self.source, start)?
        } else if let Some(end) = bare_namespace_end(self.source, start) {
            end
        } else {
            return Ok(None);
        };
        if self.starts(name_end, "::") {
            let opcode_start = name_end + 2;
            if next_char(self.source, opcode_start) != Some('"') {
                return Err(lex_error(
                    opcode_start,
                    "explicit local opcode must be double quoted",
                ));
            }
            let opcode_end = string_end(self.source, opcode_start)?;
            if next_char(self.source, opcode_end) != Some('(') {
                return Err(lex_error(
                    opcode_end,
                    "expected (...) arguments after explicit opcode",
                ));
            }
            return Ok(Some(opcode_end));
        }
        Ok(None)
    }
    fn qualified(&mut self, start: usize) -> LexResult<Option<usize>> {
        match self.qualified_open(start)? {
            Some(open) => self.group(open, true).map(Some),
            None => Ok(None),
        }
    }
    fn atom_candidates(&mut self, start: usize) -> LexResult<Vec<AtomCandidate>> {
        match next_char(self.source, start) {
            Some('[' | '(' | '<') => return self.group_candidates(start, false),
            Some(c) if c == '"' || name_start(c) => {
                if let Some(open) = self.qualified_open(start)? {
                    return self.group_candidates(open, true);
                }
            }
            _ => {}
        }
        let end = self.atom(start)?;
        Ok(vec![AtomCandidate {
            end,
            comments: self.comments.clone(),
        }])
    }
    fn atom(&mut self, start: usize) -> LexResult<usize> {
        match next_char(self.source, start) {
            Some('"') => {
                if let Some(end) = self.qualified(start)? {
                    Ok(end)
                } else {
                    string_end(self.source, start)
                }
            }
            Some('@') => name_end(self.source, start + 1),
            Some('$') => Err(lex_error(
                start,
                "$ variable references are no longer supported; use a bare name",
            )),
            Some('\'') => self.menu(start),
            Some('[' | '(' | '<') => self.group(start, false),
            Some(c) if c.is_ascii_digit() || c == '-' || c == '.' => {
                let end = number_end(self.source, start)
                    .ok_or_else(|| lex_error(start, "invalid number"))?;
                if parse_number(&self.source[start..end]).is_none() {
                    return Err(lex_error(start, "numeric literal is not finite"));
                }
                Ok(end)
            }
            Some(c) if name_start(c) => match self.qualified(start)? {
                Some(end) => Ok(end),
                None => reference_end(self.source, start),
            },
            _ => Err(lex_error(start, "expected a complete slot value")),
        }
    }
    fn group(&mut self, start: usize, argument_list: bool) -> LexResult<usize> {
        let previous = self.structural_group;
        if self.depth == 0 {
            let prefix = self.source[..start]
                .rsplit(['\r', '\n'])
                .next()
                .unwrap_or("")
                .trim();
            if is_control_header_group(self.source, start) {
                self.structural_group = Some((start, StructuralBoundary::Suite));
            } else if assignment_parts(prefix).is_some() || command_source(prefix) {
                self.structural_group = Some((start, StructuralBoundary::Statement));
            }
        }
        let result = self.group_candidates(start, argument_list);
        self.structural_group = previous;
        let candidate = result?
            .pop()
            .expect("a successful scan has at least one candidate");
        self.comments = candidate.comments;
        Ok(candidate.end)
    }
    fn structural_boundary(&self, start: usize) -> Option<StructuralBoundary> {
        self.structural_group
            .filter(|(open, _)| *open == start)
            .map(|(_, boundary)| boundary)
    }
    fn ends_structural_group(&self, start: usize, end: usize) -> bool {
        match self.structural_boundary(start) {
            Some(StructuralBoundary::Suite) => suite_boundary_after(self.source, end),
            Some(StructuralBoundary::Statement) => {
                statement_boundary_after(self.source, start, end)
            }
            None => false,
        }
    }
    /// Keep both meanings of a plain `>` until the enclosing group determines
    /// which complete boundaries survive. Template matching resolves any
    /// remaining alternatives using the slot types and following fixed text.
    fn complexity_error(&self, offset: usize, message: &str) -> LexError {
        let error = lex_error(offset, message);
        *self.hard_error.borrow_mut() = Some(error.clone());
        error
    }
    fn group_candidates(
        &mut self,
        start: usize,
        argument_list: bool,
    ) -> LexResult<Vec<AtomCandidate>> {
        if let Some(error) = self.hard_error.borrow().clone() {
            return Err(error);
        }
        if self.depth >= 128 {
            return Err(self.complexity_error(start, "expression nesting exceeds 128 groups"));
        }
        let prefix = std::mem::take(&mut self.comments);
        let key = (start, argument_list, self.structural_boundary(start));
        let cached = self.cache.borrow().get(&key).cloned();
        let result = if let Some(cached) = cached {
            cached
        } else {
            let result = self.group_candidates_uncached(start, argument_list);
            self.cache.borrow_mut().insert(key, result.clone());
            result
        };
        self.comments = prefix.clone();
        if let Some(error) = self.hard_error.borrow().clone() {
            return Err(error);
        }
        result.map(|mut candidates| {
            for candidate in &mut candidates {
                let mut comments = prefix.clone();
                comments.append(&mut candidate.comments);
                candidate.comments = comments;
            }
            candidates
        })
    }
    fn group_candidates_uncached(
        &mut self,
        start: usize,
        argument_list: bool,
    ) -> LexResult<Vec<AtomCandidate>> {
        let open = next_char(self.source, start)
            .ok_or_else(|| lex_error(start, "expected opening bracket"))?;
        let close = match open {
            '[' => ']',
            '(' => ')',
            '<' => '>',
            _ => return Err(lex_error(start, "expected opening bracket")),
        };
        let mut child = self.clone();
        child.depth += 1;
        let mut pending = vec![(
            start + 1,
            !argument_list && matches!(open, '(' | '<'),
            false,
            child,
        )];
        let mut seen = BTreeSet::new();
        let mut results = Vec::new();
        let mut failure = lex_error(start, format!("unclosed {open}; expected {close}"));
        let mut steps = 0;
        while let Some((mut pos, mut possible_infix, mut left_value, mut scanner)) = pending.pop() {
            loop {
                if results.iter().any(|candidate: &AtomCandidate| {
                    candidate.end <= pos && self.ends_structural_group(start, candidate.end)
                }) {
                    break;
                }
                steps += 1;
                self.work.set(self.work.get() + 1);
                if self.work.get() > 100_000 {
                    return Err(
                        self.complexity_error(start, "expression scanning exceeded its work limit")
                    );
                }
                if steps > 4096 {
                    return Err(
                        self.complexity_error(start, "expression has too many possible boundaries")
                    );
                }
                if !seen.insert((pos, possible_infix, left_value)) {
                    break;
                }
                let Some(c) = next_char(self.source, pos) else {
                    break;
                };
                if whitespace(c) {
                    pos = skip_space(self.source, pos);
                    continue;
                }
                if c == '#' {
                    match scanner.comment(pos) {
                        Ok(end) => {
                            pos = end;
                            continue;
                        }
                        Err(error) => {
                            failure = error;
                            break;
                        }
                    }
                }
                let comparison = matches!(c, '<' | '>')
                    && possible_infix
                    && left_value
                    && infix_right_end(self.source, start, pos, false, Some(&scanner)).is_some();
                if c == close {
                    results.push(AtomCandidate {
                        end: pos + 1,
                        comments: scanner.comments.clone(),
                    });
                    if results.len() > 64 {
                        return Err(self.complexity_error(
                            start,
                            "expression has more than 64 possible boundaries",
                        ));
                    }
                    if !comparison {
                        break;
                    }
                }
                if comparison {
                    // The lookahead established one complete RHS followed by
                    // this group's closer. Do not reinterpret shorter RHS
                    // alternatives as additional template arguments.
                    let right_start = scanner.trivia(pos + 1)?;
                    for candidate in scanner.atom_candidates(right_start)? {
                        let mut completed = scanner.clone();
                        completed.comments = candidate.comments;
                        let after = completed.trivia(candidate.end)?;
                        if next_char(self.source, after) == Some(close) {
                            results.push(AtomCandidate {
                                end: after + 1,
                                comments: completed.comments,
                            });
                        }
                    }
                    break;
                }
                if matches!(c, ')' | ']' | '>') {
                    failure = lex_error(pos, format!("mismatched bracket: expected {close}"));
                    break;
                }
                if matches!(c, '{' | '}') {
                    failure =
                        lex_error(pos, "unquoted braces are not allowed inside an expression");
                    break;
                }
                if c == '/' {
                    if left_value && !argument_list && open == '(' {
                        pos += 1;
                        left_value = false;
                        possible_infix = true;
                    } else {
                        failure = lex_error(pos, "division requires a left operand");
                        break;
                    }
                    continue;
                }
                if matches!(c, '+' | '*' | '%' | '=') || (c == '-' && possible_infix && left_value)
                {
                    pos += 1;
                    left_value = false;
                    possible_infix = !argument_list && open == '(';
                    continue;
                }
                if c == '-'
                    && !argument_list
                    && open == '('
                    && !left_value
                    && number_end(self.source, pos).is_none()
                {
                    pos += 1;
                    possible_infix = true;
                    continue;
                }
                let atom = matches!(c, '"' | '\'' | '$' | '@' | '[' | '(' | '<')
                    || c.is_ascii_digit()
                    || (matches!(c, '-' | '.') && number_end(self.source, pos).is_some());
                if atom || name_start(c) {
                    // Prefix-looking words may be fixed template labels, as
                    // in 检查 global: (3). Keep their literal reading even when
                    // the following text is not a valid reference name. Slot
                    // resolution still requires a complete reference later.
                    if !argument_list && name_start(c) && self.source[pos..].starts_with("global:")
                    {
                        pending.push((
                            bare_name_end(self.source, pos).unwrap(),
                            false,
                            false,
                            scanner.clone(),
                        ));
                    }
                    match scanner.atom_candidates(pos) {
                        Ok(candidates) => {
                            for candidate in candidates {
                                let mut next = scanner.clone();
                                next.comments = candidate.comments;
                                // A leading bare name can also be fixed
                                // template text. Preserve that reading until
                                // the complete template chooses between e.g.
                                // <score < limit> and <选择 <@ok> 1>.
                                if possible_infix
                                    && !left_value
                                    && bare_name_end(self.source, pos) == Some(candidate.end)
                                {
                                    pending.push((candidate.end, false, false, next.clone()));
                                }
                                pending.push((
                                    candidate.end,
                                    possible_infix && !left_value,
                                    true,
                                    next,
                                ));
                            }
                        }
                        Err(error) => failure = error,
                    }
                    break;
                }
                pos += c.len_utf8();
                possible_infix = false;
            }
        }
        results.sort_by_key(|candidate| candidate.end);
        results.dedup_by_key(|candidate| candidate.end);
        if let Some(index) = results
            .iter()
            .position(|candidate| self.ends_structural_group(start, candidate.end))
        {
            results.truncate(index + 1);
        }
        if results.is_empty() {
            Err(failure)
        } else {
            Ok(results)
        }
    }
    fn replace_comments(self) -> String {
        let mut bytes = self.source.as_bytes().to_vec();
        for (start, end) in self.comments {
            for byte in &mut bytes[start..end] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
        }
        String::from_utf8(bytes).expect("whole comment spans were replaced by ASCII whitespace")
    }
    fn clean(mut self) -> LexResult<String> {
        let mut pos = 0;
        let mut item_start = true;
        while let Some(c) = next_char(self.source, pos) {
            if item_start && self.starts(pos, "init") {
                let mut tail = pos + 4;
                while next_char(self.source, tail).is_some_and(|c| matches!(c, ' ' | '\t')) {
                    tail += 1;
                }
                if next_char(self.source, tail) == Some(':') {
                    let indentation = self.source[..pos].rsplit('\n').next().unwrap_or("").len();
                    pos = self.json_indented_body(tail + 1, indentation)?;
                    item_start = true;
                    continue;
                }
            }
            match c {
                '#' => {
                    let end = self.comment(pos)?;
                    if self.source[pos..end].contains(['\n', '\r']) {
                        item_start = true;
                    }
                    pos = end;
                    continue;
                }
                '\n' | '\r' => {
                    item_start = true;
                    pos += 1;
                    continue;
                }
                ' ' | '\t' => {
                    pos += 1;
                    continue;
                }
                _ => item_start = false,
            }
            match c {
                '"' => {
                    if let Some(end) = self.qualified(pos)? {
                        pos = end;
                    } else {
                        pos = string_end(self.source, pos)?;
                    }
                }
                '(' | '[' | '<' => pos = self.group(pos, false)?,
                '\'' => pos = self.menu(pos)?,
                c if name_start(c) => {
                    if let Some(end) = self.qualified(pos)? {
                        pos = end;
                    } else {
                        pos = bare_name_end(self.source, pos).unwrap();
                    }
                }
                _ => pos += c.len_utf8(),
            }
        }
        Ok(self.replace_comments())
    }
}

pub(super) fn physical_line_end(source: &str, start: usize) -> usize {
    source[start..]
        .find(['\n', '\r'])
        .map_or(source.len(), |offset| start + offset)
}

/// Scan a complete value at `start`, returning its exclusive UTF-8 byte offset.
/// Qualified namespace/opcode calls are indivisible. Callers validate slot types.
pub fn scan_atom(source: &str, start: usize) -> Result<usize> {
    scan_atom_candidates(source, start).map(|ends| *ends.last().expect("a valid atom has an end"))
}

/// A completed value followed by a suite colon ends the structural header.
/// Bare names can also be fixed template text, so a lexical alternative may
/// otherwise mistake the first comparison RHS for a nested boolean and scan
/// beyond the suite into later statements.
fn suite_boundary_after(source: &str, end: usize) -> bool {
    let tail = source[end..].trim_start_matches([' ', '\t']);
    tail.strip_prefix(':').is_some_and(|tail| {
        let tail = tail.trim_start_matches([' ', '\t']);
        tail.is_empty() || tail.starts_with(['\r', '\n', '#'])
    })
}

/// Scan the rest of a statement, including fixed text and further input
/// shells. A shorter comparison candidate leaves an unmatched `>` here and
/// cannot end a statement. Try one physical line first so ordinary adjacent
/// commands never require scanning the remainder of the document.
fn statement_tail(source: &str, start: usize) -> Option<(usize, bool, bool)> {
    fn scan(source: &str, start: usize) -> Option<(usize, bool, bool)> {
        let mut scanner = Scanner::new(source);
        let mut pos = start;
        let mut input = false;
        let mut fixed = false;
        while let Some(c) = next_char(source, pos) {
            match c {
                '\r' | '\n' => break,
                ' ' | '\t' => pos += 1,
                '#' => pos = scanner.comment(pos).ok()?,
                '(' | '<' => {
                    pos = scanner.group_candidates(pos, false).ok()?.last()?.end;
                    input = true;
                }
                '"' => {
                    pos = string_end(source, pos).ok()?;
                    input = true;
                }
                '\'' => {
                    pos = scanner.menu(pos).ok()?;
                    input = true;
                }
                ')' | '>' | '[' | ']' | '{' | '}' => return None,
                _ => {
                    fixed = true;
                    pos += c.len_utf8();
                }
            }
        }
        Some((pos, input, fixed))
    }

    let end = physical_line_end(source, start);
    scan(&source[..end], start)
        .or_else(|| (end < source.len()).then(|| scan(source, start)).flatten())
}

fn statement_boundary_after(source: &str, start: usize, end: usize) -> bool {
    let Some((pos, _, _)) = statement_tail(source, end) else {
        return false;
    };
    let mut scanner = Scanner::new(source);
    let Ok(next) = scanner.trivia(pos) else {
        return false;
    };
    if next == source.len() {
        return true;
    }
    let line = source[next..]
        .split(['\r', '\n'])
        .next()
        .unwrap_or("")
        .trim_end();
    if line.starts_with('[') || matches!(line, "pass" | "end") || assignment_parts(line).is_some() {
        return true;
    }
    let header = line.strip_suffix(':').map_or(line, str::trim_end);
    if !line.ends_with('>')
        && (declaration_header(header)
            || control_header(header)
            || branch_header(header)
            || keyword_rest(header, "start").is_some()
            || keyword_rest(header, "piece").is_some())
    {
        return true;
    }
    // The next command can start with an input and can itself have multiline
    // inputs. Continued comparison operands leave an unmatched outer closer;
    // a lone parenthesized operand has no fixed command text.
    let Some((line_end, input, fixed)) = statement_tail(source, next) else {
        return false;
    };
    if input && fixed && command_source(line) {
        return true;
    }
    // Zero-input commands at the statement's indentation are also siblings.
    // An indented bare operand remains available to a multiline comparison.
    let indentation = |pos| {
        source[..pos]
            .rsplit(['\r', '\n'])
            .next()
            .unwrap_or("")
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .count()
    };
    bare_name_end(line, 0) == Some(line.len())
        && indentation(next) <= indentation(start)
        && scanner
            .trivia(line_end)
            .is_ok_and(|after| next_char(source, after) != Some('>'))
}

fn is_control_header_group(source: &str, start: usize) -> bool {
    // Qualified expressions are templates: a colon after a nested input can
    // be literal text, including at a physical line break. Only a direct
    // control operand is eligible for the suite-boundary shortcut.
    let prefix = source[..start]
        .rsplit(['\r', '\n'])
        .next()
        .unwrap_or("")
        .trim();
    matches!(prefix, "if" | "elif" | "while" | "loop")
        && !qualified_source(source[start + 1..].trim_start())
}

/// Complete atom boundaries, sorted and deduplicated. Plain comparisons may
/// share a prefix with a closed boolean; callers matching slots must try every
/// boundary against their remaining template instead of swallowing a slot.
pub fn scan_atom_candidates(source: &str, start: usize) -> Result<Vec<usize>> {
    if start > source.len() || !source.is_char_boundary(start) {
        return Err(Error::new("atom offset is not a UTF-8 character boundary"));
    }
    Scanner::new(source)
        .atom_candidates(start)
        .map(|candidates| {
            candidates
                .into_iter()
                .map(|candidate| candidate.end)
                .collect()
        })
        .map_err(public_error)
}

/// Locate an infix operator within an ordinary `(...)` or `<...>` expression.
/// Arithmetic sides can be complete operator chains; comparison sides must be
/// single values. Editor callers may accept an unfinished right value, while
/// quoted menus stay intact and a plain `>` can close a boolean group. Explicit opcode argument
/// groups are not infix expressions and must be excluded by the caller.
pub fn infix_operator_at(
    source: &str,
    open: usize,
    position: usize,
    allow_incomplete: bool,
) -> Option<usize> {
    if open >= position || !source.is_char_boundary(open) || !source.is_char_boundary(position) {
        return None;
    }
    let open_char = next_char(source, open)?;
    if !matches!(open_char, '(' | '<') {
        return None;
    }
    if open_char == '('
        && next_char(source, position).is_some_and(|c| matches!(c, '+' | '-' | '*' | '/' | '%'))
    {
        if !arithmetic_fragment_complete(&source[open + 1..position]) {
            return None;
        }
        let end = position + 1;
        if let Ok(candidates) = scan_atom_candidates(source, open) {
            for close in candidates {
                if close > end + 1 && arithmetic_fragment_complete(&source[end..close - 1]) {
                    return Some(end);
                }
            }
        }
        return allow_incomplete
            .then(|| infix_right_end(source, open, position, true, None))
            .flatten();
    }
    let mut left = Scanner::new(&source[..position]);
    let left_start = left.trivia(open + 1).ok()?;
    let left_end = left.atom(left_start).ok()?;
    if left.trivia(left_end).ok()? != position {
        return None;
    }
    infix_right_end(source, open, position, allow_incomplete, None)
}

/// Check a mathematical fragment without resolving variable names or reporter
/// templates. This also works on a prefix ending at an editor cursor.
fn arithmetic_fragment_complete(source: &str) -> bool {
    let mut scanner = Scanner::new(source);
    let mut pos = 0;
    let mut needs_operand = true;
    loop {
        let Ok(next) = scanner.trivia(pos) else {
            return false;
        };
        pos = next;
        let Some(c) = next_char(source, pos) else {
            return !needs_operand;
        };
        if needs_operand {
            if c == '-' && number_end(source, pos).is_none() {
                pos += 1;
                continue;
            }
            match scanner
                .atom(pos)
                .ok()
                .or_else(|| bare_name_end(source, pos))
            {
                Some(end) => {
                    pos = end;
                    needs_operand = false;
                }
                None => return false,
            }
        } else if matches!(c, '+' | '-' | '*' | '/' | '%') {
            pos += 1;
            needs_operand = true;
        } else {
            return false;
        }
    }
}

fn infix_right_end(
    source: &str,
    open: usize,
    position: usize,
    allow_incomplete: bool,
    context: Option<&Scanner<'_>>,
) -> Option<usize> {
    let close = match next_char(source, open)? {
        '(' => ')',
        '<' => '>',
        _ => return None,
    };
    let tail = source.get(position..)?;
    if !tail.starts_with(['+', '-', '*', '/', '%', '=', '<', '>']) {
        return None;
    }
    if tail.starts_with(['<', '>']) && close != '>' {
        return None;
    }
    let end = position + 1;
    let mut right = context.cloned().unwrap_or_else(|| Scanner::new(source));
    let right_start = right.trivia(end).ok()?;
    if let Ok(candidates) = right.atom_candidates(right_start) {
        for candidate in candidates {
            if let Ok(after) = right.trivia(candidate.end)
                && next_char(source, after) == Some(close)
            {
                return Some(end);
            }
        }
    }
    if !allow_incomplete {
        return None;
    }
    let Some(first) = next_char(source, right_start) else {
        return (!tail.starts_with('>')).then_some(end);
    };
    if matches!(first, ')' | ']' | '>' | '{' | '}') {
        return None;
    }
    // Do not turn a close into an operator because a subsequent declaration
    // happens to start on another physical line.
    if source[end..right_start].contains(['\n', '\r'])
        && [
            "sprite", "stage", "var", "list", "global", "init", "piece", "def", "start", "import",
            "end",
        ]
        .iter()
        .any(|keyword| starts_keyword(&source[right_start..], keyword))
    {
        return None;
    }
    (matches!(first, '"' | '@' | '(' | '<' | '-' | '.')
        || first.is_ascii_digit()
        || name_start(first))
    .then_some(end)
}

/// Strip only DSL `#` comments, preserving byte length and every newline.
pub fn strip_comments(source: &str) -> Result<String> {
    Scanner::new(source).clean().map_err(public_error)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    File,
    Stage,
    Sprite,
}

pub(crate) fn keyword_rest<'a>(source: &'a str, keyword: &str) -> Option<&'a str> {
    let tail = source.strip_prefix(keyword)?;
    if tail.is_empty() || tail.chars().next().is_some_and(whitespace) {
        Some(tail.trim())
    } else {
        None
    }
}
fn data_header(header: &str) -> bool {
    ["var", "list", "global"]
        .iter()
        .any(|key| keyword_rest(header, key).is_some())
}
pub(crate) fn declaration_header(header: &str) -> bool {
    header.starts_with('@')
        || [
            "var", "list", "global", "def", "sprite", "stage", "init", "import",
        ]
        .iter()
        .any(|key| keyword_rest(header, key).is_some())
}
fn if_header(header: &str) -> bool {
    ["if", "elif"]
        .iter()
        .any(|key| keyword_rest(header, key).is_some())
}
fn branch_header(header: &str) -> bool {
    ["elif", "else"]
        .iter()
        .any(|key| keyword_rest(header, key).is_some())
}
fn starts_keyword(source: &str, keyword: &str) -> bool {
    source.strip_prefix(keyword).is_some_and(|tail| {
        tail.is_empty()
            || tail
                .chars()
                .next()
                .is_some_and(|c| whitespace(c) || matches!(c, '{' | ':'))
    })
}
fn control_header(header: &str) -> bool {
    ["if", "loop", "while"]
        .iter()
        .any(|key| keyword_rest(header, key).is_some())
}
fn obsolete_header(header: &str) -> bool {
    [
        "定义自制积木",
        "如果",
        "否则",
        "重复执行",
        "重复执行直到",
        "循环",
        "当绿旗被点击",
        "define",
        "repeat",
        "forever",
        "until",
    ]
    .iter()
    .any(|key| keyword_rest(header, key).is_some())
}
fn command_source(source: &str) -> bool {
    !source.is_empty() && !source.starts_with(['[', '@', '$'])
}

fn qualified_source(source: &str) -> bool {
    if source.starts_with('"') {
        string_end(source, 0)
            .is_ok_and(|end| source[end..].starts_with('.') || source[end..].starts_with("::"))
    } else {
        bare_namespace_end(source, 0).is_some()
    }
}
fn parse_literal(source: &str) -> Result<Value> {
    if source.starts_with('"') {
        return parse_string(source).map(Value::String);
    }
    let number = parse_number(source).ok_or_else(|| {
        Error::new("initializers must be finite numeric literals or quoted strings")
    })?;
    if !source.contains(['.', 'e', 'E']) {
        if let Ok(value) = source.parse::<i64>() {
            return Ok(Value::from(value));
        }
        if let Ok(value) = source.parse::<u64>() {
            return Ok(Value::from(value));
        }
    }
    Number::from_f64(number)
        .map(Value::Number)
        .ok_or_else(|| Error::new("initializers must be finite numeric literals or quoted strings"))
}
fn parse_list(source: &str) -> Result<Vec<Value>> {
    let mut pos = skip_space(source, 1);
    let mut values = Vec::new();
    if next_char(source, pos) != Some('}') {
        loop {
            let end = if next_char(source, pos) == Some('"') {
                string_end(source, pos).map_err(public_error)?
            } else {
                number_end(source, pos).ok_or_else(|| {
                    Error::new(
                        "list items must be numbers or quoted strings; nesting is not allowed",
                    )
                })?
            };
            values.push(parse_literal(&source[pos..end])?);
            pos = skip_space(source, end);
            match next_char(source, pos) {
                Some('}') => break,
                Some(',') => {
                    pos = skip_space(source, pos + 1);
                    if next_char(source, pos) == Some('}') {
                        return Err(Error::new(
                            "trailing comma is not allowed in list initializer",
                        ));
                    }
                }
                _ => {
                    return Err(Error::new("expected comma or } in list initializer"));
                }
            }
        }
    }
    if skip_space(source, pos + 1) != source.len() {
        return Err(Error::new("unexpected text after list initializer"));
    }
    Ok(values)
}

/// Parse one complete `.sbdsl` file into structural AST items.
pub fn parse(path: &Path, source: &str) -> Result<Vec<Item>> {
    let report = parse_recovering(path, source);
    match report.errors.into_iter().next() {
        Some(error) => Err(error),
        None => Ok(report.items),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(text: &str) -> Result<Vec<Item>> {
        parse(Path::new("test.sbdsl"), text)
    }

    #[test]
    fn single_quoted_menus_use_json_escapes_and_keep_literal_punctuation() {
        for (text, expected) in [
            (
                r#"'a\'b\\c\n\r\t\b\f\/\"\u4E2D\uD83D\uDE00'"#,
                "a'b\\c\n\r\t\u{8}\u{c}/\"中😀",
            ),
            ("''", ""),
            ("'/'", "/"),
            ("'*'", "*"),
            ("' a # b '", " a # b "),
            (r#"'"quoted"'"#, "\"quoted\""),
        ] {
            assert_eq!(parse_menu(text).unwrap(), expected, "{text}");
            assert_eq!(scan_atom(text, 0).unwrap(), text.len(), "{text}");
            assert_eq!(parse_menu(&format_menu(expected)).unwrap(), expected);
        }
        for value in ["", "'", "\\'", "\u{0}", "\t\n\r", "引号\"/😀"] {
            assert_eq!(parse_menu(&format_menu(value)).unwrap(), value);
        }
        assert_eq!(format_menu("空格"), "'空格'");
        let text = "start:\n 选择 'a#b/}<>;'\n 等待 (8 / 2) 秒 # comment\nend\n";
        let cleaned = strip_comments(text).unwrap();
        assert!(cleaned.contains("'a#b/}<>;'"));
        assert!(!cleaned.contains("# comment"));
        assert!(source(text).is_ok());
    }

    #[test]
    fn menus_reject_other_delimiters_bad_escapes_and_line_breaks() {
        for text in [
            "/'value'/",
            "'value",
            "'value'tail",
            r#"'\q'"#,
            "'a\nb'",
            "'a\tb'",
            "//",
            "/space/",
            "/*/",
            r#"/"space"/"#,
            "/'value' /",
            "/'value/",
            "/'value'",
            r#"'\x20'"#,
            r#"'\u123'"#,
            r#"'\uD800'"#,
            r#"'\uDC00'"#,
            "'a\rb'",
            "'a\\\nb'",
        ] {
            assert!(parse_menu(text).is_err(), "{text:?}");
        }
        // Ordinary DSL strings retain their existing, narrower escape rules.
        assert!(parse_string(r#""\t""#).is_err());
        assert!(parse_string(r#""\u4E2D""#).is_err());
    }

    #[test]
    fn plain_comparison_candidates_preserve_enclosing_boolean_boundaries() {
        let text = "<@flag > 1>";
        assert_eq!(
            scan_atom_candidates(text, 0).unwrap(),
            vec![text.find('>').unwrap() + 1, text.len()]
        );
        for text in [
            "<hp < 1>",
            "<score > 100>",
            "<@steps > 1>",
            "<<@ok> > 0>",
            "<<1 < 2> = <3 > 2>>",
            "<ext.模板 <@ok> (1)>",
            "<ext.模板 <@ok> <2 > 1>>",
            "ext::\"call\"(<@ok>, (1), <@steps > 1>)",
        ] {
            assert!(
                scan_atom_candidates(text, 0).unwrap().contains(&text.len()),
                "{text}"
            );
        }
        assert_eq!(infix_operator_at("<@flag>", 0, 6, true), None);
        assert_eq!(infix_operator_at("<@n > @>", 0, 4, true), Some(5));
        let text = "<<@flag> > 0>";
        assert_eq!(infix_operator_at(text, 1, 7, true), None);
        assert_eq!(infix_operator_at(text, 0, 9, false), Some(10));
    }

    #[test]
    fn nested_comparisons_reuse_scan_candidates_and_comment_spans() {
        for left_nested in [false, true] {
            let mut text = String::from("<1 > 0>");
            for _ in 0..24 {
                text = if left_nested {
                    format!("<{text} > # retained boundary\n0>")
                } else {
                    format!("<1 > # retained boundary\n{text}>")
                };
            }
            let mut scanner = Scanner::new(&text);
            let candidates = scanner.atom_candidates(0).unwrap();
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.end == text.len())
            );
            assert!(
                scanner.work.get() < 2_000,
                "left_nested={left_nested}: {} scan steps",
                scanner.work.get()
            );
            let cleaned = strip_comments(&text).unwrap();
            assert_eq!(cleaned.len(), text.len());
            assert_eq!(cleaned.matches('\n').count(), 24);
            assert!(!cleaned.contains("retained boundary"));
            assert!(
                scan_atom_candidates(&cleaned, 0)
                    .unwrap()
                    .contains(&cleaned.len())
            );
        }
    }

    #[test]
    fn expression_nesting_has_a_bounded_scan() {
        let text = format!("{}{}", "(".repeat(129), ")".repeat(129));
        let error = scan_atom(&text, 0).unwrap_err();
        assert!(error.to_string().contains("nesting exceeds"), "{error}");
        // A shorter boolean candidate must not hide a hard failure while
        // inspecting a potentially valid longer comparison.
        let text = format!("<@flag> {text}>");
        let error = scan_atom_candidates(&text, 0).unwrap_err();
        assert!(error.to_string().contains("nesting exceeds"), "{error}");
    }

    #[test]
    fn literals_are_strict_and_unicode_names_are_preserved() {
        assert_eq!(parse_string(r#""a\n\r\"\\""#).unwrap(), "a\n\r\"\\");
        assert!(parse_string(r#""\t""#).is_err());
        assert!(parse_string("\"a\nb\"").is_err());
        for number in ["0", "-90", ".5", "-.5", "1e-5", "2E+3"] {
            assert!(parse_number(number).is_some(), "{number}");
        }
        for number in ["+1", "1.", "NaN", "Infinity", "0x10", "1e", "1e999", " 1"] {
            assert!(parse_number(number).is_none(), "{number}");
        }
        assert_eq!(parse_name("玩家_１").unwrap(), "玩家_１");
        assert_eq!(parse_name(r#""a b""#).unwrap(), "a b");
        assert!(parse_name("a²").is_err());
        assert!(parse_name("1a").is_err());
    }

    #[test]
    fn comments_preserve_locations_and_literal_contents() {
        let text = "# line\n# inline #\n#\n跨行\n#\nstart:\n 说 (\"#not comment\") # tail\n 移到 ('a#b')\nend\n";
        let stripped = strip_comments(text).unwrap();
        assert_eq!(stripped.len(), text.len());
        assert_eq!(stripped.matches('\n').count(), text.matches('\n').count());
        assert!(stripped.contains("\"#not comment\""));
        assert!(stripped.contains("'a#b'"));
        let items = source(text).unwrap();
        let Item::Script { span, body, .. } = &items[0] else {
            panic!()
        };
        assert_eq!(span.line, 6);
        let Statement::Command { span, source } = &body[0] else {
            panic!()
        };
        assert_eq!((span.line, span.column), (7, 2));
        assert_eq!(source, "说 (\"#not comment\")");
        assert!(strip_comments("#\nnever closed").is_err());
    }

    #[test]
    fn atoms_keep_namespace_opcode_menus_and_comparisons_intact() {
        for atom in [
            r#"("自定义拓展".张量数量)"#,
            r#"<ext::"ready"()>"#,
            r#"(ext::"add"((1), 'a/b', <>))"#,
            "(自定义拓展.值)",
            "<自定义拓展.min.已就绪 ?>",
            "<自定义拓展.min::\"ready\"()>",
            "scratch-tensor-extension::\"done\"((1))",
            "((1)-(scratch-tensor-extension.值))",
            "((1.2)-(自定义拓展.min.值))",
            "((scratch-tensor-extension.值)-(自定义拓展.min.值))",
            "global:items",
            "<@布尔参数>",
            "<>",
            "''",
            "'*'",
            r#"'a/b'"#,
            "<hp < 1>",
            "<<a > 1> and <not <>>>",
            "(8 / 2)",
            "(8 / 4 / 2)",
        ] {
            assert_eq!(scan_atom(atom, 0).unwrap(), atom.len(), "{atom}");
        }
        assert_eq!(scan_atom("123 4", 0).unwrap(), 3);
        assert!(scan_atom("<(1 + 2]>", 0).is_err());
        assert_eq!(scan_atom("1.2", 0).unwrap(), 3);
        assert_eq!(scan_atom("1-(ext.值)", 0).unwrap(), 1);
        for name in ["", "1ext", ".ext", "-ext", "ext name", "ext/name"] {
            assert!(!is_bare_namespace(name), "{name}");
        }
    }

    #[test]
    fn bare_namespace_commands_are_structural_in_strict_and_recovering_parsers() {
        let source = "sprite \"测试\":\n    init:\n        pass\n    end\n    start:\n        scratch-tensor-extension.记录 (自定义拓展.min.值)\n        自定义拓展.min::\"run\"((1))\n    end\n    自定义拓展.min.完成 \"ok\":\n        说 (\"ok\")\n    end\nend\n";
        let strict = parse(Path::new("test.sbdsl"), source).unwrap();
        let recovering = parse_recovering(Path::new("test.sbdsl"), source);
        assert!(recovering.errors.is_empty(), "{:?}", recovering.errors);
        assert_eq!(format!("{strict:?}"), format!("{:?}", recovering.items));
    }

    #[test]
    fn shared_operator_recognition_preserves_menus_and_boolean_closers() {
        for source in [
            "(8 / 2)",
            "(8 / 4 / 2)",
            "<score > 100>",
            "<hp < 1>",
            "<score = 100>",
        ] {
            let position = source
                .find(['/', '>', '=', '<'])
                .filter(|position| *position > 0)
                .or_else(|| {
                    source[1..]
                        .find(['/', '>', '=', '<'])
                        .map(|position| position + 1)
                })
                .unwrap();
            let expected = position + 1;
            assert_eq!(
                infix_operator_at(source, 0, position, false),
                Some(expected),
                "{source}"
            );
        }
        for source in ["(8 / ", "(8 / (", "(8 /\n value", "(8 /"] {
            let position = source.find('/').unwrap();
            assert_eq!(
                infix_operator_at(source, 0, position, false),
                None,
                "{source}"
            );
            assert_eq!(
                infix_operator_at(source, 0, position, true),
                Some(position + 1),
                "{source}"
            );
        }
        for source in ["((8) 'a/b')", "((x) 'a/b')"] {
            let position = source.find('/').unwrap();
            assert_eq!(
                infix_operator_at(source, 0, position, true),
                None,
                "{source}"
            );
        }
        for source in ["<@ok > 1>", "<score > 100>", "<@ok>\nsprite \"a\":"] {
            let position = source.rfind('>').unwrap();
            assert_eq!(
                infix_operator_at(source, 0, position, true),
                None,
                "{source}"
            );
        }
    }

    #[test]
    fn operators_keep_nested_templates_commands_and_following_declarations() {
        let text = "start:\n 说 (<1 > 0>)\n 等待 (8 / 4 / 2) 秒\n ext::\"call\"(<@ok>, (1), '*', <hp < 1>)\n 说 <ext.模板 <@ok> (1)>\nend\nafter = 1\nimport next\n";
        let parsed = source(text).unwrap();
        let Item::Script { body, .. } = &parsed[0] else {
            panic!()
        };
        assert_eq!(body.len(), 4);
        assert!(matches!(&parsed[1], Item::Data { name, .. } if name == "after"));
        assert!(matches!(&parsed[2], Item::Import { module, .. } if module == "next"));
        assert!(
            parse_recovering(Path::new("test.sbdsl"), text)
                .errors
                .is_empty()
        );
    }

    #[test]
    fn parses_declarations_initializers_stage_and_sprite_add() {
        let items = source("import utils.math\na_b = -.5\nitems = {1, \"two\", 1e-5}\nstage:\n    score = 0\nend\nsprite \"玩家\":\n    init:\n        pass\n    end\n    global:全局列表 = {}\n    hp = 100\nend\nsprite \"玩家\" add:\n    pass\nend\n").unwrap();
        assert_eq!(items.len(), 6);
        let Item::Data { name, value, .. } = &items[1] else {
            panic!()
        };
        assert_eq!(name, "a_b");
        assert_eq!(value.as_f64(), Some(-0.5));
        assert!(matches!(&items[5], Item::Sprite { add: true, .. }));
        for invalid in [
            "var \"a b\" = 1",
            "\"items\" = {}",
            "x = (1 + 2)",
            "a = {1,}",
            "a = {{1}}",
            "a = {x}",
            "x = true",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn control_and_else_keep_headers_and_nested_statements() {
        let items = source("def 受伤 值: @n @warp:\n    if <hp < @n>:\n        说 (\"a\")\n    else:\n        loop 2:\n            移动 (8 / 2) 步\n        end\n    end\nend\nsprite \"玩家\":\n    init:\n        pass\n    end\n    start:\n        受伤 值: (3)\n    end\nend\n").unwrap();
        let Item::Procedure { header, body, .. } = &items[0] else {
            panic!()
        };
        assert_eq!(header, "受伤 值: @n @warp");
        let Statement::Control {
            else_body: Some(otherwise),
            ..
        } = &body[0]
        else {
            panic!()
        };
        assert!(matches!(&otherwise[0], Statement::Control { .. }));
    }

    #[test]
    fn strict_context_and_sibling_boundaries() {
        for invalid in [
            "移动 (1) 步",
            "loop 2:\n    pass",
            "sprite \"a\":\n    init:\n        pass\n    import x",
            "stage:\n    stage:\n        pass",
            "sprite \"a\":\n    init:\n        pass\n    sprite \"b\":\n        init:\n            pass",
            "start:\n    var x",
            "start:\n    当角色被点击:\n        pass",
            "var a var b",
            "start:\n    loop 2:\n        pass\n    else:\n        pass",
            "start:\n    [说 \"x\"",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
        assert!(
            source(
                "sprite \"a\":\n    init:\n        pass\n    end\n    start:\n        移动 (1) 步\n    end\nend"
            )
            .is_ok()
        );
        assert!(source("stage:\n    import utils.math\n    sprite \"a\":\n        init:\n            pass\n        end\n    end\nend\n").is_ok());
    }

    #[test]
    fn header_first_slot_and_explicit_commands_are_structural() {
        let items = source("sprite \"a\":\n    init:\n        pass\n    end\n    ext.(x 坐标) 达标:\n        ext::\"send\"((1), 'a/b', <true>)\n    end\n    ext::\"event\"('space'):\n        说 (\"x\")\n    end\nend\n").unwrap();
        let Item::Sprite { items, .. } = &items[0] else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        let Item::Script { header, .. } = &items[0] else {
            panic!()
        };
        assert_eq!(header, "ext.(x 坐标) 达标");
    }

    #[test]
    fn sprite_init_uses_json_escapes_and_preserves_dsl_comment_spans() {
        let text = r##"sprite "玩家":
    # Leading comments do not count as declarations.
    init: # inline comment #
        "x": -100,
        "y": 2e1,
        "visible": false,
        #
        A multiline DSL comment inside JSON.
        #
        "costumes": [{
            "name": "\u732b\ud83d\ude00\t\b\f\n\r\"\\\/",
            "path": "..\/assets\/a#b.svg",
            "rotationCenterX": 12.5,
            "bitmapResolution": 2
        }],
        "sounds": [{"name": "音效", "path": "../assets/sound.wav"}]
    end
    hp = 100
end
sprite "玩家" add:
    hp = 50
end
"##;
        let cleaned = strip_comments(text).unwrap();
        assert_eq!(cleaned.len(), text.len());
        assert_eq!(cleaned.matches('\n').count(), text.matches('\n').count());
        assert!(cleaned.contains("a#b.svg"));
        let items = source(text).unwrap();
        let Item::Sprite {
            init: Some(init),
            items: body,
            ..
        } = &items[0]
        else {
            panic!()
        };
        assert_eq!(
            (init.x, init.y, init.size, init.direction),
            (-100., 20., 100., 90.)
        );
        assert!(!init.visible);
        assert_eq!(init.rotation_style, "all around");
        assert_eq!(init.costumes[0].name, "猫😀\t\u{8}\u{c}\n\r\"\\/");
        assert_eq!(init.costumes[0].path, Path::new("../assets/a#b.svg"));
        let Item::Data { span, .. } = &body[0] else {
            panic!()
        };
        assert_eq!((span.line, span.column), (18, 5));
        assert!(matches!(
            &items[1],
            Item::Sprite {
                add: true,
                init: None,
                ..
            }
        ));

        let defaults = source("sprite \"a\":\n    init:\n        pass\n    end\nend").unwrap();
        let Item::Sprite {
            init: Some(init),
            items,
            ..
        } = &defaults[0]
        else {
            panic!()
        };
        assert_eq!(
            (init.x, init.y, init.size, init.direction),
            (0., 0., 100., 90.)
        );
        assert!(
            init.visible && init.costumes.is_empty() && init.sounds.is_empty() && items.is_empty()
        );
    }

    #[test]
    fn sprite_init_is_required_first_unique_and_creation_only() {
        for invalid in [
            "sprite \"a\":\n    pass",
            "sprite \"a\":\n    # only a comment\n",
            "sprite \"a\":\n    var x\n    init:\n        pass",
            "sprite \"a\":\n    init:\n        pass\n    init:\n        pass",
            "sprite \"a\" add:\n    init:\n        pass",
            "init:\n    pass",
            "stage:\n    init:\n        pass",
            "start:\n    init:\n        pass",
            "def p:\n    init:\n        pass",
            "sprite \"a\":\n    init ",
            "sprite \"a\":\n    init:\n        pass var x",
            "sprite \"a\":\n    init:\n        pass\n    start:\n        init:\n            pass",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
        assert!(source("sprite \"a\" add:\n    pass\nend").is_ok());
    }

    #[test]
    fn sprite_init_accepts_bare_fields_newline_separators_and_trailing_commas() {
        let text = r##"sprite "玩家":
 init:
  x: -100
  y: 2e1
  visible: false # initial state
  costumes: [
   {name: "猫", path: "assets/猫.svg",}
   {
    name: "狗"
    path: "assets/狗.svg"
    rotationCenterX: 12.5
   }
  ]
 end
 hp = 100
 start:
  hp = (
   100)
 end
end
"##;
        let items = source(text).unwrap();
        let Item::Sprite {
            init: Some(init),
            items,
            ..
        } = &items[0]
        else {
            panic!()
        };
        assert_eq!((init.x, init.y, init.visible), (-100., 20., false));
        assert_eq!(init.costumes.len(), 2);
        assert_eq!(init.costumes[1].rotation_center_x, Some(12.5));
        let Item::Script { body, .. } = &items[1] else {
            panic!()
        };
        assert!(
            matches!(&body[0], Statement::Command { source, .. } if source == "hp = (\n   100)")
        );
        assert!(
            parse_recovering(Path::new("test.sbdsl"), text)
                .errors
                .is_empty()
        );
    }

    #[test]
    fn init_notation_errors_keep_original_spans_and_recovery_children() {
        let text = "sprite \"角色 😀\":\n init:\n  x: 1\n  x: 2\n end\n hp = 100\nend\nimport next\n";
        let error = source(text).unwrap_err();
        assert!(error.0.contains("duplicate field"));
        assert_eq!(
            (error.span().unwrap().line, error.span().unwrap().column),
            (4, 3)
        );
        let report = parse_recovering(Path::new("test.sbdsl"), text);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        let Item::Sprite {
            init: None, items, ..
        } = &report.items[0]
        else {
            panic!()
        };
        assert!(matches!(&items[0], Item::Data { name, .. } if name == "hp"));
        assert!(matches!(&report.items[1], Item::Import { module, .. } if module == "next"));
    }

    #[test]
    fn sprite_init_rejects_invalid_json_schema_and_asset_metadata() {
        for json in [
            r#"{"x": .5}"#,
            r#"{"x": 01}"#,
            r#"{"x": +1}"#,
            r#"{"x": 1e999}"#,
            r#"{"x": hp}"#,
            r#"{"x": (1 + 2)}"#,
            r#"{"x": 1 "y": 2}"#,
            r#"{"x": 1, "x": 2}"#,
            r#"{"unknown": 1}"#,
            r#"{x-position: 1}"#,
            r#"{"visible": "true"}"#,
            r#"{"visible": <true>}"#,
            r#"{"size": 0}"#,
            r#"{"rotationStyle": "none"}"#,
            r#"{"rotationStyle": "\q"}"#,
            r#"{"costumes": [{"name":"x", "path":"a.svg", "name":"y"}]}"#,
            r#"{"costumes": [{"name":"x", "path":"a.svg", "unknown":1}]}"#,
            r#"{"costumes": [{"name":"x", "path":"a.svg"}, {"name":"x", "path":"b.svg"}]}"#,
            r#"{"sounds": [{"name":"x", "path":"a.wav"}, {"name":"x", "path":"b.wav"}]}"#,
            r#"{"costumes": [{"name":"", "path":"a.svg"}]}"#,
            r#"{"costumes": [{"name":"x", "path":""}]}"#,
            r#"{"costumes": [{"name":"x"}]}"#,
            r#"{"costumes": [{"name":"x", "path":"a.svg", "bitmapResolution":0}]}"#,
            r#"{"costumes": [{"name":"x", "path":"a.svg", "bitmapResolution":1.5}]}"#,
            r#"{"sounds": [{"name":"x", "path":"a.wav", "rotationCenterX":1}]}"#,
            r#"{"sounds": [{"name":"x", "path":"a.wav", "bitmapResolution":1}]}"#,
        ] {
            let text = format!("sprite \"a\":\n init:\n  {}", &json[1..json.len() - 1]);
            assert!(source(&text).is_err(), "{json}");
        }
        for field in [
            "x",
            "y",
            "size",
            "direction",
            "visible",
            "rotationStyle",
            "costumes",
            "sounds",
        ] {
            let text = format!("sprite \"a\":\n init:\n  \"{field}\": null");
            assert!(source(&text).is_err(), "null {field}");
        }
        for field in [
            "name",
            "path",
            "rotationCenterX",
            "rotationCenterY",
            "bitmapResolution",
        ] {
            let json = match field {
                "name" => r#"{"name":null,"path":"a.svg"}"#.to_string(),
                "path" => r#"{"name":"a","path":null}"#.to_string(),
                _ => format!(r#"{{"name":"a","path":"a.svg","{field}":null}}"#),
            };
            let text = format!("sprite \"a\":\n init:\n  \"costumes\": [{json}]");
            assert!(source(&text).is_err(), "null asset {field}");
        }
    }

    #[test]
    fn json_escape_rules_do_not_leak_into_dsl_and_json_errors_have_locations() {
        for invalid in [
            r#"x = "\t""#,
            "sprite \"a\":\n    init:\n        pass\n    x = \"\\u0061\"",
            "def p @init:\n    说 (\"\\t\")",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
        assert!(source("def p @init:\n    移到 ('#')\nend").is_ok());
        let error =
            source("sprite \"a\":\n init:\n  #中文注释\n  \"x\": null\n end\nend\n").unwrap_err();
        assert!(error.to_string().starts_with("test.sbdsl:4:"), "{error}");
        for invalid in [
            "sprite \"a\":\n    init:\n        rotationStyle: \"a\nb\"",
            "sprite \"a\":\n    init:\n        costumes: [}",
            "sprite \"a\":\n    init:\n        costumes: [",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn multiline_expressions_and_comment_columns_use_original_text() {
        let items = source("start:\n #中文# 说(连接\n (\"a\") 和 (\"b\"))\nend\n").unwrap();
        let Item::Script { body, .. } = &items[0] else {
            panic!()
        };
        let Statement::Command { span, source } = &body[0] else {
            panic!()
        };
        assert_eq!((span.line, span.column), (2, 7));
        assert!(source.contains('\n'));
        let error =
            super::parse(Path::new("例子.sbdsl"), "\nstart:\n 说 (\"\\q\")\nend").unwrap_err();
        assert!(error.to_string().starts_with("例子.sbdsl:3:"));
    }
}
