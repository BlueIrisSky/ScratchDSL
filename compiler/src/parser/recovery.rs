//! Editor parsing uses the same lexical units and declaration validation as the
//! compiler. After a failed unit it synchronizes at a sibling or enclosing body
//! boundary, retaining the successfully parsed nodes on either side.
use super::*;

#[derive(Debug)]
pub struct ParseReport {
    pub items: Vec<Item>,
    pub errors: Vec<Error>,
}

/// Parse an editable document, retaining valid AST nodes after independent
/// syntax errors. Partial nodes are for analysis only; compilation uses `parse`.
pub fn parse_recovering(path: &Path, source: &str) -> ParseReport {
    // Keep the normal path identical to compilation, including unusual but
    // legal whitespace. Synchronization heuristics run only in broken files.
    if let Ok(items) = parse(path, source) {
        return ParseReport {
            items,
            errors: vec![],
        };
    }
    let (cleaned, errors) = clean_comments(path, source);
    let mut parser = RecoveringParser {
        parser: Parser {
            file: path.display().to_string(),
            original: source,
            source: cleaned,
            pos: 0,
        },
        eof_reported: errors
            .iter()
            .any(|error| error.0 == "unterminated block comment"),
        errors,
    };
    let items = parser.items(Context::File, None);
    parser
        .errors
        .sort_by_key(|error| error.span().map_or(0, |span| span.start));
    ParseReport {
        items,
        errors: parser.errors,
    }
}

/// Strip comments without replacing or reparsing source lines. Complete groups
/// use the strict scanner, so division and menus have exactly its boundaries.
/// For a broken group we walk its tokens individually, allowing later comments
/// to be recognized without treating hashes in malformed strings as comments.
fn clean_comments(path: &Path, source: &str) -> (String, Vec<Error>) {
    let mut scanner = Scanner::new(source);
    let mut pos = 0;
    let mut errors = Vec::new();
    let mut groups = Vec::new();
    while let Some(c) = next_char(source, pos) {
        match c {
            '#' => match scanner.comment(pos) {
                Ok(end) => pos = end,
                Err(error) => {
                    errors.push(Error::new(error.message).at_source(
                        path,
                        source,
                        error.offset,
                        error.offset + 1,
                    ));
                    scanner.comments.push((pos, source.len()));
                    break;
                }
            },
            // JSON strings are a superset of DSL strings. Validation remains
            // with unit()/sprite_init(); this pass only locates comment tokens.
            '"' => {
                pos =
                    json_string_end(source, pos).unwrap_or_else(|_| physical_line_end(source, pos));
            }
            '<' | '>'
                if groups.last().is_some_and(|(_, open)| {
                    infix_operator_at(source, *open, pos, true).is_some()
                }) =>
            {
                pos += 1;
            }
            '[' | '(' | '<' => {
                let comments = scanner.comments.len();
                match scanner.group(pos, false) {
                    Ok(end) => pos = end,
                    Err(_) => {
                        scanner.comments.truncate(comments);
                        groups.push((
                            match c {
                                '[' => ']',
                                '(' => ')',
                                _ => '>',
                            },
                            pos,
                        ));
                        pos += 1;
                    }
                }
            }
            '/' => {
                pos = groups
                    .last()
                    .and_then(|(_, open)| infix_operator_at(source, *open, pos, true))
                    .unwrap_or_else(|| {
                        scanner
                            .menu(pos)
                            .unwrap_or_else(|_| physical_line_end(source, pos))
                    });
            }
            ')' | ']' | '>' => {
                if groups.last().is_some_and(|(close, _)| *close == c) {
                    groups.pop();
                }
                pos += 1;
            }
            _ => pos += c.len_utf8(),
        }
    }
    let mut bytes = source.as_bytes().to_vec();
    for (start, end) in scanner.comments {
        for byte in &mut bytes[start..end] {
            if !matches!(*byte, b'\r' | b'\n') {
                *byte = b' ';
            }
        }
    }
    (
        String::from_utf8(bytes).expect("complete comment spans become ASCII spaces"),
        errors,
    )
}

