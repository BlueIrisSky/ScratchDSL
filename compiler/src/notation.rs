//! Data objects shared by build configuration and sprite initialization.
//!
//! The syntax adds bare keys, newline separators and DSL comments to JSON.
//! Parsing emits JSON with an original-byte map, then deserializes directly
//! into the requested type. This retains serde's field/type/null validation.
use crate::{Error, Result};
use serde::de::DeserializeOwned;
use std::{collections::BTreeSet, ops::Range, path::Path};

/// Whitespace accepted between DSL tokens: ASCII spaces, tabs and newlines.
pub(crate) fn syntax_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Build files additionally allow `"path.js" as "alias"` in the root
/// `extensions` array. Sprite initialization and other data values do not.
pub fn parse_build<T: DeserializeOwned>(path: &Path, source: &str) -> Result<T> {
    parse_with_mode(path, source, 0..source.len(), true)
}

/// Parse one region of a complete source document. Errors retain byte offsets
/// and line/column positions in `source`, including any prefix before `range`.
pub fn parse_region<T: DeserializeOwned>(
    path: &Path,
    source: &str,
    range: Range<usize>,
) -> Result<T> {
    parse_with_mode(path, source, range, false)
}

fn parse_with_mode<T: DeserializeOwned>(
    path: &Path,
    source: &str,
    range: Range<usize>,
    build_extensions: bool,
) -> Result<T> {
    if range.start > range.end
        || range.end > source.len()
        || !source.is_char_boundary(range.start)
        || !source.is_char_boundary(range.end)
    {
        return Err(
            Error::new("data object range must follow UTF-8 character boundaries").at_source(
                path,
                source,
                range.start.min(source.len()),
                range.end.min(source.len()),
            ),
        );
    }
    let mut parser = Parser {
        path,
        source,
        pos: range.start,
        end: range.end,
        json: String::new(),
        origins: Vec::new(),
        build_extensions,
    };
    parser.trivia()?;
    let braced = parser.peek() == Some('{');
    parser.object(braced, 0)?;
    parser.trivia()?;
    if parser.pos != parser.end {
        return Err(parser.error(parser.pos, "unexpected text after data object"));
    }
    serde_json::from_str(&parser.json).map_err(|error| {
        // Generated JSON is compact and strings contain escaped line breaks.
        // serde's column therefore indexes directly into the byte-origin map.
        let offset = parser
            .origins
            .get(error.column().saturating_sub(1))
            .copied()
            .unwrap_or(parser.end);
        parser.error(
            offset,
            format!("invalid data object: {}", serde_message(&error)),
        )
    })
}

/// Shared DSL comment boundaries. `start` must point to `#` in `source`.
/// A standalone hash starts a multiline comment; otherwise the next hash on
/// that physical line closes it, or the line ending terminates the comment.
pub(crate) fn comment_end(source: &str, start: usize) -> Option<usize> {
    let line_end = source[start..]
        .find(['\n', '\r'])
        .map_or(source.len(), |n| start + n);
    if let Some(hash) = source[start + 1..line_end].find('#') {
        return Some(start + 1 + hash + 1);
    }
    let line_start = source[..start].rfind(['\n', '\r']).map_or(0, |n| n + 1);
    let alone = source[line_start..start]
        .chars()
        .all(|c| matches!(c, ' ' | '\t'))
        && source[start + 1..line_end]
            .chars()
            .all(|c| matches!(c, ' ' | '\t'));
    if alone {
        source[line_end..].find('#').map(|n| line_end + n + 1)
    } else {
        Some(line_end)
    }
}

struct Parser<'a> {
    path: &'a Path,
    source: &'a str,
    pos: usize,
    end: usize,
    json: String,
    origins: Vec<usize>,
    build_extensions: bool,
}

