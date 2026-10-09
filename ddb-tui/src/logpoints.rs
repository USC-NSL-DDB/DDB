//! Logpoint templates keep literal output separate from debugger expressions.
//! The stop/continue lifecycle is integrated separately from this pure parser.

const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
const MAX_EXPRESSIONS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Part {
    Text(String),
    Expression(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Template {
    pub parts: Vec<Part>,
}

impl Template {
    pub fn parse(message: &str) -> Result<Self, String> {
        if message.len() > MAX_TEMPLATE_BYTES {
            return Err("Log message exceeds 64 KiB".into());
        }
        let mut parts = Vec::new();
        let mut text = String::new();
        let mut expressions = 0;
        let mut chars = message.char_indices().peekable();
        while let Some((offset, ch)) = chars.next() {
            if matches!(ch, '{' | '}') && chars.peek().is_some_and(|(_, next)| *next == ch) {
                chars.next();
                text.push(ch);
                continue;
            }
            match ch {
                '}' => return Err("Unmatched closing brace in log message".into()),
                '{' => {}
                _ => {
                    text.push(ch);
                    continue;
                }
            }
            if !text.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut text)));
            }
            let start = offset + 1;
            let mut depth = 1;
            let mut quote = None;
            let mut escaped = false;
            let mut end = None;
            for (offset, ch) in chars.by_ref() {
                if let Some(delimiter) = quote {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == delimiter {
                        quote = None;
                    }
                } else {
                    match ch {
                        '\'' | '"' => quote = Some(ch),
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(offset);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            let end = end.ok_or("Unmatched opening brace in log message")?;
            let expression = message[start..end].trim();
            if expression.is_empty() {
                return Err("Logpoint expression cannot be empty".into());
            }
            expressions += 1;
            if expressions > MAX_EXPRESSIONS {
                return Err("Log message exceeds 128 expressions".into());
            }
            parts.push(Part::Expression(expression.into()));
        }
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        Ok(Self { parts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_are_never_debugger_expressions() {
        assert_eq!(Template::parse("").unwrap().parts, []);
        assert_eq!(
            Template::parse("literal; $pc = 0 {{braces}}")
                .unwrap()
                .parts,
            [Part::Text("literal; $pc = 0 {braces}".into())]
        );
        assert_eq!(
            Template::parse("你好 {{count}} = { counter }!")
                .unwrap()
                .parts,
            [
                Part::Text("你好 {count} = ".into()),
                Part::Expression("counter".into()),
                Part::Text("!".into())
            ]
        );
    }

    #[test]
    fn nested_braces_quotes_and_escapes_stay_inside_expressions() {
        let expression = r#"call({1, 2}, "}\\\"{", '}')"#;
        assert_eq!(
            Template::parse(&format!("result={{{expression}}}"))
                .unwrap()
                .parts,
            [
                Part::Text("result=".into()),
                Part::Expression(expression.into())
            ]
        );
        assert_eq!(
            Template::parse("{{{value}}}").unwrap().parts,
            [
                Part::Text("{".into()),
                Part::Expression("value".into()),
                Part::Text("}".into())
            ]
        );
    }

    #[test]
    fn invalid_templates_fail_before_any_expression_can_run() {
        for input in [
            "}",
            "{",
            "{}",
            "{  }",
            "{value",
            "{\"unterminated}",
            "good {x} bad }",
        ] {
            assert!(Template::parse(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn limits_count_utf8_bytes_and_expressions_not_characters() {
        assert!(Template::parse(&"x".repeat(MAX_TEMPLATE_BYTES)).is_ok());
        assert!(Template::parse(&"x".repeat(MAX_TEMPLATE_BYTES + 1)).is_err());
        assert!(Template::parse(&"é".repeat(MAX_TEMPLATE_BYTES / 2)).is_ok());
        assert!(Template::parse(&"é".repeat(MAX_TEMPLATE_BYTES / 2 + 1)).is_err());
        assert!(Template::parse(&"{x}".repeat(MAX_EXPRESSIONS)).is_ok());
        assert!(Template::parse(&"{x}".repeat(MAX_EXPRESSIONS + 1)).is_err());
    }
}
