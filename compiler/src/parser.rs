//! Structural ScratchDSL parser and the lexical primitives shared by the registry.
//!
//! Templates are resolved later. This module preserves their text while checking
//! source structure, declaration contexts, comments and literal boundaries.
use crate::model::{Item, Span, SpriteInit, Statement};
use crate::{Error, Result};
use serde_json::{Number, Value};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::Path,
    rc::Rc,
};
use unicode_general_category::{GeneralCategory, get_general_category};

mod recovery;
pub use recovery::{ParseReport, parse_recovering};

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
fn whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
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

pub(crate) fn namespace_start(c: char) -> bool {
    name_start(c)
}

pub(crate) fn namespace_continue(c: char) -> bool {
    name_continue(c) || matches!(c, '.' | '-')
}

pub(crate) fn is_bare_namespace(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(namespace_start) && chars.all(namespace_continue)
}

/// Find the namespace before the final dot in an uninterrupted name, or before
/// an explicit opcode separator. Dots inside filenames are not qualification
/// levels. Requiring a letter/underscore first keeps numeric literals separate.
pub(crate) fn bare_namespace_end(source: &str, start: usize) -> Option<usize> {
    if !next_char(source, start).is_some_and(namespace_start) {
        return None;
    }
    let mut end = advance(source, start);
    while next_char(source, end).is_some_and(namespace_continue) {
        end = advance(source, end);
    }
    if source[end..].starts_with("::") {
        return Some(end);
    }
    source[start..end]
        .rfind('.')
        .filter(|offset| *offset > 0)
        .map(|offset| start + offset)
}
fn bare_name_end(source: &str, start: usize) -> Option<usize> {
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
    if !source
        .get(start..)
        .is_some_and(|tail| tail.starts_with("/'"))
    {
        return Err(lex_error(start, "menu values must use /'value'/"));
    }
    let mut pos = start + 2;
    let mut json = String::from("\"");
    let mut origins = vec![start + 1];
    while let Some(c) = next_char(source, pos) {
        match c {
            '\'' => {
                if next_char(source, pos + 1) != Some('/') {
                    return Err(lex_error(pos + 1, "menu quote must end immediately with /"));
                }
                json.push('"');
                origins.push(pos);
                let value = serde_json::from_str::<String>(&json).map_err(|error| {
                    let offset = origins
                        .get(error.column().saturating_sub(1))
                        .copied()
                        .unwrap_or(pos);
                    lex_error(offset, format!("invalid menu string: {error}"))
                })?;
                return Ok((pos + 2, value));
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
    Err(lex_error(start, "unterminated menu; expected '/"))
}

/// Decode a complete `/'value'/` menu. Bare and double-quoted menus are invalid.
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
    format!("/'{}'/", json[1..json.len() - 1].replace('\'', "\\'"))
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
    if source.is_empty() || number_end(source, 0)? != source.len() {
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

/// A scan records comment spans so stripping preserves byte offsets and line numbers.
#[derive(Clone)]
struct Scanner<'a> {
    cache: Rc<RefCell<GroupCache>>,
    work: Rc<Cell<usize>>,
    hard_error: Rc<RefCell<Option<LexError>>>,
    depth: usize,
    source: &'a str,
    comments: Vec<(usize, usize)>,
}

type GroupCache = BTreeMap<(usize, bool), LexResult<Vec<AtomCandidate>>>;

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
    /// Data-object syntax is confined to an @init object. In particular, / is
    /// not a menu delimiter and strings support JSON escapes in this region.
    fn json_object(&mut self, start: usize) -> LexResult<usize> {
        if next_char(self.source, start) != Some('{') {
            return Err(lex_error(start, "@init requires a data object"));
        }
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
                        return Err(lex_error(pos, "mismatched delimiter in @init object"));
                    }
                    pos += 1;
                    if closes.is_empty() {
                        return Ok(pos);
                    }
                }
                _ => pos += c.len_utf8(),
            }
        }
        Err(lex_error(start, "unclosed @init object; expected }"))
    }
    fn menu(&self, start: usize) -> LexResult<usize> {
        menu_token(self.source, start).map(|(end, _)| end)
    }
    fn symbol(&self, start: usize) -> LexResult<usize> {
        let mut pos = start + 1;
        if next_char(self.source, start) == Some('$') {
            for prefix in ["global:list:", "global:", "list:"] {
                if self.starts(pos, prefix) {
                    pos += prefix.len();
                    break;
                }
            }
        }
        name_end(self.source, pos)
    }
    /// Locate the opening group of a syntactically qualified call.
    fn qualified_open(&self, start: usize) -> LexResult<Option<(usize, bool)>> {
        let quoted = next_char(self.source, start) == Some('"');
        let name_end = if quoted {
            string_end(self.source, start)?
        } else if let Some(end) = bare_namespace_end(self.source, start) {
            end
        } else {
            return Ok(None);
        };
        let explicit = self.starts(name_end, "::");
        let open = if explicit {
            let opcode_start = name_end + 2;
            if next_char(self.source, opcode_start) != Some('"') {
                return Err(lex_error(
                    opcode_start,
                    "explicit local opcode must be double quoted",
                ));
            }
            let opcode_end = string_end(self.source, opcode_start)?;
            if !self.starts(opcode_end, ".") {
                return Err(lex_error(opcode_end, "expected . after explicit opcode"));
            }
            opcode_end + 1
        } else if self.starts(name_end, ".") {
            name_end + 1
        } else {
            return Ok(None);
        };
        if explicit && self.starts(open, "hat(") {
            return Ok(Some((open + 3, true)));
        }
        match next_char(self.source, open) {
            Some('[' | '(' | '<') => Ok(Some((open, explicit))),
            _ if explicit => Err(lex_error(
                open,
                "expected explicit opcode shape or hat(...)",
            )),
            // A qualified hat header is not a slot value; its whole header is read structurally.
            _ => Ok(None),
        }
    }
    fn qualified(&mut self, start: usize) -> LexResult<Option<usize>> {
        match self.qualified_open(start)? {
            Some((open, explicit)) => self.group(open, explicit).map(Some),
            None => Ok(None),
        }
    }
    fn atom_candidates(&mut self, start: usize) -> LexResult<Vec<AtomCandidate>> {
        match next_char(self.source, start) {
            Some('[' | '(' | '<') => return self.group_candidates(start, false),
            Some(c) if c == '"' || name_start(c) => {
                if let Some((open, explicit)) = self.qualified_open(start)? {
                    return self.group_candidates(open, explicit);
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
            Some('$' | '@') => self.symbol(start),
            Some('/') => self.menu(start),
            Some('[' | '(' | '<') => self.group(start, false),
            Some(c) if c.is_ascii_digit() || c == '-' || c == '.' => {
                let end = number_end(self.source, start)
                    .ok_or_else(|| lex_error(start, "invalid number"))?;
                if parse_number(&self.source[start..end]).is_none() {
                    return Err(lex_error(start, "numeric literal is not finite"));
                }
                Ok(end)
            }
            Some(c) if name_start(c) => self.qualified(start)?.ok_or_else(|| {
                lex_error(start, "bare text is not a slot value; use a quoted string")
            }),
            _ => Err(lex_error(start, "expected a complete slot value")),
        }
    }
    fn group(&mut self, start: usize, argument_list: bool) -> LexResult<usize> {
        let candidate = self
            .group_candidates(start, argument_list)?
            .pop()
            .expect("a successful scan has at least one candidate");
        self.comments = candidate.comments;
        Ok(candidate.end)
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
        let key = (start, argument_list);
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
        if self.depth >= 128 {
            return Err(self.complexity_error(start, "expression nesting exceeds 128 groups"));
        }
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
                    && infix_right_end_inner(self.source, start, pos, false, Some(&scanner))
                        .is_some();
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
                    if possible_infix
                        && left_value
                        && infix_right_end_inner(self.source, start, pos, false, Some(&scanner))
                            .is_some()
                    {
                        pos += 1;
                        left_value = false;
                        possible_infix = false;
                    } else {
                        match scanner.menu(pos) {
                            Ok(end) => pos = end,
                            Err(error) => {
                                failure = error;
                                break;
                            }
                        }
                        if left_value {
                            possible_infix = false;
                        }
                        left_value = true;
                    }
                    continue;
                }
                if matches!(c, '+' | '*' | '=')
                    || (c == '-' && possible_infix && left_value)
                    || (scanner.starts(pos, "mod") && possible_infix && left_value)
                {
                    pos += if scanner.starts(pos, "mod") { 3 } else { 1 };
                    left_value = false;
                    possible_infix = false;
                    continue;
                }
                let atom = matches!(c, '"' | '$' | '@' | '[' | '(' | '<')
                    || c.is_ascii_digit()
                    || (matches!(c, '-' | '.') && number_end(self.source, pos).is_some());
                let qualified = if name_start(c) {
                    match scanner.qualified_open(pos) {
                        Ok(open) => open.is_some(),
                        Err(error) => {
                            failure = error;
                            break;
                        }
                    }
                } else {
                    false
                };
                if atom || qualified {
                    match scanner.atom_candidates(pos) {
                        Ok(candidates) => {
                            for candidate in candidates {
                                let mut next = scanner.clone();
                                next.comments = candidate.comments;
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
                if name_start(c) {
                    pos = bare_name_end(self.source, pos).unwrap();
                } else {
                    pos += c.len_utf8();
                }
                possible_infix = false;
            }
        }
        results.sort_by_key(|candidate| candidate.end);
        results.dedup_by_key(|candidate| candidate.end);
        if results.is_empty() {
            Err(failure)
        } else {
            Ok(results)
        }
    }
    fn clean(mut self) -> LexResult<String> {
        let mut pos = 0;
        let mut item_start = true;
        while pos < self.source.len() {
            let c = next_char(self.source, pos).unwrap();
            if item_start && self.starts(pos, "@init") {
                let tail = pos + "@init".len();
                if next_char(self.source, tail)
                    .is_none_or(|c| whitespace(c) || matches!(c, '{' | '#'))
                {
                    let object = self.trivia(tail)?;
                    if next_char(self.source, object) == Some('{') {
                        pos = self.json_object(object)?;
                        item_start = false;
                        continue;
                    }
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
                '\n' | '\r' | '{' => {
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
                '/' => pos = self.menu(pos)?,
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
        let mut bytes = self.source.as_bytes().to_vec();
        for (start, end) in self.comments {
            for byte in &mut bytes[start..end] {
                if *byte != b'\n' && *byte != b'\r' {
                    *byte = b' ';
                }
            }
        }
        Ok(
            String::from_utf8(bytes)
                .expect("whole comment spans were replaced by ASCII whitespace"),
        )
    }
}

/// Scan a complete value at `start`, returning its exclusive UTF-8 byte offset.
/// Qualified namespace/opcode calls are indivisible. Callers validate slot types.
pub fn scan_atom(source: &str, start: usize) -> Result<usize> {
    scan_atom_candidates(source, start).map(|ends| *ends.last().expect("a valid atom has an end"))
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

/// Locate a binary operator within an ordinary `(...)` or `<...>` expression.
/// The left side must be exactly one complete value. Strict callers require a
/// complete right value followed by this group's close. Editor callers may
/// accept an unfinished right value, but a complete menu stays a menu and
/// a plain `>` can also close a boolean group. Explicit opcode argument
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
    if !matches!(next_char(source, open), Some('(' | '<')) {
        return None;
    }
    let mut left = Scanner::new(source.get(..position)?);
    let left_start = left.trivia(open + 1).ok()?;
    let left_end = left.atom(left_start).ok()?;
    if left.trivia(left_end).ok()? != position {
        return None;
    }
    infix_right_end(source, open, position, allow_incomplete)
}

fn infix_right_end(
    source: &str,
    open: usize,
    position: usize,
    allow_incomplete: bool,
) -> Option<usize> {
    infix_right_end_inner(source, open, position, allow_incomplete, None)
}

fn infix_right_end_inner(
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
    let length = if tail.starts_with("mod") {
        3
    } else if tail.starts_with(['+', '-', '*', '/', '=', '<', '>']) {
        if tail.as_bytes().get(1) == Some(&b'*') {
            return None;
        }
        1
    } else {
        return None;
    };
    if tail.starts_with(['<', '>']) && close != '>' {
        return None;
    }
    let end = position + length;
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
    if tail.starts_with('/') && right.menu(position).is_ok() {
        return None;
    }
    let Some(first) = next_char(source, right_start) else {
        return (!tail.starts_with('>')).then_some(end);
    };
    if matches!(first, ')' | ']' | '>' | '{' | '}') {
        return None;
    }
    // Do not turn a close into an operator because a subsequent declaration
    // happens to start with @ on another physical line.
    if source[end..right_start].contains(['\n', '\r'])
        && [
            "@sprite",
            "@stage",
            "@var",
            "@list",
            "@global",
            "@init",
            "import",
            "定义自制积木",
        ]
        .iter()
        .any(|keyword| starts_keyword(&source[right_start..], keyword))
    {
        return None;
    }
    (matches!(first, '"' | '$' | '@' | '(' | '<' | '-' | '.')
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

struct Parser<'a> {
    file: String,
    original: &'a str,
    source: String,
    pos: usize,
}
impl Parser<'_> {
    fn span(&self, pos: usize) -> Span {
        let end = self
            .original
            .get(pos..)
            .and_then(|s| s.chars().next())
            .map_or(pos, |c| pos + c.len_utf8());
        Span::from_source(Path::new(&self.file), self.original, pos, end)
    }
    fn range(&self, start: usize, end: usize) -> Span {
        // Headers are parsed from trimmed text. Exclude trailing whitespace and
        // stripped comments from their diagnostic range as well.
        let end = start + self.source[start..end].trim_end().len();
        Span::from_source(Path::new(&self.file), self.original, start, end)
    }
    fn error(&self, pos: usize, message: impl Into<String>) -> Error {
        Error::new(message).at(&self.span(pos))
    }
    fn skip_space(&mut self) {
        self.pos = skip_space(&self.source, self.pos);
    }
    fn separator(&mut self) -> Result<()> {
        let mut pos = self.pos;
        while matches!(next_char(&self.source, pos), Some(' ' | '\t')) {
            pos += 1;
        }
        match next_char(&self.source, pos) {
            None | Some('}' | '\n' | '\r') => {
                self.pos = pos;
                Ok(())
            }
            _ => Err(self.error(
                pos,
                "multiple sibling statements must be separated by a newline",
            )),
        }
    }
    /// Read a declaration/header/command until a body, sibling newline, or parent close.
    fn unit(&mut self) -> Result<(usize, String, bool)> {
        let start = self.pos;
        let mut pos = start;
        let mut scanner = Scanner::new(&self.source);
        while let Some(c) = next_char(&self.source, pos) {
            match c {
                '{' => {
                    let text = self.source[start..pos].trim().to_string();
                    self.pos = pos + 1;
                    return Ok((start, text, true));
                }
                '}' | '\n' | '\r' => break,
                '"' => {
                    pos = if let Some(end) = scanner
                        .qualified(pos)
                        .map_err(|e| self.error(e.offset, e.message))?
                    {
                        end
                    } else {
                        string_end(&self.source, pos)
                            .map_err(|e| self.error(e.offset, e.message))?
                    };
                }
                '(' | '[' | '<' => {
                    pos = scanner
                        .group(pos, false)
                        .map_err(|e| self.error(e.offset, e.message))?
                }
                '/' => {
                    pos = scanner
                        .menu(pos)
                        .map_err(|e| self.error(e.offset, e.message))?
                }
                c if name_start(c) => {
                    if let Some(end) = scanner
                        .qualified(pos)
                        .map_err(|e| self.error(e.offset, e.message))?
                    {
                        pos = end;
                    } else {
                        pos = bare_name_end(&self.source, pos).unwrap();
                    }
                }
                ')' | ']' | '>' => return Err(self.error(pos, "unmatched closing bracket")),
                _ => pos += c.len_utf8(),
            }
        }
        self.pos = pos;
        Ok((start, self.source[start..pos].trim().to_string(), false))
    }
    fn items(&mut self, context: Context, nested: bool) -> Result<Vec<Item>> {
        let mut result = Vec::new();
        loop {
            self.skip_space();
            match next_char(&self.source, self.pos) {
                None if nested => {
                    return Err(self.error(self.pos, "unclosed declaration body; expected }"));
                }
                None => return Ok(result),
                Some('}') if nested => {
                    self.pos += 1;
                    return Ok(result);
                }
                Some('}') => return Err(self.error(self.pos, "unexpected } at file top level")),
                _ => {}
            }
            let (start, header, body) = self.unit()?;
            let span = self.range(start, self.pos.saturating_sub(usize::from(body)));
            if let Some(rest) = keyword_rest(&header, "import") {
                if context == Context::Sprite {
                    return Err(self.error(start, "import is not allowed inside a sprite"));
                }
                if body {
                    return Err(self.error(start, "import does not have a body"));
                }
                if rest.is_empty()
                    || !rest
                        .split('.')
                        .all(|part| !part.starts_with('"') && parse_name(part).is_ok())
                {
                    return Err(
                        self.error(start, "import requires a dotted module name without .sbdsl")
                    );
                }
                if rest.ends_with(".sbdsl") {
                    return Err(self.error(start, "omit the .sbdsl extension in import"));
                }
                result.push(Item::Import {
                    module: rest.into(),
                    span,
                });
            } else if let Some(rest) = keyword_rest(&header, "@stage") {
                if context != Context::File {
                    return Err(
                        self.error(start, "@stage is only allowed directly at file top level")
                    );
                }
                if !rest.is_empty() || !body {
                    return Err(self.error(start, "expected @stage { ... }"));
                }
                result.push(Item::Stage(self.items(Context::Stage, true)?));
            } else if let Some(rest) = keyword_rest(&header, "@sprite") {
                if context == Context::Sprite {
                    return Err(self.error(
                        start,
                        "sprite declarations cannot be nested inside a sprite",
                    ));
                }
                if !body {
                    return Err(self.error(start, "sprite declaration needs a body"));
                }
                if !rest.starts_with('"') {
                    return Err(self.error(start, "sprite name must be double quoted"));
                }
                let name_end = string_end(rest, 0).map_err(|e| self.error(start, e.message))?;
                let name = parse_string(&rest[..name_end]).map_err(|e| self.error(start, e.0))?;
                let tail = rest[name_end..].trim();
                if !tail.is_empty() && tail != "add" {
                    return Err(self.error(start, "expected optional add after sprite name"));
                }
                result.push(Item::Sprite {
                    name,
                    add: tail == "add",
                    init: if tail == "add" {
                        None
                    } else {
                        Some(self.sprite_init()?)
                    },
                    items: self.items(Context::Sprite, true)?,
                    span,
                });
            } else if keyword_rest(&header, "@var").is_some()
                || keyword_rest(&header, "@list").is_some()
                || keyword_rest(&header, "@global").is_some()
            {
                if body {
                    return Err(self.error(start, "data declaration does not have a script body"));
                }
                result.push(self.data(&header, context, start)?);
            } else if let Some(rest) = keyword_rest(&header, "定义自制积木") {
                if !body || rest.is_empty() {
                    return Err(self.error(
                        start,
                        "procedure definition needs a nonempty header and a body",
                    ));
                }
                result.push(Item::Procedure {
                    header: rest.into(),
                    body: self.statements()?,
                    span,
                });
            } else {
                if keyword_rest(&header, "@init").is_some() {
                    return Err(self.error(
                        start,
                        "@init must appear exactly once as the first item of a new sprite; it is forbidden in add and other bodies",
                    ));
                }
                if header.starts_with('@') {
                    return Err(self.error(start, "unknown or misplaced declaration"));
                }
                if !body {
                    return Err(self.error(start, "only declarations and event scripts are allowed here; commands need a script body"));
                }
                if control_header(&header) || keyword_rest(&header, "否则").is_some() {
                    return Err(self.error(
                        start,
                        "control flow is only allowed inside an event or procedure body",
                    ));
                }
                if header.is_empty() {
                    return Err(self.error(start, "event body needs a hat header"));
                }
                result.push(Item::Script {
                    header,
                    body: self.statements()?,
                    span,
                });
            }
            self.separator()?;
        }
    }
    fn sprite_init(&mut self) -> Result<SpriteInit> {
        self.skip_space();
        let start = self.pos;
        if !starts_keyword(&self.source[start..], "@init") {
            return Err(self.error(start, "a new sprite must begin with @init { ... }"));
        }
        let (_, header, body) = self.unit()?;
        if header != "@init" || !body {
            return Err(self.error(start, "expected @init followed by a data object"));
        }
        let object = self.pos - 1;
        let end = Scanner::new(&self.source)
            .json_object(object)
            .map_err(|e| self.error(e.offset, e.message))?;
        let init: SpriteInit = crate::notation::parse_region(
            Path::new(&self.file),
            self.original,
            object..end,
            false,
        )?;
        init.validate()
            .map_err(|error| error.at(&self.span(start)))?;
        self.pos = end;
        self.separator()?;
        Ok(init)
    }
    fn data(&self, header: &str, context: Context, start: usize) -> Result<Item> {
        let (global, declaration) = if let Some(rest) = keyword_rest(header, "@global") {
            if context != Context::Sprite {
                return Err(self.error(
                    start,
                    "@global modifier is only allowed in a sprite declaration body",
                ));
            }
            (true, rest)
        } else {
            (false, header)
        };
        let (list, rest) = if let Some(rest) = keyword_rest(declaration, "@var") {
            (false, rest)
        } else if let Some(rest) = keyword_rest(declaration, "@list") {
            (true, rest)
        } else {
            return Err(self.error(start, "@global must precede @var or @list"));
        };
        let end = name_end(rest, 0).map_err(|e| self.error(start, e.message))?;
        let name = parse_name(&rest[..end]).map_err(|e| self.error(start, e.0))?;
        let tail = rest[end..].trim();
        let value = if tail.is_empty() {
            if list {
                Value::Array(vec![])
            } else {
                Value::from(0)
            }
        } else if let Some(initializer) = tail.strip_prefix('=') {
            let initializer = initializer.trim();
            if list {
                parse_list(initializer).map_err(|e| self.error(start, e.0))?
            } else {
                parse_literal(initializer).map_err(|e| self.error(start, e.0))?
            }
        } else {
            return Err(self.error(start, "expected = or end of data declaration"));
        };
        Ok(Item::Data {
            name,
            value,
            list,
            global,
            span: self.range(start, start + header.len()),
        })
    }
    fn statements(&mut self) -> Result<Vec<Statement>> {
        let mut result = Vec::new();
        loop {
            self.skip_space();
            match next_char(&self.source, self.pos) {
                None => return Err(self.error(self.pos, "unclosed script body; expected }")),
                Some('}') => {
                    self.pos += 1;
                    return Ok(result);
                }
                _ => {}
            }
            let (start, header, body) = self.unit()?;
            let span = self.range(start, self.pos.saturating_sub(usize::from(body)));
            if header.starts_with('@')
                || keyword_rest(&header, "import").is_some()
                || keyword_rest(&header, "定义自制积木").is_some()
            {
                return Err(self.error(
                    start,
                    "declarations and imports are not allowed inside a script body",
                ));
            }
            if body {
                if !control_header(&header) {
                    return Err(self.error(
                        start,
                        "only built-in control flow may introduce a nested script body",
                    ));
                }
                let then_body = self.statements()?;
                let after_body = self.pos;
                self.skip_space();
                let mut else_body = None;
                if starts_keyword(&self.source[self.pos..], "否则") {
                    if keyword_rest(&header, "如果").is_none() {
                        return Err(self.error(self.pos, "otherwise branch requires an if control"));
                    }
                    let else_start = self.pos;
                    let (_, otherwise, has_body) = self.unit()?;
                    if otherwise != "否则" || !has_body {
                        return Err(self.error(else_start, "expected 否则 { ... }"));
                    }
                    else_body = Some(self.statements()?);
                } else {
                    self.pos = after_body;
                }
                result.push(Statement::Control {
                    header,
                    body: then_body,
                    else_body,
                    span,
                });
            } else {
                if !command_source(&header) {
                    return Err(self.error(
                        start,
                        "expected a complete command block or control-flow body",
                    ));
                }
                result.push(Statement::Command {
                    source: header,
                    span,
                });
            }
            self.separator()?;
        }
    }
}

fn keyword_rest<'a>(source: &'a str, keyword: &str) -> Option<&'a str> {
    let tail = source.strip_prefix(keyword)?;
    if tail.is_empty() || tail.chars().next().is_some_and(whitespace) {
        Some(tail.trim())
    } else {
        None
    }
}
fn starts_keyword(source: &str, keyword: &str) -> bool {
    source.strip_prefix(keyword).is_some_and(|tail| {
        tail.is_empty()
            || tail
                .chars()
                .next()
                .is_some_and(|c| whitespace(c) || c == '{')
    })
}
fn control_header(header: &str) -> bool {
    ["如果", "重复执行", "重复执行直到", "循环"]
        .iter()
        .any(|key| keyword_rest(header, key).is_some())
}
fn command_source(source: &str) -> bool {
    if scan_atom(source, 0).ok() != Some(source.len()) {
        return false;
    }
    if source.starts_with('[') {
        return true;
    }
    let name_end = if source.starts_with('"') {
        string_end(source, 0).ok()
    } else {
        bare_namespace_end(source, 0)
    };
    let Some(name_end) = name_end else {
        return false;
    };
    if let Some(tail) = source[name_end..].strip_prefix('.') {
        return tail.starts_with('[');
    }
    if source[name_end..].starts_with("::") {
        let start = name_end + 2;
        let Ok(end) = string_end(source, start) else {
            return false;
        };
        return source[end..].starts_with(".[");
    }
    false
}
fn parse_literal(source: &str) -> Result<Value> {
    if source.starts_with('"') {
        return parse_string(source).map(Value::String);
    }
    if parse_number(source).is_some() && !source.contains(['.', 'e', 'E']) {
        if let Ok(value) = source.parse::<i64>() {
            return Ok(Value::from(value));
        }
        if let Ok(value) = source.parse::<u64>() {
            return Ok(Value::from(value));
        }
    }
    parse_number(source)
        .and_then(Number::from_f64)
        .map(Value::Number)
        .ok_or_else(|| Error::new("initializers must be finite numeric literals or quoted strings"))
}
fn parse_list(source: &str) -> Result<Value> {
    if !source.starts_with('[') {
        return Err(Error::new(
            "list initializer must be a one-dimensional literal array",
        ));
    }
    let mut pos = skip_space(source, 1);
    let mut values = Vec::new();
    if next_char(source, pos) == Some(']') {
        if skip_space(source, pos + 1) != source.len() {
            return Err(Error::new("unexpected text after list initializer"));
        }
        return Ok(Value::Array(values));
    }
    loop {
        let end = if next_char(source, pos) == Some('"') {
            string_end(source, pos).map_err(public_error)?
        } else {
            number_end(source, pos).ok_or_else(|| {
                Error::new("list items must be numbers or quoted strings; nesting is not allowed")
            })?
        };
        values.push(parse_literal(&source[pos..end])?);
        pos = skip_space(source, end);
        match next_char(source, pos) {
            Some(']') => {
                if skip_space(source, pos + 1) != source.len() {
                    return Err(Error::new("unexpected text after list initializer"));
                }
                return Ok(Value::Array(values));
            }
            Some(',') => {
                pos = skip_space(source, pos + 1);
                if next_char(source, pos) == Some(']') {
                    return Err(Error::new(
                        "trailing comma is not allowed in list initializer",
                    ));
                }
            }
            _ => return Err(Error::new("expected comma or ] in list initializer")),
        }
    }
}

/// Parse one complete `.sbdsl` file into structural AST items.
pub fn parse(path: &Path, source: &str) -> Result<Vec<Item>> {
    let file = path.display().to_string();
    let cleaned = Scanner::new(source).clean().map_err(|e| {
        let end = source
            .get(e.offset..)
            .and_then(|s| s.chars().next())
            .map_or(e.offset, |c| e.offset + c.len_utf8());
        Error::new(e.message).at_source(path, source, e.offset, end)
    })?;
    Parser {
        file,
        original: source,
        source: cleaned,
        pos: 0,
    }
    .items(Context::File, false)
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
            ("/''/", ""),
            ("/'/'/", "/"),
            ("/'*'/", "*"),
            ("/' a # b '/", " a # b "),
            (r#"/'"quoted"'/"#, "\"quoted\""),
            (
                r#"/'a\'b\\c\n\r\t\b\f\/\"\u4E2D\uD83D\uDE00'/"#,
                "a'b\\c\n\r\t\u{8}\u{c}/\"中😀",
            ),
        ] {
            assert_eq!(parse_menu(text).unwrap(), expected, "{text}");
            assert_eq!(scan_atom(text, 0).unwrap(), text.len(), "{text}");
            assert_eq!(parse_menu(&format_menu(expected)).unwrap(), expected);
        }
        for value in ["", "'", "\\'", "\u{0}", "\t\n\r", "引号\"/😀"] {
            assert_eq!(parse_menu(&format_menu(value)).unwrap(), value);
        }
        let text = "当绿旗被点击 {\n [选择 /'a#b/}<>;'/]\n [等待 (8 / 2) 秒] # comment\n}";
        let cleaned = strip_comments(text).unwrap();
        assert!(cleaned.contains("/'a#b/}<>;'/"));
        assert!(!cleaned.contains("# comment"));
        assert!(source(text).is_ok());
    }

    #[test]
    fn menus_reject_other_delimiters_bad_escapes_and_line_breaks() {
        for text in [
            "//",
            "/space/",
            "/*/",
            r#"/"space"/"#,
            "/'value' /",
            "/'value/",
            "/'value'",
            "/'value'/tail",
            r#"/'\q'/"#,
            r#"/'\x20'/"#,
            r#"/'\u123'/"#,
            r#"/'\uD800'/"#,
            r#"/'\uDC00'/"#,
            "/'a\nb'/",
            "/'a\rb'/",
            "/'a\tb'/",
            "/'a\\\nb'/",
        ] {
            assert!(parse_menu(text).is_err(), "{text:?}");
        }
        // Ordinary DSL strings retain their existing, narrower escape rules.
        assert!(parse_string(r#""\t""#).is_err());
        assert!(parse_string(r#""\u4E2D""#).is_err());
    }

    #[test]
    fn plain_comparison_candidates_preserve_enclosing_boolean_boundaries() {
        let text = "<@flag> 1>";
        assert_eq!(scan_atom_candidates(text, 0).unwrap(), vec![7, text.len()]);
        for text in [
            "<$hp < 1>",
            "<$score>100>",
            "<@steps > 1>",
            "<<@ok> > 0>",
            "<<1 < 2> = <3 > 2>>",
            "ext.<模板 <@ok> 1>",
            "ext.<模板 <@ok> <2 > 1>>",
            "ext::\"call\".[<@ok>, 1, <@steps > 1>]",
        ] {
            assert!(
                scan_atom_candidates(text, 0).unwrap().contains(&text.len()),
                "{text}"
            );
        }
        assert_eq!(infix_operator_at("<@flag>", 0, 6, true), None);
        assert_eq!(infix_operator_at("<@n > $>", 0, 4, true), Some(5));
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
        let text = "# line\n# inline #\n#\n跨行\n#\n当绿旗被点击 {\n [说 \"#not comment\"] # tail\n [移到 /'a#b'/]\n}\n";
        let stripped = strip_comments(text).unwrap();
        assert_eq!(stripped.len(), text.len());
        assert_eq!(stripped.matches('\n').count(), text.matches('\n').count());
        assert!(stripped.contains("\"#not comment\""));
        assert!(stripped.contains("/'a#b'/"));
        let items = source(text).unwrap();
        let Item::Script { span, body, .. } = &items[0] else {
            panic!()
        };
        assert_eq!(span.line, 6);
        let Statement::Command { span, source } = &body[0] else {
            panic!()
        };
        assert_eq!((span.line, span.column), (7, 2));
        assert_eq!(source, "[说 \"#not comment\"]");
        assert!(strip_comments("#\nnever closed").is_err());
    }

    #[test]
    fn atoms_keep_namespace_opcode_menus_and_comparisons_intact() {
        for atom in [
            r#""自定义拓展".(张量数量)"#,
            r#"ext::"ready".<>"#,
            r#"ext::"add".(1, /'a/b'/, <>)"#,
            "自定义拓展.(值)",
            "scratch-tensor-extension.[记录 自定义拓展.min.(值)]",
            "自定义拓展.min.<已就绪 ?>",
            "自定义拓展.min::\"ready\".<>",
            "scratch-tensor-extension::\"done\".hat(1)",
            "(1-scratch-tensor-extension.(值))",
            "(1.2-自定义拓展.min.(值))",
            "(scratch-tensor-extension.(值)-自定义拓展.min.(值))",
            "$global:list:\"a b\"",
            "<@布尔参数>",
            "<>",
            "/''/",
            "/'*'/",
            r#"/'a/b'/"#,
            "<$hp < 1>",
            "<<$a > 1> 与 <不成立 <>>>",
            "(8 / 2)",
            "(8 / (4 / 2))",
            "[选择 1 /'*'/]",
        ] {
            assert_eq!(scan_atom(atom, 0).unwrap(), atom.len(), "{atom}");
        }
        assert_eq!(scan_atom("123 4", 0).unwrap(), 3);
        assert!(scan_atom("<(1 + 2]>", 0).is_err());
        assert_eq!(scan_atom("1.2", 0).unwrap(), 3);
        assert_eq!(scan_atom("1-ext.(值)", 0).unwrap(), 1);
        for name in ["", "1ext", ".ext", "-ext", "ext name", "ext/name"] {
            assert!(!is_bare_namespace(name), "{name}");
        }
    }

    #[test]
    fn bare_namespace_commands_are_structural_in_strict_and_recovering_parsers() {
        let source = "@sprite \"测试\" {\n@init {}\n当绿旗被点击 {\nscratch-tensor-extension.[记录 自定义拓展.min.(值)]\n自定义拓展.min::\"run\".[1]\n}\n自定义拓展.min.完成 \"ok\" { [说 \"ok\"] }\n}\n";
        let strict = parse(Path::new("test.sbdsl"), source).unwrap();
        let recovering = parse_recovering(Path::new("test.sbdsl"), source);
        assert!(recovering.errors.is_empty(), "{:?}", recovering.errors);
        assert_eq!(format!("{strict:?}"), format!("{:?}", recovering.items));
    }

    #[test]
    fn shared_operator_recognition_preserves_menus_and_boolean_closers() {
        for source in [
            "(8/2)",
            "(8 / (4 / 2))",
            "<$score>100>",
            "<$hp<1>",
            "<$score=100>",
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
        for source in ["(8 / $", "(8 / (", "(8 /\n $value", "(8 /"] {
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
        for source in ["(8 /'*'/)", "(8 /'a#b'/)", "($x /'a/b'/)"] {
            let position = source.find('/').unwrap();
            assert_eq!(
                infix_operator_at(source, 0, position, true),
                None,
                "{source}"
            );
        }
        for source in ["<@ok> 1>", "<$score>100>", "<@ok>\n@sprite \"a\" {}"] {
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
        let text = "当绿旗被点击 {\n [说 <1 > 0>]\n [等待 (8 /\n  (4/2)) 秒]\n ext::\"call\".[<@ok>, 1, /'*'/, <$hp<1>]\n [说 ext.<模板 <@ok> 1>]\n}\n@var after = 1\nimport next\n";
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
        let items = source("import utils.math\n@var \"a b\" = -.5\n@list items = [1, \"two\", 1e-5]\n@stage { @var score }\n@sprite \"玩家\" {\n @init {}\n @global @list \"全局 列表\"\n @var hp = 100\n}\n@sprite \"玩家\" add {}\n").unwrap();
        assert_eq!(items.len(), 6);
        let Item::Data { name, value, .. } = &items[1] else {
            panic!()
        };
        assert_eq!(name, "a b");
        assert_eq!(value.as_f64(), Some(-0.5));
        let Item::Sprite { add, .. } = &items[5] else {
            panic!()
        };
        assert!(*add);
        for invalid in [
            "@var x = (1 + 2)",
            "@list a = [1,]",
            "@list a = [[1]]",
            "@list a = [$x]",
            "@var x = true",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn control_and_else_keep_headers_and_nested_statements() {
        let items = source("定义自制积木 受伤 值: @n @warp {\n 如果 <$hp < @n> 那么 { [说 \"a\"] }\n 否则 {\n  重复执行 2 次 { [移动 (8 / 2) 步] }\n }\n}\n@sprite \"玩家\" { @init {}\n当绿旗被点击 { [受伤 值: 3] } }\n").unwrap();
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
            "[移动 1 步]",
            "重复执行 {}",
            "@sprite \"a\" { @init {}\nimport x }",
            "@stage { @stage {} }",
            "@sprite \"a\" { @init {}\n@sprite \"b\" { @init {} } }",
            "当绿旗被点击 { @var x }",
            "当绿旗被点击 { 当角色被点击 {} }",
            "当绿旗被点击 { [移动 1 步] [移动 2 步] }",
            "@var a @var b",
            "当绿旗被点击 { 重复执行 {} 否则 {} }",
            "当绿旗被点击 { [说 \"x\"]",
            "@sprite \"a\" { @init {} } @sprite \"b\" { @init {} }",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
        assert!(source("@sprite \"a\" { @init {}\n当绿旗被点击 { [移动 1 步] } }").is_ok());
        assert!(source("@stage { import utils.math\n@sprite \"a\" { @init {} }\n}").is_ok());
    }

    #[test]
    fn header_first_slot_and_explicit_commands_are_structural() {
        let items = source("@sprite \"a\" {\n @init {}\n ext.(x 坐标) 达标 {\n  ext::\"send\".[1, /'a/b'/, <true>]\n }\n ext::\"event\".hat(/'space'/) { [说 \"x\"] }\n}").unwrap();
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
        let text = r##"@sprite "玩家" {
    # Leading comments do not count as declarations.
    @init # inline comment # {
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
    }
    @var hp = 100
}
@sprite "玩家" add { @var hp = 50 }
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

        let defaults = source("@sprite \"a\" { @init {} }").unwrap();
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
            "@sprite \"a\" {}",
            "@sprite \"a\" { # only a comment\n}",
            "@sprite \"a\" { @var x\n@init {} }",
            "@sprite \"a\" { @init {}\n@init {} }",
            "@sprite \"a\" add { @init {} }",
            "@init {}",
            "@stage { @init {} }",
            "当绿旗被点击 { @init {} }",
            "定义自制积木 p { @init {} }",
            "@sprite \"a\" { @init [] }",
            "@sprite \"a\" { @init {} @var x }",
            "@sprite \"a\" { @init {}\n当绿旗被点击 { @init {} } }",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
        assert!(source("@sprite \"a\" add {}").is_ok());
    }

    #[test]
    fn sprite_init_accepts_bare_fields_newline_separators_and_trailing_commas() {
        let text = r##"@sprite "玩家" {
 @init {
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
 }
 @var hp = 100
 当绿旗被点击 {
  [将 $hp
   设为 100]
 }
}
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
            matches!(&body[0], Statement::Command { source, .. } if source == "[将 $hp\n   设为 100]")
        );
        assert!(
            parse_recovering(Path::new("test.sbdsl"), text)
                .errors
                .is_empty()
        );
    }

    #[test]
    fn init_notation_errors_keep_original_spans_and_recovery_children() {
        let text =
            "@sprite \"角色 😀\" {\n @init {\n  x: 1\n  x: 2\n }\n @var hp = 100\n}\nimport next\n";
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
            r#"{"x": $hp}"#,
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
            let text = format!("@sprite \"a\" {{ @init {json} }}");
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
            let text = format!("@sprite \"a\" {{ @init {{\"{field}\":null}} }}");
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
            let text = format!("@sprite \"a\" {{ @init {{\"costumes\":[{json}]}} }}");
            assert!(source(&text).is_err(), "null asset {field}");
        }
    }

    #[test]
    fn json_escape_rules_do_not_leak_into_dsl_and_json_errors_have_locations() {
        for invalid in [
            r#"@var x = "\t""#,
            r#"@sprite "a" { @init {}
@var x = "\u0061" }"#,
            r#"定义自制积木 p @init { [说 "\t"] }"#,
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
        // A parameter named @init in a procedure header is not an init block.
        assert!(source("定义自制积木 p @init { [移到 /'#'/] }").is_ok());
        let error =
            source("@sprite \"a\" {\n @init {\n  #中文注释\n  \"x\": null\n }\n}").unwrap_err();
        assert!(error.to_string().starts_with("test.sbdsl:4:"), "{error}");
        for invalid in [
            "@sprite \"a\" { @init {\"rotationStyle\":\"a\nb\"} }",
            "@sprite \"a\" { @init {\"costumes\":[} }",
            "@sprite \"a\" { @init {\"costumes\":[",
        ] {
            assert!(source(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn multiline_expressions_and_comment_columns_use_original_text() {
        let items = source("当绿旗被点击 {\n#中文# [说 (连接\n \"a\" 和 \"b\")]\n}\n").unwrap();
        let Item::Script { body, .. } = &items[0] else {
            panic!()
        };
        let Statement::Command { span, source } = &body[0] else {
            panic!()
        };
        assert_eq!((span.line, span.column), (2, 6));
        assert!(source.contains('\n'));
        let error =
            super::parse(Path::new("例子.sbdsl"), "\n当绿旗被点击 { [说 \"\\q\"] }").unwrap_err();
        assert!(error.to_string().starts_with("例子.sbdsl:2:"));
    }
}
