// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Lexer for OpenDSS .dss script files.
//!
//! Produces a sequence of `Token`s that the command parser consumes.
//!
//! ## Grammar elements handled
//! - `!` and `//` — line comments (strip to end of line)
//! - `~` — continuation line (logical continuation of previous command)
//! - `"..."` and `'...'` — quoted strings (preserve whitespace inside)
//! - `[...]` and `(...)` — array delimiters
//! - `|` — alternative array separator (treated as space inside arrays)
//! - `;` — separator within property lists (same as space)

/// A lexical token from a .dss script.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// A bare word (command verb, property name, type name, bus name, etc.)
    Word(String),
    /// `=` property assignment.
    Equals,
    /// A value token (right-hand side of `=`, or positional argument).
    Value(String),
    /// `[` or `(` — start of an array value.
    LeftBracket,
    /// `]` or `)` — end of an array value.
    RightBracket,
    /// `,` — array element separator.
    Comma,
    /// End of a logical command line.
    Newline,
    /// `~` — continuation; properties append to the current object.
    Continuation,
}

/// Tokenise a raw .dss script into a flat sequence of tokens.
///
/// Lines that start with `!` or `//` are stripped entirely.
/// Inline comments (after command content) are stripped.
/// `~` at the start of a line becomes `Token::Continuation`.
pub fn tokenize(input: &str) -> Vec<Token> {
    let logical_lines = preprocess_lines(input);
    let mut tokens = Vec::new();

    for line in logical_lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('~') {
            tokens.push(Token::Continuation);
            let rest = line.strip_prefix('~').map(str::trim).unwrap_or("");
            if !rest.is_empty() {
                tokenize_property_list(rest, &mut tokens);
            }
        } else {
            // Normal command line: first token is the command verb.
            let first_space = line.find(char::is_whitespace).unwrap_or(line.len());
            let verb = &line[..first_space];
            tokens.push(Token::Word(verb.to_string()));

            let rest = line[first_space..].trim();
            if !rest.is_empty() {
                tokenize_property_list(rest, &mut tokens);
            }
        }
        tokens.push(Token::Newline);
    }

    tokens
}

/// Pre-process raw lines:
/// 1. Strip `!` and `//` comments.
/// 2. Join `~` continuation lines onto the previous logical line.
///
/// Returns a list of logical lines (no continuation markers, no comments).
fn preprocess_lines(input: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    for raw_line in input.lines() {
        let line = strip_comment(raw_line);
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('~') {
            // Continuation — attach to previous logical line.
            // Emit separately so the tokenizer sees the `~` prefix.
            result.push(trimmed.to_string());
        } else {
            result.push(trimmed.to_string());
        }
    }

    result
}

/// Strip a `!` or `//` comment, respecting quoted regions.
fn strip_comment(line: &str) -> &str {
    let mut in_quote: Option<char> = None;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if let Some(q) = in_quote {
            if c == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }

        match c {
            '"' | '\'' => {
                in_quote = Some(c);
                i += 1;
            }
            '!' => {
                // Rest of line is comment.
                return &line[..byte_offset(line, i)];
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                return &line[..byte_offset(line, i)];
            }
            _ => {
                i += 1;
            }
        }
    }

    line
}

/// Get byte offset of char index `ci` in `s`.
fn byte_offset(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

/// Evaluate an OpenDSS inline RPN (Reverse Polish Notation) expression.
///
/// DSS uses `(expr)` for inline math, e.g. `(8 1000 /)` means 8 / 1000 = 0.008.
/// Supported operators: `+`, `-`, `*`, `/`, `sqrt`, `sqr`, `inv`, `abs`, `neg`.
/// Returns `Some(result)` if the expression is valid RPN, `None` otherwise.
fn eval_rpn(s: &str) -> Option<f64> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // Quick check: must have at least one operator (otherwise it's just a number
    // or array, not an RPN expression). Single numbers are not RPN.
    let has_operator = tokens.iter().any(|t| {
        matches!(
            t.to_lowercase().as_str(),
            "+" | "-" | "*" | "/" | "sqrt" | "sqr" | "inv" | "abs" | "neg"
        )
    });
    if !has_operator {
        return None;
    }

    let mut stack: Vec<f64> = Vec::new();
    for tok in &tokens {
        if let Ok(num) = tok.parse::<f64>() {
            stack.push(num);
        } else {
            match tok.to_lowercase().as_str() {
                "+" => {
                    let b = stack.pop()?;
                    let a = stack.pop()?;
                    stack.push(a + b);
                }
                "-" => {
                    let b = stack.pop()?;
                    let a = stack.pop()?;
                    stack.push(a - b);
                }
                "*" => {
                    let b = stack.pop()?;
                    let a = stack.pop()?;
                    stack.push(a * b);
                }
                "/" => {
                    let b = stack.pop()?;
                    let a = stack.pop()?;
                    if b.abs() < 1e-30 {
                        return None;
                    }
                    stack.push(a / b);
                }
                "sqrt" => {
                    let a = stack.pop()?;
                    stack.push(a.sqrt());
                }
                "sqr" => {
                    let a = stack.pop()?;
                    stack.push(a * a);
                }
                "inv" => {
                    let a = stack.pop()?;
                    if a.abs() < 1e-30 {
                        return None;
                    }
                    stack.push(1.0 / a);
                }
                "abs" => {
                    let a = stack.pop()?;
                    stack.push(a.abs());
                }
                "neg" => {
                    let a = stack.pop()?;
                    stack.push(-a);
                }
                _ => return None, // Unknown operator → not valid RPN
            }
        }
    }
    if stack.len() == 1 {
        Some(stack[0])
    } else {
        None // Stack should have exactly one value
    }
}

