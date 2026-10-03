//! Dokploy keeps an environment block as one text of `KEY=VALUE` lines. The document plans it
//! per variable, so the engine reads the text for which variables are there and writes it back
//! with the owned variables set and everything else left exactly as it was.

/// The text of an environment block, line by line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct EnvText {
    lines: Vec<Line>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Line {
    /// A `KEY=VALUE` line, kept as written until it is set.
    Variable { key: String, raw: String },
    /// A blank line, a comment, or anything that is not `KEY=VALUE`: kept as it is.
    Other(String),
}

impl EnvText {
    pub(crate) fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().map(parse_line).collect(),
        }
    }

    /// The names of the variables, in order.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().filter_map(|line| match line {
            Line::Variable { key, .. } => Some(key.as_str()),
            Line::Other(_) => None,
        })
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.keys().any(|existing| existing == key)
    }

    /// Sets a variable: in place when it is there, at the end otherwise. A repeated variable
    /// keeps its first line, which is the one a shell would not override.
    pub(crate) fn set(&mut self, key: &str, value: &str) {
        let mut seen = false;
        self.lines.retain_mut(|line| match line {
            Line::Variable { key: existing, raw } if existing == key => {
                if seen {
                    return false;
                }
                seen = true;
                *raw = format!("{key}={value}");
                true
            }
            _ => true,
        });
        if !seen {
            self.lines.push(Line::Variable {
                key: key.to_owned(),
                raw: format!("{key}={value}"),
            });
        }
    }

    pub(crate) fn render(&self) -> String {
        self.lines
            .iter()
            .map(|line| match line {
                Line::Variable { raw, .. } | Line::Other(raw) => raw.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn parse_line(line: &str) -> Line {
    let trimmed = line.trim_start();
    let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    if let Some((key, _)) = body.split_once('=')
        && !trimmed.starts_with('#')
        && is_name(key.trim())
    {
        return Line::Variable {
            key: key.trim().to_owned(),
            raw: line.to_owned(),
        };
    }

    Line::Other(line.to_owned())
}

/// A variable name: letters, digits, and underscores, not starting with a digit.
pub(crate) fn is_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_are_set_in_place_and_everything_else_is_left_alone() {
        let mut text = EnvText::parse("# keep me\nA=1\n\nexport B=2\nnot a variable\nA=again\n");
        text.set("A", "10");
        text.set("C", "3");

        assert_eq!(
            text.render(),
            "# keep me\nA=10\n\nexport B=2\nnot a variable\nC=3"
        );
        assert!(text.contains("B") && text.contains("C") && !text.contains("D"));
    }

    #[test]
    fn a_value_keeps_everything_after_the_first_equals_sign() {
        let text = EnvText::parse("URL=postgres://u:p@h/db?x=1");
        assert_eq!(text.render(), "URL=postgres://u:p@h/db?x=1");
        assert!(text.contains("URL"));
    }
}