fn serde_message(error: &serde_json::Error) -> String {
    let message = error.to_string();
    // serde describes the generated JSON position. The enclosing Error already
    // carries the mapped source location, so do not expose a second location.
    message
        .rsplit_once(" at line ")
        .map_or_else(|| message.clone(), |(message, _)| message.to_string())
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.source[self.pos..self.end].chars().next()
    }

    fn error(&self, pos: usize, message: impl Into<String>) -> Error {
        let pos = self.source.floor_char_boundary(pos.min(self.end));
        let end = pos
            + self.source[pos..self.end]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
        Error::new(message).at_source(self.path, self.source, pos, end)
    }

    fn emit(&mut self, token: char, origin: usize) {
        self.json.push(token);
        self.origins
            .extend(std::iter::repeat_n(origin, token.len_utf8()));
    }

    fn emit_source(&mut self, start: usize, end: usize) {
        self.json.push_str(&self.source[start..end]);
        self.origins.extend(start..end);
    }

    /// Return whether trivia contains a physical line break. Escaped `\n`
    /// inside a string never reaches this method and cannot separate members.
    fn trivia(&mut self) -> Result<bool> {
        let start = self.pos;
        loop {
            match self.peek() {
                Some(c) if syntax_ws(c) => self.pos += 1,
                Some('#') => {
                    self.pos = comment_end(&self.source[..self.end], self.pos)
                        .ok_or_else(|| self.error(self.pos, "unterminated block comment"))?;
                }
                _ => break,
            }
        }
        Ok(self.source[start..self.pos].contains(['\n', '\r']))
    }

    fn expect(&mut self, token: char) -> Result<()> {
        if self.peek() != Some(token) {
            return Err(self.error(self.pos, format!("expected {token}")));
        }
        self.emit(token, self.pos);
        self.pos += token.len_utf8();
        Ok(())
    }

    fn quoted(&mut self) -> Result<(usize, usize, String)> {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek() {
                Some('"') => {
                    self.pos += 1;
                    let decoded = serde_json::from_str::<String>(&self.source[start..self.pos])
                        .map_err(|error| {
                            self.error(
                                start + error.column().saturating_sub(1),
                                format!("invalid string: {}", serde_message(&error)),
                            )
                        })?;
                    return Ok((start, self.pos, decoded));
                }
                Some('\r' | '\n') => {
                    return Err(self.error(
                        self.pos,
                        "strings cannot cross physical lines; use JSON escapes",
                    ));
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some('\r' | '\n') => {
                            return Err(self.error(
                                self.pos,
                                "strings cannot cross physical lines; use JSON escapes",
                            ));
                        }
                        Some(c) => self.pos += c.len_utf8(),
                        None => return Err(self.error(start, "unterminated string escape")),
                    }
                }
                Some(c) => self.pos += c.len_utf8(),
                None => return Err(self.error(start, "unterminated string")),
            }
        }
    }

    fn key(&mut self) -> Result<(String, usize)> {
        let start = self.pos;
        if self.peek() == Some('"') {
            let (start, end, name) = self.quoted()?;
            self.emit_source(start, end);
            return Ok((name, start));
        }
        if !self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            return Err(self.error(
                start,
                "object keys must be ASCII identifiers or double-quoted strings",
            ));
        }
        self.pos += 1;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.pos += 1;
        }
        let name = self.source[start..self.pos].to_string();
        self.emit('"', start);
        self.emit_source(start, self.pos);
        self.emit('"', self.pos - 1);
        Ok((name, start))
    }

    fn object(&mut self, braced: bool, depth: usize) -> Result<()> {
        self.check_depth(depth)?;
        if braced {
            self.expect('{')?;
        } else {
            self.emit('{', self.pos);
        }
        self.trivia()?;
        let mut keys = BTreeSet::new();
        loop {
            if braced && self.peek() == Some('}') {
                self.expect('}')?;
                return Ok(());
            }
            if !braced && self.pos == self.end {
                self.emit('}', self.end);
                return Ok(());
            }
            if self.pos == self.end {
                return Err(self.error(self.pos, "unclosed data object; expected }"));
            }
            let (key, origin) = self.key()?;
            if !keys.insert(key.clone()) {
                return Err(self.error(origin, format!("duplicate field `{key}`")));
            }
            self.trivia()?;
            self.expect(':')?;
            self.trivia()?;
            if self.build_extensions && depth == 0 && key == "extensions" {
                self.extensions(depth + 1)?;
            } else {
                self.value(depth + 1)?;
            }
            self.separator(if braced { Some('}') } else { None })?;
        }
    }

    fn array(&mut self, depth: usize) -> Result<()> {
        self.check_depth(depth)?;
        self.expect('[')?;
        self.trivia()?;
        loop {
            if self.peek() == Some(']') {
                self.expect(']')?;
                return Ok(());
            }
            if self.pos == self.end {
                return Err(self.error(self.pos, "unclosed array; expected ]"));
            }
            self.value(depth + 1)?;
            self.separator(Some(']'))?;
        }
    }

    fn extensions(&mut self, depth: usize) -> Result<()> {
        self.check_depth(depth)?;
        self.expect('[')?;
        self.trivia()?;
        loop {
            if self.peek() == Some(']') {
                self.expect(']')?;
                return Ok(());
            }
            if self.peek() != Some('"') {
                return Err(self.error(self.pos, "拓展必须写成双引号路径，可使用 as \"别名\""));
            }
            self.extension()?;
            self.separator(Some(']'))?;
        }
    }

    fn extension(&mut self) -> Result<()> {
        let (path_start, path_end, _) = self.quoted()?;
        self.trivia()?;
        let alias_keyword = self.source[self.pos..self.end]
            .strip_prefix("as")
            .is_some_and(|rest| {
                !rest
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
        if !alias_keyword {
            // An unaliased entry's newline belongs to the array separator.
            self.pos = path_end;
            self.emit_source(path_start, path_end);
            return Ok(());
        }
        let keyword = self.pos;
        if keyword == path_end {
            return Err(self.error(keyword, "as 前必须有空白或注释"));
        }
        self.pos += 2;
        let after_keyword = self.pos;
        self.trivia()?;
        if self.pos == after_keyword {
            return Err(self.error(after_keyword, "as 后必须有空白或注释"));
        }
        if self.peek() != Some('"') {
            return Err(self.error(self.pos, "as 后的拓展别名必须使用双引号"));
        }
        let (alias_start, alias_end, alias) = self.quoted()?;
        crate::config::validate_extension_alias(&alias)
            .map_err(|message| self.error(alias_start, message))?;
        for token in "{\"path\":".chars() {
            self.emit(token, path_start);
        }
        self.emit_source(path_start, path_end);
        for token in ",\"alias\":".chars() {
            self.emit(token, alias_start);
        }
        self.emit_source(alias_start, alias_end);
        self.emit('}', alias_end - 1);
        Ok(())
    }

    /// Normalize a comma or physical newline to JSON's comma. A trailing comma
    /// before the closing delimiter (or unbraced root EOF) emits no comma.
    fn separator(&mut self, close: Option<char>) -> Result<()> {
        let start = self.pos;
        let newline = self.trivia()?;
        if self.peek() == Some(',') {
            let comma = self.pos;
            self.pos += 1;
            self.trivia()?;
            if self.at_close(close) {
                return Ok(());
            }
            self.emit(',', comma);
            return Ok(());
        }
        if self.at_close(close) {
            return Ok(());
        }
        if newline {
            self.emit(',', start);
            return Ok(());
        }
        Err(self.error(
            self.pos,
            "object members and array elements must be separated by a comma or physical newline",
        ))
    }

    fn at_close(&self, close: Option<char>) -> bool {
        match close {
            Some(close) => self.peek() == Some(close),
            None => self.pos == self.end,
        }
    }

    fn check_depth(&self, depth: usize) -> Result<()> {
        if depth >= 128 {
            Err(self.error(self.pos, "data objects and arrays are nested too deeply"))
        } else {
            Ok(())
        }
    }

    fn value(&mut self, depth: usize) -> Result<()> {
        match self.peek() {
            Some('{') => self.object(true, depth),
            Some('[') => self.array(depth),
            Some('"') => {
                let (start, end, _) = self.quoted()?;
                self.emit_source(start, end);
                Ok(())
            }
            Some(_) => {
                let start = self.pos;
                while let Some(c) = self
                    .peek()
                    .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n' | ',' | '}' | ']' | '#'))
                {
                    self.pos += c.len_utf8();
                }
                let scalar = &self.source[start..self.pos];
                if matches!(scalar, "true" | "false" | "null") {
                    self.emit_source(start, self.pos);
                    return Ok(());
                }
                if scalar.starts_with('-')
                    || scalar.as_bytes().first().is_some_and(u8::is_ascii_digit)
                {
                    serde_json::from_str::<serde_json::Number>(scalar).map_err(|error| {
                        self.error(
                            start + error.column().saturating_sub(1),
                            format!("invalid finite JSON number: {}", serde_message(&error)),
                        )
                    })?;
                    self.emit_source(start, self.pos);
                    return Ok(());
                }
                Err(self.error(start, "expected an object, array, quoted string, finite JSON number, true, false, or null"))
            }
            None => Err(self.error(self.pos, "expected a value")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::{Value, json};

    fn value(source: &str) -> Result<Value> {
        parse_region(Path::new("sbdsl.buildsb"), source, 0..source.len())
    }

    #[test]
    fn bare_keys_newline_members_and_arrays_share_one_data_grammar() {
        let source = r##"name: "示例 😀"
entry: "main.sbdsl"
extensions: [
 "自定义拓展.js"
 "another.js",
]
runtime: {
 fps: 60
 fencing: false,
 miscLimits: true
}
stage: { backdrops: [], sounds: [], }
"##;
        let expected = json!({
            "name": "示例 😀", "entry": "main.sbdsl",
            "extensions": ["自定义拓展.js", "another.js"],
            "runtime": {"fps":60, "fencing":false, "miscLimits":true},
            "stage": {"backdrops":[], "sounds":[]}
        });
        assert_eq!(value(source).unwrap(), expected);
        assert_eq!(value(&format!("{{\n{source}\n}} ")).unwrap(), expected);
        assert_eq!(value("").unwrap(), json!({}));
        assert_eq!(value("{}\n# comment").unwrap(), json!({}));
        assert_eq!(value("a: 1,\nb: 2,").unwrap(), json!({"a":1,"b":2}));
    }

    #[test]
    fn build_extension_aliases_preserve_strings_comments_and_newline_separators() {
        let source = r##"extensions: [
 "first.js"
 "second.js" # path comment
 as # keyword comment
 " 张量-拓展 "
 "third.js"# inline #as# inline #"\u732b\ud83d\ude00"
 "last.js",
]
runtime: { fps: 60 }
"##;
        let expected = json!({
            "extensions": [
                "first.js",
                {"path":"second.js", "alias":" 张量-拓展 "},
                {"path":"third.js", "alias":"猫😀"},
                "last.js"
            ],
            "runtime": {"fps":60}
        });
        let path = Path::new("sbdsl.buildsb");
        assert_eq!(parse_build::<Value>(path, source).unwrap(), expected);
        assert_eq!(
            parse_build::<Value>(path, &format!("{{\n{source}\n}}")).unwrap(),
            expected
        );
        assert_eq!(
            parse_build::<Value>(path, "extensions: [\"as.js\", \"x.js\" as \"as\"]").unwrap()["extensions"]
                [1]["alias"],
            "as"
        );
        assert_eq!(
            parse_build::<Value>(path, "extensions: [\"x.js\" # end\n]").unwrap()["extensions"],
            json!(["x.js"])
        );
    }

    #[test]
    fn aliases_are_only_supported_in_build_root_extension_entries() {
        let path = Path::new("sbdsl.buildsb");
        let alias = r#"extensions: ["custom.js" as "custom"]"#;
        assert!(parse_region::<Value>(path, alias, 0..alias.len()).is_err());
        for source in [
            r#"entry: "main.sbdsl" as "main""#,
            r#"runtime: { extensions: ["custom.js" as "custom"] }"#,
            r#"other: ["custom.js" as "custom"]"#,
            r#"extensions: [{path: "custom.js", alias: "custom"}]"#,
            r#"extensions: [["custom.js", "custom"]]"#,
            r#"extensions: [false]"#,
            r#"extensions: null"#,
            r#"extensions: "custom.js" as "custom""#,
        ] {
            assert!(parse_build::<Value>(path, source).is_err(), "{source}");
        }
        assert_eq!(
            parse_build::<Value>(path, r#"entry: "literal as \"text\".sbdsl""#).unwrap()["entry"],
            "literal as \"text\".sbdsl"
        );
    }

    #[test]
    fn extension_alias_errors_point_to_the_invalid_token() {
        let path = Path::new("sbdsl.buildsb");
        for (entry, bad_token) in [
            (r#""a.js"as "x""#, "as"),
            (r#""a.js" as"x""#, "\"x\""),
            (r#""a.js" asdf "x""#, "asdf"),
            (r#""a.js" AS "x""#, "AS"),
            (r#""a.js" as alias"#, "alias"),
            (r#""a.js" as “alias”"#, "“alias”"),
            (r#""a.js" as """#, "\"\""),
            (r#""a.js" as " \u3000 ""#, "\" \\u3000 "),
            (r#""a.js" as "line\nbreak""#, "\"line"),
            (r#""a.js" as "tab\t""#, "\"tab"),
            (r#""a.js" as "delete\u007f""#, "\"delete"),
            (r#""a.js" as "x" as "y""#, "as \"y\""),
            (r#""a.js" as "x" "b.js""#, "\"b.js\""),
        ] {
            let source = format!("name: \"项目 😀\"\nextensions: [\n {entry}\n]\n");
            let error = parse_build::<Value>(path, &source).unwrap_err();
            let span = error.span().unwrap();
            assert_eq!(
                span.start,
                source.find(bad_token).unwrap(),
                "{source}: {error}"
            );
            assert_eq!(span.line, 3, "{source}: {error}");
        }
        for source in [
            "extensions: [\"a.js\" as ]",
            "extensions: [\"a.js\" as",
            "extensions: [\"a.js\" as \"unterminated]",
        ] {
            assert!(parse_build::<Value>(path, source).is_err(), "{source}");
        }
    }

    #[test]
    fn strings_keep_json_escapes_and_hashes() {
        let parsed = value(
            r##"text: "\u732b\ud83d\ude00\t\b\f\n\r\"\\\/ # literal"
"space key": null
_private0: -1.25e+3
"##,
        )
        .unwrap();
        assert_eq!(parsed["text"], "猫😀\t\u{8}\u{c}\n\r\"\\/ # literal");
        assert_eq!(parsed["space key"], Value::Null);
        assert_eq!(parsed["_private0"], -1250.0);
        for source in [
            "x: \"\\q\"",
            "x: \"\\uD800\"",
            "x: \"line\nbreak\"",
            "x: \"unterminated",
            "x: \"trailing\\",
        ] {
            assert!(value(source).is_err(), "{source}");
        }
    }

    #[test]
    fn comments_use_dsl_boundaries_and_physical_newlines() {
        let source = "# line comment\na: 1 # tail\n#\nmultiline comment\n#\nb: [1 # inline #, 2]\nc: \"#\"\n";
        assert_eq!(value(source).unwrap(), json!({"a":1,"b":[1,2],"c":"#"}));
        assert!(value("a: 1 # inline # b: 2").is_err());
        assert!(value("a: 1\n#\nnever closed").is_err());
        assert_eq!(value("a: 1\r\nb: 2").unwrap(), json!({"a":1,"b":2}));
        assert_eq!(value("a: 1\rb: 2").unwrap(), json!({"a":1,"b":2}));
    }

    #[test]
    fn members_need_commas_or_actual_line_breaks() {
        for source in [
            "a: 1 b: 2",
            "a: [1 2]",
            "a: [\"x\" \"y\"]",
            "a: {} b: {}",
            "a: [{} {}]",
            "a: 1,, b: 2",
            "a: [,1]",
            "a: \"x\\ny\" b: 2",
            "a: 1 \\n b: 2",
            "a: 1 # inline # b: 2",
        ] {
            assert!(value(source).is_err(), "{source}");
        }
        for source in [
            "a: [1,2,]",
            "a: [1\n2\n]",
            "a: {}\nb: {}",
            "a: [{}\n{}]",
            "a: 1,\nb: 2",
        ] {
            assert!(value(source).is_ok(), "{source}");
        }
    }

    #[test]
    fn values_are_literals_and_bare_keys_are_ascii_identifiers() {
        for source in [
            "a: bare",
            "a: $hp",
            "a: @param",
            "a: (1 + 2)",
            "a: /menu/",
            "a: .5",
            "a: 01",
            "a: +1",
            "a: 1.",
            "a: NaN",
            "a: Infinity",
            "a: 0x10",
            "a: 1e999",
            "a-b: 1",
            "角色: 1",
            "1st: 1",
            "a.b: 1",
            "a = 1",
        ] {
            assert!(value(source).is_err(), "{source}");
        }
        assert_eq!(
            value("a: true\nb: false\nc: null\nd: -2e-3").unwrap(),
            json!({"a":true,"b":false,"c":null,"d":-0.002})
        );
        assert_eq!(value("\"角色\": 1").unwrap(), json!({"角色":1}));
    }

    #[test]
    fn duplicate_keys_are_rejected_before_deserializing_any_target() {
        for source in [
            "x: 1\nx: 2",
            "{x: 1, \"x\": 2}",
            r#"x: 1, "\u0078": 2"#,
            "x: { a: 1\na: 2 }",
            "x: [{ name: \"a\", name: \"b\" }]",
        ] {
            let error = value(source).unwrap_err();
            assert!(error.0.contains("duplicate field"), "{source}: {error}");
            let span = error.span().unwrap();
            assert!(span.start > 0 && source.is_char_boundary(span.start));
        }
        assert!(value("x: {a: 1}\ny: {a: 2}").is_ok());
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Typed {
        count: u32,
    }

    #[test]
    fn serde_field_type_and_null_validation_maps_to_original_source() {
        let path = Path::new("角色.sbdsl");
        for object in [
            "{count: null}",
            "{count: -1}",
            "{count: 1.5}",
            "{count: 1\n unknown: 2}",
        ] {
            let fields = &object[1..object.len() - 1];
            let source = format!("sprite \"角色 😀\":\n    init:\n        {fields}\n");
            let start = source.find(fields).unwrap();
            let error =
                parse_region::<Typed>(path, &source, start..start + fields.len()).unwrap_err();
            let span = error.span().unwrap();
            assert!(
                span.start >= start && span.end <= start + fields.len(),
                "{error}"
            );
            assert_eq!(
                span.line,
                if object.contains("unknown") { 4 } else { 3 },
                "{error}"
            );
            assert!(source.is_char_boundary(span.start) && source.is_char_boundary(span.end));
            assert!(!error.0.contains(" at line "), "{error}");
        }
        let typed: Typed = parse_region(path, "count: 2", 0..8).unwrap();
        assert_eq!(typed.count, 2);
    }

    #[test]
    fn root_braces_and_region_boundaries_are_enforced() {
        let path = Path::new("test");
        for source in [
            "[]",
            "{x: 1",
            "x: [1",
            "{x: 1} trailing",
            "x: 1}",
            "{x: [1}",
        ] {
            assert!(value(source).is_err(), "{source}");
        }
        assert!(parse_region::<Value>(path, "😀", 1..4).is_err());
        let deep = format!("x: {}0{}", "[".repeat(200), "]".repeat(200));
        assert!(value(&deep).unwrap_err().0.contains("nested too deeply"));
    }
}
