//! Indentation is structural: source text is never rewritten, so editor and
//! compiler diagnostics continue to use the user's original byte positions.
use super::*;

#[derive(Debug)]
pub struct ParseReport {
    pub items: Vec<Item>,
    pub errors: Vec<Error>,
}

struct Unit {
    start: usize,
    end: usize,
    indent: usize,
    header: String,
    body: bool,
}

/// Parse an editable document, retaining valid nodes around independent errors.
pub fn parse_recovering(path: &Path, source: &str) -> ParseReport {
    let (cleaned, errors) = clean_comments(path, source);
    let mut parser = IndentedParser {
        path,
        original: source,
        source: cleaned,
        pos: 0,
        errors,
    };
    let items = parser.items(Context::File, 0);
    parser
        .errors
        .sort_by_key(|error| error.span().map_or(0, |span| span.start));
    ParseReport {
        items,
        errors: parser.errors,
    }
}

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
            // with read_unit()/sprite_init(); this pass only locates comment tokens.
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
            '\'' => {
                pos = scanner
                    .menu(pos)
                    .unwrap_or_else(|_| physical_line_end(source, pos));
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
    (scanner.replace_comments(), errors)
}

struct IndentedParser<'a> {
    path: &'a Path,
    original: &'a str,
    source: String,
    pos: usize,
    errors: Vec<Error>,
}

