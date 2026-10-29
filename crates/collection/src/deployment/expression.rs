//! Small, deterministic expressions for collection base paths; never JavaScript.

use std::collections::BTreeMap;

#[derive(PartialEq)]
enum Value {
    Text(String),
    Bool(bool),
}

struct Parser<'a> {
    source: &'a str,
    at: usize,
    inputs: &'a BTreeMap<String, String>,
}

impl Parser<'_> {
    fn error(&self, message: &str) -> String {
        format!("base_path expression at byte {}: {message}", self.at + 1)
    }

    fn eat(&mut self, token: &str) -> bool {
        self.at += self.source[self.at..].len() - self.source[self.at..].trim_start().len();
        if self.source[self.at..].starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    fn conditional(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 32 {
            return Err(self.error("nesting exceeds 32 levels"));
        }
        let mut value = self.atom(depth)?;
        let equal = if self.eat("==") {
            Some(true)
        } else if self.eat("!=") {
            Some(false)
        } else {
            None
        };
        if let Some(equal) = equal {
            let right = self.atom(depth)?;
            if !matches!((&value, &right), (Value::Text(_), Value::Text(_))) {
                return Err(self.error("comparisons require strings"));
            }
            value = Value::Bool((value == right) == equal);
        }
        if !self.eat("?") {
            return Ok(value);
        }
        let yes = self.conditional(depth + 1)?;
        if !self.eat(":") {
            return Err(self.error("expected ':' after the true branch"));
        }
        let no = self.conditional(depth + 1)?;
        let Value::Bool(condition) = value else {
            return Err(self.error("a conditional requires a comparison"));
        };
        if !matches!((&yes, &no), (Value::Text(_), Value::Text(_))) {
            return Err(self.error("conditional branches must produce strings"));
        }
        Ok(if condition { yes } else { no })
    }

    fn atom(&mut self, depth: usize) -> Result<Value, String> {
        if self.eat("(") {
            let value = self.conditional(depth + 1)?;
            if !self.eat(")") {
                return Err(self.error("expected ')'"));
            }
            return Ok(value);
        }
        let rest = &self.source[self.at..];
        if let Some(quote @ ('\'' | '"')) = rest.chars().next() {
            self.at += 1;
            let mut text = String::new();
            while let Some(ch) = self.source[self.at..].chars().next() {
                self.at += ch.len_utf8();
                if ch == quote {
                    return Ok(Value::Text(text));
                }
                if ch == '\\' {
                    let escaped = self.source[self.at..]
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("unfinished escape"))?;
                    if !matches!(escaped, '\\' | '\'' | '"') {
                        return Err(self.error("only quotes and backslashes may be escaped"));
                    }
                    self.at += escaped.len_utf8();
                    text.push(escaped);
                } else {
                    text.push(ch);
                }
            }
            return Err(self.error("unclosed string"));
        }
        let length = rest
            .bytes()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == b'_')
            .count();
        if length == 0 {
            return Err(
                self.error("expected a template input, quoted string or parenthesized expression")
            );
        }
        let name = &rest[..length];
        self.at += length;
        self.inputs
            .get(name)
            .cloned()
            .map(Value::Text)
            .ok_or_else(|| {
                self.error(&format!(
                    "unknown input `{name}`; use serviceName, tenant, platform or slot"
                ))
            })
    }
}

pub(super) fn render(template: &str, inputs: &BTreeMap<String, String>) -> Result<String, String> {
    if template.len() > 16 * 1024 {
        return Err("base_path template exceeds 16 KiB".into());
    }
    let mut rendered = String::new();
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        rendered.push_str(&rest[..open]);
        rest = &rest[open + 2..];
        let close = rest
            .find("}}")
            .ok_or("base_path has an unclosed '{{' expression")?;
        let mut parser = Parser {
            source: &rest[..close],
            at: 0,
            inputs,
        };
        let value = parser.conditional(0)?;
        parser.eat("");
        if parser.at != parser.source.len() {
            return Err(parser.error(
                "unexpected syntax; only ==, != and condition ? value : fallback are supported",
            ));
        }
        let Value::Text(text) = value else {
            return Err(parser.error("the result must be a string"));
        };
        rendered.push_str(&text);
        rest = &rest[close + 2..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("tenant".into(), "north".into()),
            ("serviceName".into(), "shopsvc".into()),
            ("platform".into(), "stage-a".into()),
            ("slot".into(), "stage".into()),
        ])
    }

    #[test]
    fn names_comparisons_nested_conditionals_and_strings_are_deterministic() {
        for (source, expected) in [
            (
                "/{{tenant == 'north' ? serviceName : 'legacysvc'}}-{{slot}}",
                "/shopsvc-stage",
            ),
            ("/{{tenant != 'north' ? 'other' : serviceName}}", "/shopsvc"),
            (
                "/{{(tenant == 'north') ? (slot == 'stage' ? 'one' : 'two') : 'three'}}",
                "/one",
            ),
            (
                "/{{tenant == 'south' ? 'one' : tenant == 'north' ? 'two' : 'three'}}",
                "/two",
            ),
            (
                "/{{'雪'}}/{{\"quoted \\\"text\\\"\"}}",
                "/雪/quoted \"text\"",
            ),
            ("/literal", "/literal"),
        ] {
            assert_eq!(render(source, &inputs()).unwrap(), expected, "{source}");
        }
    }

    #[test]
    fn unknown_names_executable_syntax_and_wrong_types_are_rejected() {
        for expression in [
            "tenant == 'north'",
            "tenant ? 'one' : 'two'",
            "tenant == 'north' ? 'one'",
            "tenant == 'north' ? 'one' : secret",
            "baseUrl",
            "basePath",
            "environment",
            "unknown",
            "tenant.replace('north', 'south')",
            "fetch('https://example.test')",
            "tenant = 'south'",
            "tenant == 'north' && slot == 'stage' ? 'one' : 'two'",
            "'one' + 'two'",
            "tenant == 'north' ? 'one' : (slot == 'stage')",
            "'unclosed",
            "",
            "(tenant",
        ] {
            assert!(
                render(&format!("/{{{{{expression}}}}}"), &inputs()).is_err(),
                "{expression}"
            );
        }
        assert!(render("/{{tenant", &inputs()).is_err());
    }

    #[test]
    fn depth_and_source_size_are_bounded() {
        let nested = format!("/{{{{{}tenant{}}}}}", "(".repeat(34), ")".repeat(34));
        assert!(render(&nested, &inputs()).unwrap_err().contains("nesting"));
        assert!(render(&"x".repeat(16 * 1024 + 1), &inputs())
            .unwrap_err()
            .contains("16 KiB"));
    }
}