fn physical_line_end(source: &str, start: usize) -> usize {
    source[start..]
        .find(['\n', '\r'])
        .map_or(source.len(), |offset| start + offset)
}

struct Unit {
    start: usize,
    header: String,
    body: bool,
    span: Span,
    closed: bool,
}

struct RecoveringParser<'a> {
    parser: Parser<'a>,
    errors: Vec<Error>,
    // One unfinished token at EOF must not produce an extra missing-brace
    // diagnostic for every enclosing control/script/sprite.
    eof_reported: bool,
}

impl RecoveringParser<'_> {
    fn push(&mut self, error: Error) {
        let duplicate = self.errors.iter().any(|previous| {
            previous.0 == error.0
                && previous.span().map(|span| span.start) == error.span().map(|span| span.start)
        });
        if !duplicate {
            self.errors.push(error);
        }
    }

    fn error(&mut self, pos: usize, message: impl Into<String>) {
        self.push(self.parser.error(pos, message));
    }

    fn unit(&mut self) -> Option<Unit> {
        let start = self.parser.pos;
        match self.parser.unit() {
            Ok((start, header, body)) => Some(Unit {
                start,
                header,
                body,
                span: self
                    .parser
                    .range(start, self.parser.pos.saturating_sub(usize::from(body))),
                closed: body && self.body_closed(self.parser.pos),
            }),
            Err(error) => {
                self.push(error);
                self.sync_unit(start);
                if self.parser.pos == self.parser.source.len() {
                    self.eof_reported = true;
                }
                None
            }
        }
    }

    fn separator(&mut self) {
        // An unterminated child may have returned at an untouched declaration
        // on the next line. Its newline was already consumed while looking for
        // the missing close, and it still separates the sibling correctly.
        if self.indentation(self.parser.pos).is_some() {
            return;
        }
        if let Err(error) = self.parser.separator() {
            self.push(error);
            self.sync_unit(self.parser.pos);
        }
    }

    /// A bad header's body is skipped as one unit. Its declarations must never
    /// be interpreted as children of the surrounding role or file.
    fn reject(&mut self, unit: &Unit, message: impl Into<String>) {
        self.error(unit.start, message);
        if unit.body {
            self.skip_body(unit.start);
        }
    }

    /// Synchronize a failed header/command. Lexically complete atoms can span
    /// lines; failed atoms end at the next physical sibling/body boundary.
    fn sync_unit(&mut self, start: usize) {
        let source = &self.parser.source;
        let mut pos = start;
        let mut expression_closes = Vec::new();
        while let Some(c) = next_char(source, pos) {
            match c {
                '\r' | '\n' | '}' => break,
                '{' if !expression_closes.is_empty() => pos += 1,
                '{' => {
                    self.parser.pos = pos + 1;
                    self.skip_body(start);
                    return;
                }
                '"' => {
                    pos = match json_string_end(source, pos) {
                        Ok(end) => end,
                        Err(_) => {
                            let end = physical_line_end(source, pos);
                            if source[start..].starts_with('@')
                                && let Some(brace) = source[pos..end].rfind('{')
                            {
                                self.parser.pos = pos + brace + 1;
                                self.skip_body(start);
                                return;
                            }
                            end
                        }
                    };
                }
                '/' => {
                    pos = Scanner::new(source)
                        .menu(pos)
                        .unwrap_or_else(|_| physical_line_end(source, pos));
                }
                '[' | '(' | '<' => {
                    pos = match Scanner::new(source).group(pos, false) {
                        Ok(end) => end,
                        Err(_) => {
                            expression_closes.push(match c {
                                '[' => ']',
                                '(' => ')',
                                '<' => '>',
                                _ => unreachable!(),
                            });
                            pos + 1
                        }
                    };
                }
                ')' | ']' | '>' => {
                    if expression_closes.last() == Some(&c) {
                        expression_closes.pop();
                    }
                    pos += 1;
                }
                _ => pos += c.len_utf8(),
            }
        }
        self.parser.pos = pos;
    }

    /// Walk balanced braces using string/menu boundaries. No AST is emitted
    /// from a rejected body, even when it contains otherwise legal declarations.
    fn skip_body(&mut self, header_start: usize) {
        let closed = self.body_closed(self.parser.pos);
        let source = &self.parser.source;
        let mut depth = 1;
        let mut pos = self.parser.pos;
        while let Some(c) = next_char(source, pos) {
            // If a damaged body never closes, a new outer declaration is a
            // usable boundary; do not swallow the rest of the project module.
            if !closed && depth == 1 && self.outer_declaration(pos, header_start) {
                break;
            }
            match c {
                '"' => {
                    pos = json_string_end(source, pos)
                        .unwrap_or_else(|_| physical_line_end(source, pos));
                }
                '/' => {
                    pos = Scanner::new(source).menu(pos).unwrap_or(pos + 1);
                }
                '[' | '(' | '<' => {
                    pos = Scanner::new(source).group(pos, false).unwrap_or(pos + 1);
                }
                '{' => {
                    depth += 1;
                    pos += 1;
                }
                '}' => {
                    depth -= 1;
                    pos += 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => pos += c.len_utf8(),
            }
        }
        self.parser.pos = pos;
        if pos == source.len() {
            self.eof_reported = true;
        }
    }

    fn body_closed(&self, start: usize) -> bool {
        let source = &self.parser.source;
        let mut depth = 1;
        let mut pos = start;
        while let Some(c) = next_char(source, pos) {
            match c {
                '"' => {
                    pos = json_string_end(source, pos)
                        .unwrap_or_else(|_| physical_line_end(source, pos));
                }
                '/' => {
                    pos = Scanner::new(source).menu(pos).unwrap_or(pos + 1);
                }
                '[' | '(' | '<' => {
                    pos = Scanner::new(source).group(pos, false).unwrap_or(pos + 1);
                }
                '{' => {
                    depth += 1;
                    pos += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                    pos += 1;
                }
                _ => pos += c.len_utf8(),
            }
        }
        false
    }

    fn indentation(&self, pos: usize) -> Option<usize> {
        let source = &self.parser.source;
        let line = source[..pos].rfind(['\n', '\r']).map_or(0, |i| i + 1);
        let prefix = &source[line..pos];
        prefix
            .chars()
            .all(|c| matches!(c, ' ' | '\t'))
            .then(|| prefix.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum())
    }

    fn outer_declaration(&self, pos: usize, header_start: usize) -> bool {
        let Some(indent) = self.indentation(pos) else {
            return false;
        };
        let Some(parent_indent) = self.indentation(header_start) else {
            return false;
        };
        if indent > parent_indent {
            return false;
        }
        let tail = &self.parser.source[pos..];
        ["@sprite", "@stage", "import", "定义自制积木"]
            .iter()
            .any(|keyword| starts_keyword(tail, keyword))
    }

    fn unclosed(&mut self, what: &str) {
        if self.parser.pos < self.parser.source.len() || !self.eof_reported {
            self.error(
                self.parser.pos,
                format!("unclosed {what} body; expected }}"),
            );
        }
        if self.parser.pos == self.parser.source.len() {
            self.eof_reported = true;
        }
    }

    fn items(&mut self, context: Context, parent: Option<(usize, bool)>) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            self.parser.skip_space();
            match next_char(&self.parser.source, self.parser.pos) {
                None => {
                    if parent.is_some() {
                        self.unclosed("declaration");
                    }
                    break;
                }
                Some('}') => {
                    self.parser.pos += 1;
                    if parent.is_some() {
                        break;
                    }
                    self.error(self.parser.pos - 1, "unexpected } at file top level");
                    continue;
                }
                _ => {}
            }
            if context == Context::Sprite
                && parent.is_some_and(|(start, closed)| {
                    !closed && self.outer_declaration(self.parser.pos, start)
                })
                && ["@sprite", "@stage", "import"]
                    .iter()
                    .any(|keyword| starts_keyword(&self.parser.source[self.parser.pos..], keyword))
            {
                self.unclosed("declaration");
                break;
            }
            if let Some(unit) = self.unit() {
                if let Some(item) = self.item(&unit, context) {
                    items.push(item);
                }
                self.separator();
            }
        }
        items
    }

    fn item(&mut self, unit: &Unit, context: Context) -> Option<Item> {
        let Unit {
            start,
            header,
            body,
            span,
            closed,
        } = unit;
        if let Some(rest) = keyword_rest(header, "import") {
            if context == Context::Sprite {
                self.reject(unit, "import is not allowed inside a sprite");
            } else if *body {
                self.reject(unit, "import does not have a body");
            } else if rest.is_empty()
                || !rest
                    .split('.')
                    .all(|part| !part.starts_with('"') && parse_name(part).is_ok())
            {
                self.reject(unit, "import requires a dotted module name without .sbdsl");
            } else if rest.ends_with(".sbdsl") {
                self.reject(unit, "omit the .sbdsl extension in import");
            } else {
                return Some(Item::Import {
                    module: rest.into(),
                    span: span.clone(),
                });
            }
        } else if let Some(rest) = keyword_rest(header, "@stage") {
            if context != Context::File {
                self.reject(unit, "@stage is only allowed directly at file top level");
            } else if !rest.is_empty() || !body {
                self.reject(unit, "expected @stage { ... }");
            } else {
                return Some(Item::Stage(
                    self.items(Context::Stage, Some((*start, *closed))),
                ));
            }
        } else if let Some(rest) = keyword_rest(header, "@sprite") {
            if context == Context::Sprite {
                self.reject(unit, "sprite declarations cannot be nested inside a sprite");
            } else if !body {
                self.reject(unit, "sprite declaration needs a body");
            } else if !rest.starts_with('"') {
                self.reject(unit, "sprite name must be double quoted");
            } else {
                let parsed_name = string_end(rest, 0).and_then(|end| {
                    parse_string(&rest[..end])
                        .map(|name| (name, rest[end..].trim()))
                        .map_err(|error| lex_error(*start, error.0))
                });
                match parsed_name {
                    Err(error) => self.reject(unit, error.message),
                    Ok((_, tail)) if !tail.is_empty() && tail != "add" => {
                        self.reject(unit, "expected optional add after sprite name");
                    }
                    Ok((name, tail)) => {
                        let add = tail == "add";
                        let init = if add { None } else { self.sprite_init() };
                        return Some(Item::Sprite {
                            name,
                            add,
                            init,
                            items: self.items(Context::Sprite, Some((*start, *closed))),
                            span: span.clone(),
                        });
                    }
                }
            }
        } else if ["@var", "@list", "@global"]
            .iter()
            .any(|keyword| keyword_rest(header, keyword).is_some())
        {
            if *body {
                self.reject(unit, "data declaration does not have a script body");
            } else {
                match self.parser.data(header, context, *start) {
                    Ok(item) => return Some(item),
                    Err(error) => self.push(error),
                }
            }
        } else if let Some(rest) = keyword_rest(header, "定义自制积木") {
            if !body || rest.is_empty() {
                self.reject(
                    unit,
                    "procedure definition needs a nonempty header and a body",
                );
            } else {
                return Some(Item::Procedure {
                    header: rest.into(),
                    body: self.statements(*start, *closed),
                    span: span.clone(),
                });
            }
        } else if keyword_rest(header, "@init").is_some() {
            self.reject(unit, "@init must appear exactly once as the first item of a new sprite; it is forbidden in add and other bodies");
        } else if header.starts_with('@') {
            self.reject(unit, "unknown or misplaced declaration");
        } else if !body {
            self.reject(
                unit,
                "only declarations and event scripts are allowed here; commands need a script body",
            );
        } else if control_header(header) || keyword_rest(header, "否则").is_some() {
            self.reject(
                unit,
                "control flow is only allowed inside an event or procedure body",
            );
        } else if header.is_empty() {
            self.reject(unit, "event body needs a hat header");
        } else {
            return Some(Item::Script {
                header: header.clone(),
                body: self.statements(*start, *closed),
                span: span.clone(),
            });
        }
        None
    }

    fn sprite_init(&mut self) -> Option<SpriteInit> {
        self.parser.skip_space();
        let start = self.parser.pos;
        match self.parser.sprite_init() {
            Ok(init) => Some(init),
            Err(error) => {
                self.push(error);
                self.parser.pos = start;
                if starts_keyword(&self.parser.source[start..], "@init")
                    && let Some(unit) = self.unit()
                    && unit.body
                {
                    self.skip_body(start);
                }
                None
            }
        }
    }

    fn statements(&mut self, parent: usize, closed: bool) -> Vec<Statement> {
        let mut statements = Vec::new();
        loop {
            self.parser.skip_space();
            match next_char(&self.parser.source, self.parser.pos) {
                None => {
                    self.unclosed("script");
                    break;
                }
                Some('}') => {
                    self.parser.pos += 1;
                    break;
                }
                _ => {}
            }
            if !closed && self.outer_declaration(self.parser.pos, parent) {
                self.unclosed("script");
                break;
            }
            if let Some(unit) = self.unit() {
                if let Some(statement) = self.statement(&unit) {
                    statements.push(statement);
                }
                self.separator();
            }
        }
        statements
    }

    fn statement(&mut self, unit: &Unit) -> Option<Statement> {
        let Unit {
            start,
            header,
            body,
            span,
            closed,
        } = unit;
        if header.starts_with('@')
            || ["import", "定义自制积木"]
                .iter()
                .any(|keyword| keyword_rest(header, keyword).is_some())
        {
            self.reject(
                unit,
                "declarations and imports are not allowed inside a script body",
            );
        } else if *body {
            if !control_header(header) {
                self.reject(
                    unit,
                    "only built-in control flow may introduce a nested script body",
                );
            } else {
                let then_body = self.statements(*start, *closed);
                let after_body = self.parser.pos;
                self.parser.skip_space();
                let mut else_body = None;
                if starts_keyword(&self.parser.source[self.parser.pos..], "否则") {
                    if let Some(otherwise) = self.unit() {
                        if keyword_rest(header, "如果").is_none() {
                            self.reject(&otherwise, "otherwise branch requires an if control");
                        } else if otherwise.header != "否则" || !otherwise.body {
                            self.reject(&otherwise, "expected 否则 { ... }");
                        } else {
                            else_body = Some(self.statements(otherwise.start, otherwise.closed));
                        }
                    }
                } else {
                    self.parser.pos = after_body;
                }
                return Some(Statement::Control {
                    header: header.clone(),
                    body: then_body,
                    else_body,
                    span: span.clone(),
                });
            }
        } else if !command_source(header) {
            self.reject(
                unit,
                "expected a complete command block or control-flow body",
            );
        } else {
            return Some(Statement::Command {
                source: header.clone(),
                span: span.clone(),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recover(source: &str) -> ParseReport {
        let report = parse_recovering(Path::new("编辑器.sbdsl"), source);
        for error in &report.errors {
            let span = error.span().expect("recovery errors have source locations");
            assert!(source.is_char_boundary(span.start), "{error}");
            assert!(source.is_char_boundary(span.end), "{error}");
            assert!(
                span.start <= span.end && span.end <= source.len(),
                "{error}"
            );
        }
        report
    }

    fn data_names(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Data { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn valid_recovery_ast_is_identical_to_strict_parser() {
        let source = r##"# Comment
import helpers.math
@list "名字 😀" = [1,
    "two", 3]
@stage {
 @var count = 1
}
@sprite "玩家" {
 @init {
  "costumes": [{"name": "a", "path": "a\u002fb#c.svg"}]
 }
 @var hp = 100
 定义自制积木 检查 @n <@ok> {
  如果 <@ok> 那么 {
   [说 (
     @n / 2
   )]
  } 否则 { [说 /'a/b#c'/] }
 }
 当绿旗被点击 { [检查 1 <>] }
}
"##;
        let strict = parse(Path::new("编辑器.sbdsl"), source).unwrap();
        let report = recover(source);
        assert!(report.errors.is_empty());
        assert_eq!(format!("{strict:?}"), format!("{:?}", report.items));
    }

    #[test]
    fn declaration_errors_keep_later_imports_and_data() {
        let source = "@var broken = true\n@list list = [1,]\n@var 正常 = 2\nimport utils.math\n@global @var bad\n@var later = 3\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
        assert_eq!(data_names(&report.items), ["正常", "later"]);
        assert!(matches!(&report.items[1], Item::Import { module, .. } if module == "utils.math"));
        assert_eq!(
            report
                .errors
                .iter()
                .map(|e| e.span().unwrap().line)
                .collect::<Vec<_>>(),
            [1, 2, 5]
        );
    }

    #[test]
    fn broken_tokens_preserve_later_commands_and_utf8_locations() {
        let source = "当绿旗被点击 {\n [说 \"中文 😀\n [等待 1 秒]\n [移到 /'鼠标\n [等待 2 秒]\n [说 (1 + 2]\n [等待 3 秒]\n}\nimport fine\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
        let Item::Script { body, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(body.len(), 3, "{body:?}");
        assert!(body.iter().all(
            |item| matches!(item, Statement::Command { source, .. } if source.starts_with("[等待"))
        ));
        assert!(matches!(&report.items[1], Item::Import { module, .. } if module == "fine"));
        assert_eq!(
            report.errors[0].span().unwrap().start,
            source.find("\n [等待").unwrap()
        );
    }

    #[test]
    fn unknown_and_misplaced_declaration_bodies_do_not_leak() {
        let source = "@unknown {\n@var hidden = 1\n@sprite \"hidden\" { @init {} }\n}\n@sprite \"real\" {\n @init {}\n@stage { @var hidden2 = 2 }\n@sprite \"nested\" { @init {}\n @global @var hidden3 = 3\n}\n @var visible = 4\n}\n@var outside = 5\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
        assert_eq!(report.items.len(), 2, "{:?}", report.items);
        let Item::Sprite { name, items, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(name, "real");
        assert_eq!(data_names(items), ["visible"]);
        assert_eq!(data_names(&report.items), ["outside"]);
    }

    #[test]
    fn invalid_or_missing_init_keeps_role_children_and_next_role() {
        let source = "@sprite \"bad-json\" {\n @init {\"x\": , \"y\": 1}\n @var a = 1\n}\n@sprite \"missing\" {\n @var b = 2\n}\n@sprite \"good\" { @init {}\n @var c = 3\n}\nimport after\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert_eq!(report.items.len(), 4);
        for (index, expected) in ["a", "b", "c"].iter().enumerate() {
            let Item::Sprite { init, items, .. } = &report.items[index] else {
                panic!()
            };
            assert_eq!(init.is_some(), index == 2);
            assert_eq!(data_names(items), [*expected]);
        }
    }

    #[test]
    fn nested_control_errors_keep_then_else_and_siblings() {
        let source = "当绿旗被点击 {\n 如果 <> 那么 {\n  @var illegal = 1\n  [等待 1 秒]\n } 否则 {\n  nonsense\n  [等待 2 秒]\n }\n [等待 3 秒]\n}\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        let Item::Script { body, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(body.len(), 2);
        let Statement::Control {
            body,
            else_body: Some(otherwise),
            ..
        } = &body[0]
        else {
            panic!()
        };
        assert_eq!(body.len(), 1);
        assert_eq!(otherwise.len(), 1);
    }

    #[test]
    fn unfinished_token_at_eof_reports_only_its_root_cause() {
        for source in [
            "当绿旗被点击 { [说 \"unfinished",
            "当绿旗被点击 { 如果 <> 那么 { [移到 /unfinished",
            "当绿旗被点击 { 如果 <> 那么 { [等待 (1 + 2",
            "当绿旗被点击 {\n #\n unfinished comment",
        ] {
            let report = recover(source);
            assert_eq!(report.errors.len(), 1, "{source}\n{:?}", report.errors);
        }
    }

    #[test]
    fn missing_body_close_recovers_at_later_outer_declaration() {
        let source = "当绿旗被点击 {\n [等待 1 秒]\n@sprite \"later\" {\n @init {}\n @var hp = 1\n}\nimport after\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(report.items.len(), 3, "{:?}", report.items);
        assert!(matches!(&report.items[1], Item::Sprite { name, .. } if name == "later"));
        assert!(matches!(&report.items[2], Item::Import { module, .. } if module == "after"));
    }

    #[test]
    fn comments_and_multiline_atoms_survive_unrelated_syntax_errors() {
        let source = r##"@var invalid = true
# @var hidden = 1
@list values = [1,
 # invisible list comment
 2]
当绿旗被点击 {
 [说 "# literal"]
 [说 /'a#b'/]
 [等待 (8 /
   # division comment
   2) 秒]
}
@sprite "json" {
 @init {"costumes": [{"name": "x", "path": "a\u002fb#c.svg"}]}
}
"##;
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(data_names(&report.items), ["values"]);
        let Item::Script { body, .. } = &report.items[1] else {
            panic!()
        };
        assert_eq!(body.len(), 3);
        let Item::Sprite {
            init: Some(init), ..
        } = &report.items[2]
        else {
            panic!()
        };
        assert_eq!(init.costumes[0].path, Path::new("a/b#c.svg"));
    }

    #[test]
    fn malformed_header_skips_its_body_but_not_the_next_role() {
        let source = "@sprite bare {\n @init {}\n @global @var hidden = 1\n}\n@sprite \"visible\" { @init {} }\n@var after = 2\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(report.items.len(), 2);
        assert!(matches!(&report.items[0], Item::Sprite { name, .. } if name == "visible"));
        assert_eq!(data_names(&report.items), ["after"]);
    }

    #[test]
    fn malformed_single_quoted_menus_keep_following_commands_and_declarations() {
        let source = "当绿旗被点击 {\n [选择 /'bad\\q#value'/]\n [等待 1 秒]\n [选择 /'unfinished#\n [等待 2 秒]\n [选择 /'ok#}/'/]\n}\n@var after = 1\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        let Item::Script { body, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(body.len(), 3);
        assert!(
            matches!(&body[2], Statement::Command { source, .. } if source == "[选择 /'ok#}/'/]")
        );
        assert_eq!(data_names(&report.items), ["after"]);
    }

    #[test]
    fn rejected_body_ignores_braces_in_menus_following_division() {
        let source = "@sprite invalid {\n @init {}\n 当绿旗被点击 {\n  [something (4 / 2) /'}'/]\n }\n @var leaked = 1\n}\n@var safe = 2\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(data_names(&report.items), ["safe"]);
    }

    #[test]
    fn brace_in_broken_expression_is_not_a_script_body() {
        let source = "当绿旗被点击 {\n [说 {]\n [等待 1 秒]\n}\n@var after = 1\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        let Item::Script { body, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(body.len(), 1);
        assert!(matches!(&body[0], Statement::Command { source, .. } if source == "[等待 1 秒]"));
        assert_eq!(data_names(&report.items), ["after"]);
    }

    #[test]
    fn unfinished_declaration_string_does_not_expose_role_contents() {
        let source =
            "@sprite \"broken {\n @init {}\n @var hidden = 1\n}\n@sprite \"safe\" { @init {} }\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(data_names(&report.items).is_empty());
        assert!(matches!(&report.items[0], Item::Sprite { name, .. } if name == "safe"));
    }

    #[test]
    fn every_editing_prefix_terminates_with_valid_original_spans() {
        let source = "@sprite \"角色😀\" {\n @init {\"x\": 10}\n @list logs = [1, \"二\"]\n 定义自制积木 调用 @n {\n  如果 <@n < 2> 那么 {\n   [说 /'a#b'/]\n  } 否则 { [等待 (4 / 2) 秒] }\n }\n}\nimport helper\n";
        for (end, _) in source
            .char_indices()
            .chain(std::iter::once((source.len(), '\0')))
        {
            recover(&source[..end]);
        }
    }
}