/// Tokenise the property portion of a command:
/// `TypeName.ObjectName prop1=val1 prop2=[a b c] ...`
fn tokenize_property_list(s: &str, tokens: &mut Vec<Token>) {
    let mut chars = s.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        match c {
            // Skip whitespace and semicolons (DSS allows `;` as separator)
            ' ' | '\t' | ';' => {
                chars.next();
            }
            '=' => {
                tokens.push(Token::Equals);
                chars.next();
            }
            '(' => {
                // OpenDSS uses (...) for inline RPN math expressions.
                // Collect content up to matching ')' and try to evaluate as RPN.
                // Examples: (8 1000 /) → 0.008, (.5 1000 /) → 0.0005
                chars.next(); // consume '('
                let inner_start = chars.peek().map(|&(b, _)| b).unwrap_or(s.len());
                let mut inner_end = s.len();
                while let Some(&(i, ch)) = chars.peek() {
                    if ch == ')' {
                        inner_end = i;
                        chars.next(); // consume ')'
                        break;
                    }
                    chars.next();
                }
                let inner = &s[inner_start..inner_end];
                if let Some(val) = eval_rpn(inner) {
                    // Successfully evaluated RPN → emit as a single word token
                    tokens.push(Token::Word(val.to_string()));
                } else {
                    // Not a valid RPN expression → treat as array brackets
                    tokens.push(Token::LeftBracket);
                    tokenize_property_list(inner, tokens);
                    tokens.push(Token::RightBracket);
                }
            }
            '[' => {
                tokens.push(Token::LeftBracket);
                chars.next();
            }
            ']' | ')' => {
                tokens.push(Token::RightBracket);
                chars.next();
            }
            ',' | '|' => {
                tokens.push(Token::Comma);
                chars.next();
            }
            '"' | '\'' => {
                let quote = c;
                chars.next(); // consume opening quote
                let inner_start = chars.peek().map(|&(b, _)| b).unwrap_or(s.len());
                let mut inner_end = s.len();
                while let Some(&(i, ch)) = chars.peek() {
                    if ch == quote {
                        inner_end = i;
                        chars.next(); // consume closing quote
                        break;
                    }
                    chars.next();
                }
                let value = s[inner_start..inner_end].to_string();
                tokens.push(Token::Value(value));
            }
            _ => {
                // Collect a word token (up to whitespace, =, [, ], (, ), ,, |, ;)
                let word_start = start;
                let mut word_end = s.len();
                chars.next();
                while let Some(&(i, ch)) = chars.peek() {
                    if " \t=[](),|;".contains(ch) {
                        word_end = i;
                        break;
                    }
                    chars.next();
                }
                let word = s[word_start..word_end].to_string();
                if !word.is_empty() {
                    tokens.push(Token::Word(word));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bang_comment() {
        let tokens = tokenize("New Line.L1 bus1=A bus2=B ! this is a comment");
        assert!(
            !tokens
                .iter()
                .any(|t| matches!(t, Token::Word(w) if w.contains("comment")))
        );
    }

    #[test]
    fn strips_slash_comment() {
        let tokens = tokenize("New Line.L1 // skip");
        assert!(
            !tokens
                .iter()
                .any(|t| matches!(t, Token::Word(w) if w == "skip"))
        );
    }

    #[test]
    fn continuation_produces_continuation_token() {
        let tokens = tokenize("New Line.L1 bus1=A\n~ bus2=B");
        assert!(tokens.contains(&Token::Continuation));
    }

    #[test]
    fn array_brackets_parsed() {
        let tokens = tokenize("New Line.L1 rmatrix=[1 2 3]");
        assert!(tokens.contains(&Token::LeftBracket));
        assert!(tokens.contains(&Token::RightBracket));
    }

    #[test]
    fn rpn_division() {
        assert!((eval_rpn("8 1000 /").unwrap() - 0.008).abs() < 1e-12);
    }

    #[test]
    fn rpn_multiplication() {
        assert!((eval_rpn("3 4 *").unwrap() - 12.0).abs() < 1e-12);
    }

    #[test]
    fn rpn_sqrt() {
        assert!((eval_rpn("16 sqrt").unwrap() - 4.0).abs() < 1e-12);
    }

    #[test]
    fn rpn_complex_expression() {
        // (2 3 + 4 *) = (2 + 3) * 4 = 20
        assert!((eval_rpn("2 3 + 4 *").unwrap() - 20.0).abs() < 1e-12);
    }

    #[test]
    fn rpn_single_number_is_not_rpn() {
        // A single number is NOT an RPN expression — it's a plain value.
        assert!(eval_rpn("8").is_none());
    }

    #[test]
    fn rpn_inline_in_tokenizer() {
        // XHL=(8 1000 /) should produce a Word("0.008") not brackets
        let tokens = tokenize("New Transformer.Sub XHL=(8 1000 /)");
        assert!(
            !tokens.contains(&Token::LeftBracket),
            "RPN expression should not produce brackets: {:?}",
            tokens
        );
        // Should have a Word containing the evaluated value
        let has_value = tokens.iter().any(|t| {
            if let Token::Word(w) = t {
                if let Ok(v) = w.parse::<f64>() {
                    (v - 0.008).abs() < 1e-10
                } else {
                    false
                }
            } else {
                false
            }
        });
        assert!(
            has_value,
            "Should contain evaluated RPN value 0.008: {:?}",
            tokens
        );
    }
}
