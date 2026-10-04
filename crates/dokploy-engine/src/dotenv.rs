//! The dotenv files an `env_file` names (ADR 0010).
//!
//! A file is read as lines of `KEY=VALUE`. Nothing here ever puts a value in an error: the
//! file is a secret by default, so a problem is named by its line number.

use thiserror::Error;
use zeroize::Zeroizing;

/// A dotenv file that cannot be read as variables.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DotenvError {
    /// The file is not UTF-8 text.
    #[error("it is not UTF-8 text")]
    NotText,
    /// A line is not blank, a comment, or `KEY=VALUE`.
    #[error("line {line} is not `KEY=VALUE`, a comment, or blank")]
    NotAVariable { line: usize },
    /// A name is not letters, digits, and underscores, or starts with a digit.
    #[error("line {line}: `{name}` is not a variable name")]
    BadName { line: usize, name: String },
    /// A variable is set twice, and a file does not say which one wins.
    #[error("`{name}` is set on line {first} and again on line {second}")]
    Duplicate {
        name: String,
        first: usize,
        second: usize,
    },
}

/// One variable: its name and its value, in a buffer that is zeroised when dropped.
pub(crate) type Variable = (String, Zeroizing<Vec<u8>>);

/// The variables of a dotenv file, in file order, each value in a buffer that is zeroised when
/// dropped.
///
/// Blank lines and lines starting with `#` are skipped, and `export ` before a name is
/// accepted. Everything else is `KEY=VALUE`, with no spaces around the name and the value
/// being what [`dotenv_value`] makes of the rest of the line. A variable that appears twice
/// is an error.
pub(crate) fn parse_dotenv(bytes: &[u8]) -> Result<Vec<Variable>, DotenvError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DotenvError::NotText)?;
    let mut variables: Vec<Variable> = Vec::new();
    let mut lines_of: Vec<usize> = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let content = raw.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        let content = content.strip_prefix("export ").unwrap_or(content);
        let Some((name, value)) = content.split_once('=') else {
            return Err(DotenvError::NotAVariable { line });
        };
        if !valid_name(name) {
            return Err(DotenvError::BadName {
                line,
                name: name.chars().take(64).collect(),
            });
        }
        if let Some(position) = variables.iter().position(|(existing, _)| existing == name) {
            return Err(DotenvError::Duplicate {
                name: name.to_owned(),
                first: lines_of[position],
                second: line,
            });
        }
        variables.push((
            name.to_owned(),
            Zeroizing::new(dotenv_value(value).as_bytes().to_vec()),
        ));
        lines_of.push(line);
    }

    Ok(variables)
}

/// The value of a variable from what follows the first `=` of its line.
///
/// Everything after the `=` is the value, except that one matching pair of `"` or `'` around all
/// of it is removed, as `docker compose` does, so a file can be shared with it. Dokploy's
/// environment text has no quoting of its own and would pass the quote characters on to the
/// container. Nothing inside the quotes is unescaped, and a value that must keep its quotes is
/// written with a second pair around it.
fn dotenv_value(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    match bytes {
        [first @ (b'"' | b'\''), .., last] if first == last => &raw[1..raw.len() - 1],
        _ => raw,
    }
}

fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(text: &str) -> Vec<(String, String)> {
        parse_dotenv(text.as_bytes())
            .expect("parses")
            .into_iter()
            .map(|(name, value)| (name, String::from_utf8(value.to_vec()).unwrap()))
            .collect()
    }

    #[test]
    fn lines_are_variables_comments_and_blanks() {
        assert_eq!(
            pairs(
                "# a comment\n\nLOG_LEVEL=info\n  export PORT=8080\r\nEMPTY=\nURL=postgres://u:p@h/db?a=b\n"
            ),
            [
                ("LOG_LEVEL".to_owned(), "info".to_owned()),
                ("PORT".to_owned(), "8080".to_owned()),
                ("EMPTY".to_owned(), String::new()),
                ("URL".to_owned(), "postgres://u:p@h/db?a=b".to_owned()),
            ]
        );
        assert!(pairs("").is_empty());
    }

    #[test]
    fn one_matching_pair_of_quotes_around_a_whole_value_is_removed() {
        let quoted = |name: &str, value: &str| (name.to_owned(), value.to_owned());
        assert_eq!(
            pairs(
                "GREETING=\"hello world\"\nSINGLE='z'\nKEPT=\"\"x\"\"\nEMPTYQ=\"\"\nHALF=\"open\nLONE=\"\nMIXED=\"a'\nINNER=a\"b\"\nESCAPED=\"a\\\"b\"\n"
            ),
            [
                quoted("GREETING", "hello world"),
                quoted("SINGLE", "z"),
                quoted("KEPT", "\"x\""),
                quoted("EMPTYQ", ""),
                quoted("HALF", "\"open"),
                quoted("LONE", "\""),
                quoted("MIXED", "\"a'"),
                quoted("INNER", "a\"b\""),
                quoted("ESCAPED", "a\\\"b"),
            ],
            "only a pair that wraps the whole value goes, and nothing inside is unescaped"
        );
    }

    #[test]
    fn a_problem_is_named_by_its_line_and_never_by_a_value() {
        let secret = "hunter2-canary";
        assert_eq!(
            parse_dotenv(format!("OK=1\n{secret}\n").as_bytes()),
            Err(DotenvError::NotAVariable { line: 2 })
        );
        assert_eq!(
            parse_dotenv(b"1BAD=x\n"),
            Err(DotenvError::BadName {
                line: 1,
                name: "1BAD".to_owned()
            })
        );
        assert_eq!(
            parse_dotenv(b"BAD NAME=x\n"),
            Err(DotenvError::BadName {
                line: 1,
                name: "BAD NAME".to_owned()
            })
        );
        assert_eq!(
            parse_dotenv(format!("A={secret}\nB=2\nA={secret}\n").as_bytes()),
            Err(DotenvError::Duplicate {
                name: "A".to_owned(),
                first: 1,
                second: 3
            })
        );
        assert_eq!(parse_dotenv(&[0xff, 0xfe]), Err(DotenvError::NotText));
        for error in [
            parse_dotenv(format!("{secret}\n").as_bytes()).unwrap_err(),
            parse_dotenv(format!("A={secret}\nA={secret}\n").as_bytes()).unwrap_err(),
        ] {
            assert!(!error.to_string().contains(secret), "{error}");
        }
    }
}