impl IndentedParser<'_> {
    fn span_at(&self, pos: usize) -> Span {
        Span::from_source(self.path, self.original, pos, advance(self.original, pos))
    }

    fn range(&self, start: usize, end: usize) -> Span {
        // Headers are parsed from trimmed text. Exclude trailing whitespace and
        // stripped comments from their diagnostic range as well.
        let end = start + self.source[start..end].trim_end().len();
        Span::from_source(self.path, self.original, start, end)
    }

    fn make_error(&self, pos: usize, message: impl Into<String>) -> Error {
        Error::new(message).at(&self.span_at(pos))
    }

    fn error(&mut self, pos: usize, message: impl Into<String>) {
        self.errors.push(self.make_error(pos, message));
    }

    fn data(&self, header: &str, assignment: Assignment<'_>, start: usize) -> Result<Item> {
        if assignment.operator != "=" {
            return Err(self.make_error(start, "data initializer requires ="));
        }
        let (name, global) =
            parse_reference(assignment.target).map_err(|e| self.make_error(start, e.0))?;
        let list = assignment.value.starts_with('{');
        let value = if list {
            parse_list(assignment.value).map(Value::Array)
        } else {
            parse_literal(assignment.value)
        }
        .map_err(|e| self.make_error(start, e.0))?;
        Ok(Item::Data {
            name,
            value,
            list,
            global,
            span: self.range(start, start + header.len()),
        })
    }

    fn peek(&mut self) -> Option<(usize, usize)> {
        self.pos = skip_space(&self.source, self.pos);
        let pos = self.pos;
        if pos == self.source.len() {
            return None;
        }
        let line = self.source[..pos]
            .rfind(['\r', '\n'])
            .map_or(0, |offset| offset + 1);
        let indent = self.original[line..pos]
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .fold(0, |column, c| {
                if c == '\t' {
                    (column / 4 + 1) * 4
                } else {
                    column + 1
                }
            });
        Some((pos, indent))
    }

    fn span(&self, unit: &Unit) -> Span {
        self.range(unit.start, unit.end)
    }

    fn read_unit(&mut self, indent: usize) -> Option<Unit> {
        let start = self.pos;
        let mut pos = start;
        let mut scanner = Scanner::new(&self.source);
        let scanned: LexResult<usize> = (|| {
            while let Some(c) = next_char(&self.source, pos) {
                match c {
                    '\r' | '\n' => break,
                    '"' => {
                        pos = if let Some(end) = scanner.qualified(pos)? {
                            end
                        } else {
                            string_end(&self.source, pos)?
                        };
                    }
                    '\'' => pos = scanner.menu(pos)?,
                    '[' => {
                        return Err(lex_error(
                            pos,
                            "commands no longer use []; write block text directly",
                        ));
                    }
                    '(' | '<' => {
                        // A template can start with its first input. There is
                        // no preceding command text for Scanner::group to use
                        // when selecting the statement boundary.
                        if pos == start {
                            scanner.structural_group = Some((pos, StructuralBoundary::Statement));
                        }
                        pos = scanner.group(pos, false)?;
                    }
                    '{' if assignment_parts(self.source[start..pos].trim()).is_some() => {
                        pos = scanner.json_object(pos)?;
                    }
                    '{' => {
                        return Err(lex_error(
                            pos,
                            "blocks use : and indentation; braces are only for list literals",
                        ));
                    }
                    '}' | ']' | ')' | '>' => {
                        return Err(lex_error(pos, "unmatched closing bracket"));
                    }
                    c if name_start(c) => {
                        pos = scanner
                            .qualified(pos)?
                            .unwrap_or_else(|| bare_name_end(&self.source, pos).unwrap());
                    }
                    _ => pos += c.len_utf8(),
                }
            }
            Ok(pos)
        })();
        match scanned {
            Ok(end) => {
                self.pos = end;
                let text = self.source[start..end].trim_end();
                let body = text.ends_with(':');
                let header = if body { &text[..text.len() - 1] } else { text }.trim_end();
                Some(Unit {
                    start,
                    end: start + header.len(),
                    indent,
                    header: header.into(),
                    body,
                })
            }
            Err(error) => {
                self.error(error.offset, error.message);
                self.pos = physical_line_end(&self.source, start);
                self.skip_children(indent);
                None
            }
        }
    }

    fn body_indent(&mut self, unit: &Unit) -> Option<usize> {
        match self.peek() {
            Some((_, indent)) if indent > unit.indent => Some(indent),
            _ => None,
        }
    }

    /// Collect declaration members until their matching `end`. Unlike a
    /// statement body, a member list may contain data, procedures and scripts.
    fn items_until_end(&mut self, context: Context, indent: usize, header: &Unit) -> Vec<Item> {
        let mut items = Vec::new();
        while let Some(unit) = self.next_at(indent) {
            if unit.header == "end" && !unit.body {
                return items;
            }
            if let Some(item) = self.item(&unit, context) {
                items.push(item);
            }
        }
        self.error(
            header.start,
            "missing end; close this block with end at the same indentation",
        );
        items
    }

    fn skip_children(&mut self, parent_indent: usize) {
        while let Some((start, indent)) = self.peek() {
            if indent <= parent_indent {
                break;
            }
            self.pos = physical_line_end(&self.source, start);
        }
    }

    fn line_text(&self, start: usize) -> &str {
        self.source[start..physical_line_end(&self.source, start)].trim()
    }

    /// A block body is optional when the matching `end` follows immediately.
    fn block_body(&mut self, unit: &Unit) -> Vec<Statement> {
        match self.peek() {
            Some((_, indent)) if indent > unit.indent => self.statements(indent),
            _ => Vec::new(),
        }
    }

    /// Consume the mandatory `end` for an event, definition, piece or C-block.
    fn expect_end(&mut self, unit: &Unit) {
        if let Some((start, indent)) = self.peek()
            && indent == unit.indent
            && self.line_text(start) == "end"
        {
            self.pos = physical_line_end(&self.source, start);
            return;
        }
        self.error(
            unit.start,
            "missing end; close this block with end at the same indentation",
        );
    }

    fn reject_stray_end(&mut self, unit: &Unit) -> bool {
        if keyword_rest(&unit.header, "end").is_none() {
            return false;
        }
        if unit.header == "end" && !unit.body {
            self.reject(unit, "unexpected end; no enclosing block to close");
        } else {
            self.reject(unit, "end must be written alone on its own line");
        }
        true
    }

    fn reject(&mut self, unit: &Unit, message: impl Into<String>) {
        self.error(unit.start, message);
        self.skip_children(unit.indent);
    }

    fn next_at(&mut self, indent: usize) -> Option<Unit> {
        loop {
            let (start, actual) = self.peek()?;
            if actual < indent {
                return None;
            }
            if actual > indent {
                self.error(
                    start,
                    "unexpected indentation; dedent must match an enclosing block",
                );
                self.pos = physical_line_end(&self.source, start);
                self.skip_children(indent);
                continue;
            }
            if let Some(unit) = self.read_unit(indent) {
                return Some(unit);
            }
        }
    }

    fn items(&mut self, context: Context, indent: usize) -> Vec<Item> {
        let mut items = Vec::new();
        while let Some(unit) = self.next_at(indent) {
            if let Some(item) = self.item(&unit, context) {
                items.push(item);
            }
        }
        items
    }

    fn item(&mut self, unit: &Unit, context: Context) -> Option<Item> {
        let header = &unit.header;
        let span = self.span(unit);
        if header == "pass" && !unit.body {
            return None;
        }
        if self.reject_stray_end(unit) {
            return None;
        }
        if obsolete_header(header) {
            self.reject(
                unit,
                "this syntax is no longer supported; use def, start, if, else, loop, or while",
            );
            return None;
        }
        if let Some(assignment) = assignment_parts(header) {
            if unit.body {
                self.reject(unit, "data assignments do not have an indented body");
            } else {
                match self.data(header, assignment, unit.start) {
                    Ok(item) => return Some(item),
                    Err(error) => self.errors.push(error),
                }
            }
            return None;
        }
        if data_header(header) {
            self.reject(
                unit,
                "var/list declarations are no longer supported; use name = value or name = {...}",
            );
            return None;
        }
        if let Some(rest) = keyword_rest(header, "import") {
            if context == Context::Sprite {
                self.reject(unit, "import is not allowed inside a sprite");
            } else if unit.body {
                self.reject(unit, "import does not have a body");
            } else if !rest.split('.').all(is_data_name) {
                self.reject(unit, "import requires a dotted module name without .sbdsl");
            } else if rest.ends_with(".sbdsl") {
                self.reject(unit, "omit the .sbdsl extension in import");
            } else {
                return Some(Item::Import {
                    module: rest.into(),
                    span,
                });
            }
            return None;
        }
        if !unit.body {
            self.reject(unit, "expected : followed by an indented block");
            return None;
        }
        if let Some(rest) = keyword_rest(header, "stage") {
            if context != Context::File {
                self.reject(unit, "stage is only allowed directly at file top level");
            } else if !rest.is_empty() {
                self.reject(unit, "expected stage:");
            } else {
                let children = match self.body_indent(unit) {
                    Some(indent) => self.items_until_end(Context::Stage, indent, unit),
                    None => {
                        self.expect_end(unit);
                        Vec::new()
                    }
                };
                return Some(Item::Stage(children));
            }
        } else if let Some(rest) = keyword_rest(header, "sprite") {
            if context == Context::Sprite {
                self.reject(unit, "sprite declarations cannot be nested inside a sprite");
            } else if !rest.starts_with('"') {
                self.reject(unit, "sprite name must be double quoted");
            } else {
                let name = string_end(rest, 0).map_err(public_error).and_then(|end| {
                    parse_string(&rest[..end]).map(|name| (name, rest[end..].trim()))
                });
                match name {
                    Err(error) => self.reject(unit, error.0),
                    Ok((_, tail)) if !tail.is_empty() && tail != "add" => {
                        self.reject(unit, "expected optional add after sprite name");
                    }
                    Ok((name, tail)) => {
                        let add = tail == "add";
                        let (init, children) = if let Some(indent) = self.body_indent(unit) {
                            let init = if add { None } else { self.sprite_init(indent) };
                            (init, self.items_until_end(Context::Sprite, indent, unit))
                        } else {
                            if !add {
                                self.error(unit.start, "a new sprite must begin with init:");
                            }
                            self.expect_end(unit);
                            (None, vec![])
                        };
                        return Some(Item::Sprite {
                            name,
                            add,
                            init,
                            items: children,
                            span,
                        });
                    }
                }
            }
        } else if let Some(rest) = keyword_rest(header, "def") {
            if rest.is_empty() {
                self.reject(
                    unit,
                    "procedure definition needs a nonempty header and a body",
                );
            } else {
                let body = self.block_body(unit);
                self.expect_end(unit);
                return Some(Item::Procedure {
                    header: rest.into(),
                    body,
                    span,
                });
            }
        } else if keyword_rest(header, "piece").is_some() {
            if header != "piece" {
                self.reject(unit, "expected piece: with no additional text");
            } else if !unit.body {
                self.reject(unit, "piece must be written as piece: followed by end");
            } else {
                let body = self.block_body(unit);
                self.expect_end(unit);
                return Some(Item::Piece { body, span });
            }
        } else if keyword_rest(header, "init").is_some() {
            self.reject(unit, "init must appear exactly once as the first item of a new sprite; it is forbidden in add and other bodies");
        } else if header.starts_with('@') {
            self.reject(unit, "unknown or misplaced declaration");
        } else if control_header(header) || branch_header(header) {
            self.reject(
                unit,
                "control flow is only allowed inside an event or procedure body",
            );
        } else if header.is_empty() {
            self.reject(unit, "event body needs a hat header");
        } else {
            let body = self.block_body(unit);
            self.expect_end(unit);
            return Some(Item::Script {
                header: header.clone(),
                body,
                span,
            });
        }
        None
    }

    fn sprite_init(&mut self, indent: usize) -> Option<SpriteInit> {
        let Some((start, actual)) = self.peek() else {
            self.error(self.pos, "a new sprite must begin with init:");
            return None;
        };
        if actual != indent || !starts_keyword(&self.source[start..], "init") {
            self.error(start, "a new sprite must begin with init:");
            return None;
        }
        let unit = self.read_unit(indent)?;
        if unit.header != "init" {
            self.reject(&unit, "expected init: followed by indented data fields");
            return None;
        }
        let has_body = self.body_indent(&unit).is_some();
        let object_start = self.pos;
        if has_body {
            self.skip_children(indent);
        }
        let object_end = self.pos;
        let text = self.source[object_start..object_end].trim();
        let init = (|| -> crate::Result<SpriteInit> {
            if text.starts_with('{') {
                return Err(Error::new(
                    "init uses indented fields without outer braces; use pass for defaults",
                )
                .at(&self.span_at(object_start)));
            }
            if !has_body || text == "pass" {
                Ok(SpriteInit::default())
            } else {
                crate::notation::parse_region::<SpriteInit>(
                    self.path,
                    self.original,
                    object_start..object_end,
                )
            }
        })();
        let result = init.and_then(|init| {
            init.validate()
                .map_err(|error| error.at(&self.span(&unit)))?;
            Ok(init)
        });
        self.expect_end(&unit);
        match result {
            Ok(init) => Some(init),
            Err(error) => {
                self.errors.push(error);
                None
            }
        }
    }

    fn statements(&mut self, indent: usize) -> Vec<Statement> {
        let mut statements = Vec::new();
        while let Some(unit) = self.next_at(indent) {
            if let Some(statement) = self.statement(&unit) {
                statements.push(statement);
            }
        }
        statements
    }

    fn statement(&mut self, unit: &Unit) -> Option<Statement> {
        let header = &unit.header;
        if header == "pass" && !unit.body {
            return None;
        }
        if self.reject_stray_end(unit) {
            return None;
        }
        if keyword_rest(header, "piece").is_some() {
            self.reject(
                unit,
                "piece cannot be nested inside a script, piece, or control-flow body",
            );
            return None;
        }
        if let Some(assignment) = assignment_parts(header) {
            let target = parse_reference(assignment.target);
            if unit.body {
                self.reject(unit, "assignments do not have an indented body");
            } else if target.is_err() {
                self.reject(
                    unit,
                    "assignment target must be a scalar variable reference",
                );
            } else if assignment.value.is_empty() {
                self.reject(unit, "assignment requires a right-hand value");
            } else if assignment.value.starts_with('{') {
                if assignment.operator != "=" {
                    self.reject(unit, "list assignment requires =");
                } else {
                    match parse_list(assignment.value) {
                        Ok(values) => {
                            let (name, global) = target.unwrap();
                            return Some(Statement::ListAssignment {
                                name,
                                global,
                                values,
                                span: self.span(unit),
                            });
                        }
                        Err(error) => self.reject(unit, error.0),
                    }
                }
            } else {
                return Some(Statement::Command {
                    source: header.clone(),
                    span: self.span(unit),
                });
            }
            return None;
        }
        if declaration_header(header) {
            self.reject(
                unit,
                "declarations and imports are not allowed inside a script body",
            );
        } else if branch_header(header) {
            self.reject(
                unit,
                "elif and else require a preceding if at the same indentation",
            );
        } else if control_header(header) {
            return Some(self.control(unit));
        } else if unit.body && qualified_source(header) {
            return Some(self.extension_control(unit));
        } else if unit.body {
            self.reject(
                unit,
                "expected built-in control flow or a qualified extension C-block header",
            );
        } else if command_source(header) {
            return Some(Statement::Command {
                source: header.clone(),
                span: self.span(unit),
            });
        } else {
            self.reject(
                unit,
                "expected an assignment, complete command block, or control-flow body",
            );
        }
        None
    }

    fn extension_control(&mut self, unit: &Unit) -> Statement {
        let first = self.block_body(unit);
        let mut branches = vec![first];
        while let Some((start, indent)) = self.peek() {
            if indent != unit.indent || !starts_keyword(&self.source[start..], "else") {
                break;
            }
            let Some(otherwise) = self.read_unit(indent) else {
                continue;
            };
            if otherwise.header != "else" {
                self.reject(
                    &otherwise,
                    "extension C-block branches use else: with no condition",
                );
                continue;
            }
            branches.push(self.block_body(&otherwise));
        }
        self.expect_end(unit);
        Statement::ExtensionControl {
            header: unit.header.clone(),
            branches,
            span: self.span(unit),
        }
    }

    fn control(&mut self, unit: &Unit) -> Statement {
        if if_header(&unit.header) {
            return self.if_control(unit);
        }
        let body = self.block_body(unit);
        self.expect_end(unit);
        Statement::Control {
            header: unit.header.clone(),
            body,
            else_body: None,
            span: self.span(unit),
        }
    }

    /// Parse the complete if/elif/else chain before consuming its single end.
    fn if_control(&mut self, unit: &Unit) -> Statement {
        let mut branches = vec![(unit.header.clone(), self.block_body(unit), self.span(unit))];
        let mut else_body = None;
        while let Some((start, indent)) = self.peek() {
            if indent != unit.indent {
                break;
            }
            let line = self.line_text(start);
            if !starts_keyword(line, "elif") && !starts_keyword(line, "else") {
                break;
            }
            let Some(otherwise) = self.read_unit(indent) else {
                continue;
            };
            if keyword_rest(&otherwise.header, "elif").is_some() {
                if else_body.is_some() {
                    self.reject(&otherwise, "elif cannot follow else");
                    continue;
                }
                let body = self.block_body(&otherwise);
                branches.push((otherwise.header.clone(), body, self.span(&otherwise)));
            } else if otherwise.header == "else" {
                if else_body.is_some() {
                    self.reject(&otherwise, "duplicate else");
                    continue;
                }
                else_body = Some(self.block_body(&otherwise));
            } else {
                self.reject(
                    &otherwise,
                    "else must be written as else: with no condition",
                );
            }
        }
        self.expect_end(unit);
        let mut chain = else_body;
        for (header, body, span) in branches.into_iter().rev() {
            chain = Some(vec![Statement::Control {
                header,
                body,
                else_body: chain,
                span,
            }]);
        }
        chain
            .and_then(|mut nodes| nodes.pop())
            .expect("if chain has at least one node")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(source: &str) -> Result<Vec<Item>> {
        parse(Path::new("python.sbdsl"), source)
    }

    #[test]
    fn python_suites_declarations_init_and_elif_keep_source_locations() {
        let source = "values = {1, \"二\"}\nsprite \"玩家\":\n    init:\n        x: -10\n        costumes: [{name: \"猫\", path: \"a\\u002fb.svg\"}]\n    end\n    global:score = 0\n    def 检查 @n:\n        if <@n < 0>:\n            说 (\"负\")\n        elif <@n = 0>:\n            pass\n        elif <@n > 1>:\n            说 (\"大\")\n        else:\n            说 (\"一\")\n        end\n    end\n    start:\n        loop 2:\n            检查 (1)\n        end\n    end\nend\n";
        let items = parse_text(source).unwrap();
        assert!(
            matches!(&items[0], Item::Data { list: true, value, .. } if value.as_array().unwrap().len() == 2)
        );
        let Item::Sprite {
            init: Some(init),
            items,
            ..
        } = &items[1]
        else {
            panic!()
        };
        assert_eq!(init.x, -10.);
        assert_eq!(init.costumes[0].path, Path::new("a/b.svg"));
        assert!(matches!(&items[0], Item::Data { global: true, .. }));
        let Item::Procedure { body, .. } = &items[1] else {
            panic!()
        };
        let Statement::Control {
            else_body: Some(otherwise),
            ..
        } = &body[0]
        else {
            panic!()
        };
        let Statement::Control {
            header,
            span,
            body,
            else_body: Some(otherwise),
            ..
        } = &otherwise[0]
        else {
            panic!()
        };
        assert_eq!(header, "elif <@n = 0>");
        assert!(body.is_empty());
        assert_eq!((span.line, span.column), (10, 9));
        assert_eq!(&source[span.start..span.end], header);
        assert!(matches!(
            &otherwise[0],
            Statement::Control {
                else_body: Some(_),
                ..
            }
        ));
        let recovered = parse_recovering(Path::new("python.sbdsl"), source);
        assert!(recovered.errors.is_empty(), "{:?}", recovered.errors);
    }

    #[test]
    fn indentation_recovery_retains_siblings_after_bad_tokens_and_init() {
        let source = "sprite \"玩家\":\n    init:\n        x: 1\n        x: 2\n    end\n    hp = 100\n    start:\n        说(\"broken\n        等待 (1) 秒\n        if <>:\n            var illegal = 1\n            说 (\"ok\")\n        else:\n            说 (\"else\")\n        end\n    end\nend\noutside = 2\n";
        let report = parse_recovering(Path::new("python.sbdsl"), source);
        assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
        let Item::Sprite {
            init: None, items, ..
        } = &report.items[0]
        else {
            panic!()
        };
        assert!(matches!(&items[0], Item::Data { name, .. } if name == "hp"));
        let Item::Script { body, .. } = &items[1] else {
            panic!()
        };
        assert_eq!(body.len(), 2);
        assert!(matches!(&report.items[1], Item::Data { name, .. } if name == "outside"));
        for error in report.errors {
            let span = error.span().unwrap();
            assert!(span.end <= source.len() && source.is_char_boundary(span.start));
        }
    }

    #[test]
    fn invalid_indent_branches_and_empty_suites_are_rejected() {
        for source in [
            "start:\n说 (\"bad\")\n",
            "start:\n    if <>:\n        pass\n      说 (\"bad\")\n",
            "start:\n    else:\n        pass\n",
            "start:\n    loop 2:\n        pass\n    else:\n        pass\n",
            "sprite \"a\":\n    a = 1\n",
            "sprite \"a\" add:\n    init:\n        pass\n",
            "start:\n    var a = 1\n",
            "start:\n",
        ] {
            assert!(parse_text(source).is_err(), "{source}");
        }
        assert!(
            parse_text(
                "sprite \"a\":\n    init:\n        pass\n    end\n    start:\n        pass\n    end\nend\n"
            )
            .is_ok()
        );
        assert!(
            parse_text("start:\r\n\tif <>:\r\n\t\tpass\r\n\telse:\r\n\t\tpass\r\n\tend\r\nend\r\n")
                .is_ok()
        );
    }

    #[test]
    fn multiline_commands_lists_comments_and_all_editing_prefixes_terminate() {
        let source = "名字 = {\n    1,\n    \"二\"\n}\nsprite \"猫😀\": # hello\n    init:\n        pass\n    end\n    start:\n        说 (1 + 2)\n        # comment\n        if <>:\n            说 'a#b'\n        else:\n            pass\n        end\n    end\nend\n";
        assert!(parse_text(source).is_ok());
        for (end, _) in source
            .char_indices()
            .chain(std::iter::once((source.len(), '\0')))
        {
            let report = parse_recovering(Path::new("python.sbdsl"), &source[..end]);
            for error in report.errors {
                let span = error.span().unwrap();
                assert!(span.end <= end && source.is_char_boundary(span.start));
            }
        }
    }

    #[test]
    fn bare_data_files_and_inline_comments_do_not_change_indentation() {
        let items = parse_text("score = 0\nnames = {1, \"two\"}\n").unwrap();
        assert_eq!(items.len(), 2);
        let source = "# inline 中文 # sprite \"a\":\n    # comment # init:\n        pass\n    end\n    # comment # start:\n        # comment # 说 (\"ok\")\n    end\nend\n";
        let items = parse_text(source).unwrap();
        let Item::Sprite { items, .. } = &items[0] else {
            panic!()
        };
        let Item::Script { body, .. } = &items[0] else {
            panic!()
        };
        let Statement::Command { span, .. } = &body[0] else {
            panic!()
        };
        assert_eq!(span.line, 5);
        assert_eq!(&source[span.start..span.end], "说 (\"ok\")");
    }

    #[test]
    fn end_terminates_events_definitions_pieces_and_control_flow() {
        let source = r#"piece:
    说("loose")
    if <true>:
        说("inside")
    end
end
sprite "玩家":
    init:
        pass
    end
    def 动作 (steps)<ready>:
        if <@ready>:
            等待 (@steps) 秒
        end
    end
    start:
        动作 (1)<true>
    end
end
"#;
        let items = parse_text(source).unwrap();
        assert!(matches!(&items[0], Item::Piece { .. }));
        let Item::Sprite { items, .. } = &items[1] else {
            panic!()
        };
        assert!(matches!(&items[0], Item::Procedure { .. }));
        assert!(matches!(&items[1], Item::Script { .. }));
        assert!(
            parse_recovering(Path::new("python.sbdsl"), source)
                .errors
                .is_empty()
        );

        for invalid in [
            "start:\n    说 (1)\n",
            "start:\n    if <>:\n        pass\n",
            "piece:\n    piece:\n    end\nend\n",
            "piece:\n    start:\n        pass\n    end\nend\n",
            "end\n",
        ] {
            assert!(parse_text(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn old_spellings_and_brace_suites_are_rejected() {
        for source in [
            "@a = 1",
            "@a = {1}",
            "@stage:\n    pass",
            "@sprite \"a\":\n    init:\n        pass",
            "a = (1)",
            "start { 说 (\"old\") }",
            "sprite \"a\" { init {} }",
            "sprite \"a\":\n    @init:\n        pass",
            "sprite \"a\":\n    init:\n        {}\n",
            "sprite \"a\":\n    init:\n        pass\n    @global var a\n",
            "定义自制积木 p:\n    pass",
            "define p:\n    pass",
            "当绿旗被点击:\n    pass",
            "start:\n    如果 <> 那么:\n        pass",
            "start:\n    repeat 2:\n        pass",
            "start:\n    forever:\n        pass",
            "start:\n    until <>:\n        pass",
            "start:\n    if <>:\n        pass\n    否则:\n        pass",
        ] {
            assert!(parse_text(source).is_err(), "{source}");
        }
    }

    #[test]
    fn loop_and_while_headers_remain_source_exact() {
        let source = "start:\n    loop 3:\n        while <x < 5>:\n            x += 1\n        end\n    end\n    while true:\n        pass\n    end\nend\n";
        let items = parse_text(source).unwrap();
        let Item::Script { body, .. } = &items[0] else {
            panic!()
        };
        let Statement::Control {
            header,
            body: nested,
            ..
        } = &body[0]
        else {
            panic!()
        };
        assert_eq!(header, "loop 3");
        assert!(
            matches!(&nested[0], Statement::Control { header, .. } if header == "while <x < 5>")
        );
        assert!(matches!(&body[1], Statement::Control { header, .. } if header == "while true"));
    }

    #[test]
    fn extension_controls_keep_ordered_else_branches_and_nested_bodies() {
        let source = r#"start:
    ext.选择 <true> 第二 (2) 尾部 ("done"):
        score = 1
        if <score > 0>:
            说 (score)
        end
    else:
        "自定义 扩展"::"loop"((2)):
            score += 1
        end
    else:
        pass
    else:
        说 ("fourth")
    end
    说 ("after")
end
"#;
        let items = parse_text(source).unwrap();
        let Item::Script { body, .. } = &items[0] else {
            panic!("expected script");
        };
        let Statement::ExtensionControl {
            header,
            branches,
            span,
        } = &body[0]
        else {
            panic!("expected extension control");
        };
        assert_eq!(header, r#"ext.选择 <true> 第二 (2) 尾部 ("done")"#);
        assert_eq!(branches.len(), 4);
        assert_eq!(branches[0].len(), 2);
        assert!(matches!(&branches[0][1], Statement::Control { .. }));
        assert!(
            matches!(&branches[1][0], Statement::ExtensionControl { branches, .. } if branches.len() == 1)
        );
        assert!(branches[2].is_empty());
        assert_eq!(span.line, 2);
        assert_eq!(body.len(), 2);
        let recovering = parse_recovering(Path::new("python.sbdsl"), source);
        assert!(recovering.errors.is_empty(), "{:?}", recovering.errors);
    }

    #[test]
    fn extension_controls_reject_orphan_branches_bad_suites_and_wrong_shapes() {
        for source in [
            "start:\n    控制:\n        pass\n",
            "start:\n    (ext.值):\n        pass\n",
            "start:\n    ext.控制:\n        pass\n    elif <true>:\n        pass\n",
            "start:\n    ext.控制:\n        pass\n    else value:\n        pass\n",
            "start:\n    ext.控制:\n        pass\n    else:\n    说 (1)\n",
            "start:\n    else:\n        pass\n",
        ] {
            assert!(parse_text(source).is_err(), "{source}");
        }
        let report = parse_recovering(
            Path::new("python.sbdsl"),
            "start:\n    ext.控制:\n        pass\n    else:\n    说 (1)\n    end\nend\nafter = 2\n",
        );
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert!(matches!(&report.items[1], Item::Data { name, .. } if name == "after"));
    }

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

    fn names(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Data { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn invalid_data_retains_following_imports_and_declarations() {
        let report = recover(
            "broken = true\nbroken_list = {1,}\n正常 = 2\nimport utils.math\nglobal var bad\nlater = 3\n",
        );
        assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
        assert_eq!(names(&report.items), ["正常", "later"]);
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
    fn broken_tokens_keep_later_commands_and_utf8_locations() {
        let source = "start:\n    说(\"中文 😀\n    等待 (1) 秒\n    移到('鼠标\n    等待 (2) 秒\n    说(1 + 2]\n    等待 (3) 秒\nend\nimport fine\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
        let Item::Script { body, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(body.len(), 3);
        assert!(body.iter().all(
            |item| matches!(item, Statement::Command { source, .. } if source.starts_with("等待"))
        ));
        assert!(matches!(&report.items[1], Item::Import { module, .. } if module == "fine"));
        assert_eq!(
            report.errors[0].span().unwrap().start,
            source.find("\n    等待").unwrap()
        );
    }

    #[test]
    fn invalid_role_headers_and_misplaced_bodies_do_not_expose_children() {
        let source = "sprite bare:\n    init:\n        pass\n    global:hidden = 1\nsprite \"real\":\n    init:\n        pass\n    end\n    stage:\n        hidden2 = 2\n    sprite \"nested\":\n        init:\n            pass\n        end\n        global:hidden3 = 3\n    visible = 4\nend\noutside = 5\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 3, "{:?}", report.errors);
        assert_eq!(report.items.len(), 2);
        let Item::Sprite { name, items, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(name, "real");
        assert_eq!(names(items), ["visible"]);
        assert_eq!(names(&report.items), ["outside"]);
    }

    #[test]
    fn missing_and_invalid_init_retain_role_children() {
        let source = "sprite \"bad-json\":\n    init:\n        x: ,\n        y: 1\n    end\n    a = 1\nend\nsprite \"missing\":\n    b = 2\nend\nsprite \"good\":\n    init:\n        pass\n    end\n    c = 3\nend\nimport after\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert_eq!(report.items.len(), 4);
        for (index, expected) in ["a", "b", "c"].iter().enumerate() {
            let Item::Sprite { init, items, .. } = &report.items[index] else {
                panic!()
            };
            assert_eq!(init.is_some(), index == 2);
            assert_eq!(names(items), [*expected]);
        }
    }

    #[test]
    fn nested_errors_retain_if_elif_else_and_outer_sibling() {
        let source = "start:\n    if <>:\n        var illegal = 1\n        等待 (1) 秒\n    elif <>:\n        var invalid = 2\n        等待 (2) 秒\n    else:\n        等待 (3) 秒\n    end\n    等待 (4) 秒\nend\n";
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
        let Statement::Control {
            body,
            else_body: Some(otherwise),
            ..
        } = &otherwise[0]
        else {
            panic!()
        };
        assert_eq!(body.len(), 1);
        assert_eq!(otherwise.len(), 1);
    }

    #[test]
    fn unfinished_token_at_eof_has_one_primary_error() {
        for (source, expected) in [
            ("start:\n    说(\"unfinished\nend\n", 1),
            (
                "start:\n    if <>:\n        移到('unfinished\n    end\nend\n",
                1,
            ),
            ("start:\n    if <>:\n        等待(1 + 2\n    end\nend\n", 1),
            // The unterminated block comment swallows the closing end, so the
            // missing-end diagnostic is expected alongside the comment error.
            ("start:\n    #\n    unfinished comment\nend\n", 2),
        ] {
            let report = recover(source);
            assert_eq!(
                report.errors.len(),
                expected,
                "{source}\n{:?}",
                report.errors
            );
        }
    }

    #[test]
    fn missing_indented_body_recovers_at_next_declaration() {
        let report = recover(
            "start:\nsprite \"later\":\n    init:\n        pass\n    end\n    hp = 1\nend\nimport after\n",
        );
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(report.items.len(), 3);
        assert!(matches!(&report.items[1], Item::Sprite { name, .. } if name == "later"));
        assert!(matches!(&report.items[2], Item::Import { module, .. } if module == "after"));
    }

    #[test]
    fn comments_multiline_atoms_and_json_escapes_survive_unrelated_errors() {
        let source = "invalid = true\n# hidden = 1\nvalues = {1,\n    # invisible list comment\n    2}\nstart:\n    说 (\"# literal\")\n    说 'a#b'\n    等待 (8 / 2) 秒\nend\nsprite \"json\":\n    init:\n        costumes: [{name: \"x\", path: \"a\\u002fb#c.svg\"}]\n    end\nend\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert_eq!(names(&report.items), ["values"]);
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
    fn malformed_menu_ignores_hashes_and_keeps_later_declarations() {
        let source = "start:\n    选择 'bad\\q#value'\n    等待 (1) 秒\n    选择 'unfinished#\n    等待 (2) 秒\n    选择 'ok#}/'\nend\nafter = 1\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        let Item::Script { body, .. } = &report.items[0] else {
            panic!()
        };
        assert_eq!(body.len(), 3);
        assert!(matches!(&body[2], Statement::Command { source, .. } if source == "选择 'ok#}/'"));
        assert_eq!(names(&report.items), ["after"]);
    }

    #[test]
    fn unfinished_role_name_does_not_expose_indented_contents() {
        let source = "sprite \"broken:\n    init:\n        pass\n    hidden = 1\nsprite \"safe\":\n    init:\n        pass\n    end\nend\n";
        let report = recover(source);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(names(&report.items).is_empty());
        assert!(matches!(&report.items[0], Item::Sprite { name, .. } if name == "safe"));
    }
}
