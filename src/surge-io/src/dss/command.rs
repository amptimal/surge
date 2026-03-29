// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Command-level parser for OpenDSS .dss scripts.
//!
//! Takes the flat token stream from the lexer and produces a list of
//! `DssCommand` variants that the object-builder consumes.

use super::lexer::Token;

/// A parsed OpenDSS command.
#[derive(Debug, Clone)]
pub enum DssCommand {
    /// `New <type>.<name> [prop=val ...]`
    New {
        obj_type: String,
        obj_name: String,
        properties: Vec<(String, String)>,
    },
    /// `Edit <type>.<name> [prop=val ...]` — same as New but object must exist.
    Edit {
        obj_type: String,
        obj_name: String,
        properties: Vec<(String, String)>,
    },
    /// `Set <option>=<value>` — sets a global option.
    Set { key: String, value: String },
    /// `More` or `~` continuation — appends properties to the last object.
    More { properties: Vec<(String, String)> },
    /// `Redirect <filename>` — parse another file and include its definitions.
    Redirect { path: String },
    /// `Compile <filename>` — same as Redirect in DSS.
    Compile { path: String },
    /// `Clear` — reset the circuit and start fresh.
    Clear,
    /// `Solve` — trigger a power flow (we parse but do not execute during import).
    Solve,
    /// Any other command that we recognise but skip (print warning).
    #[allow(dead_code)]
    Unknown { verb: String, args: String },
}

/// Parse a flat token stream into a list of `DssCommand`s.
pub fn parse_commands(tokens: &[Token]) -> Vec<DssCommand> {
    // Split token stream on Newline boundaries into logical lines.
    let lines: Vec<&[Token]> = tokens
        .split(|t| *t == Token::Newline)
        .filter(|s| !s.is_empty())
        .collect();

    let mut commands = Vec::new();

    for line in lines {
        if let Some(cmd) = parse_line(line) {
            commands.push(cmd);
        }
    }

    commands
}

/// Parse one logical line of tokens into a `DssCommand`.
fn parse_line(line: &[Token]) -> Option<DssCommand> {
    if line.is_empty() {
        return None;
    }

    let verb = match &line[0] {
        Token::Word(w) => w.as_str(),
        Token::Continuation => {
            // `~` lines: collect properties from the rest.
            let props = collect_properties(&line[1..]);
            return Some(DssCommand::More { properties: props });
        }
        _ => return None,
    };

    match verb.to_lowercase().as_str() {
        "new" => {
            let (obj_type, obj_name, properties) = parse_typed_object(&line[1..]);
            Some(DssCommand::New {
                obj_type,
                obj_name,
                properties,
            })
        }
        "edit" => {
            let (obj_type, obj_name, properties) = parse_typed_object(&line[1..]);
            Some(DssCommand::Edit {
                obj_type,
                obj_name,
                properties,
            })
        }
        "set" => {
            let props = collect_properties(&line[1..]);
            if let Some((k, v)) = props.into_iter().next() {
                Some(DssCommand::Set { key: k, value: v })
            } else {
                None
            }
        }
        "more" => {
            let props = collect_properties(&line[1..]);
            Some(DssCommand::More { properties: props })
        }
        "redirect" => {
            let path = extract_single_value(&line[1..]);
            Some(DssCommand::Redirect { path })
        }
        "compile" => {
            let path = extract_single_value(&line[1..]);
            Some(DssCommand::Compile { path })
        }
        "clear" => Some(DssCommand::Clear),
        "solve" => Some(DssCommand::Solve),
        // Commands we silently skip or acknowledge.
        "buscoords" | "makebuslist" | "calcvoltagebases" | "setkvbase" | "setkv" | "show"
        | "export" | "plot" | "reset" | "calcincmatrix" | "batchedit" | "disable" | "enable"
        | "updateisource" | "open" | "close" | "sample" | "snapshot" | "addbusmarker"
        | "cleanup" | "setisolated" | "readvoltages" => Some(DssCommand::Unknown {
            verb: verb.to_string(),
            args: tokens_to_str(&line[1..]),
        }),
        _ => Some(DssCommand::Unknown {
            verb: verb.to_string(),
            args: tokens_to_str(&line[1..]),
        }),
    }
}

/// Parse `<Type>.<Name> [prop=val ...]` from a token slice.
///
/// Handles two DSS syntax forms:
/// - Standard:  `New Circuit.ieee13 basekv=4.16`
/// - Alternate: `New object=circuit.ieee34 basekv=69`  (also `element=`)
fn parse_typed_object(tokens: &[Token]) -> (String, String, Vec<(String, String)>) {
    if tokens.is_empty() {
        return (String::new(), String::new(), Vec::new());
    }

    // Detect `object=Type.Name` or `element=Type.Name` alternate prefix.
    // Token layout: Word("object") Equals Word("Type.Name") [rest...]
    let (type_name, rest) = if let Token::Word(key) = &tokens[0] {
        let lkey = key.to_lowercase();
        if (lkey == "object" || lkey == "element") && tokens.get(1) == Some(&Token::Equals) {
            match tokens.get(2) {
                Some(Token::Word(tn)) => (tn.clone(), &tokens[3..]),
                _ => return (String::new(), String::new(), Vec::new()),
            }
        } else {
            (key.clone(), &tokens[1..])
        }
    } else {
        return (String::new(), String::new(), Vec::new());
    };

    // Split Type.Name at the first dot.
    let (obj_type, obj_name) = if let Some(dot) = type_name.find('.') {
        (
            type_name[..dot].to_string(),
            type_name[dot + 1..].to_string(),
        )
    } else {
        (type_name, String::new())
    };

    let properties = collect_properties(rest);
    (obj_type, obj_name, properties)
}

/// Collect `prop=val` pairs from a token slice.
///
/// Supports:
/// - `key=value` (simple)
/// - `key=[a b c]` or `key=(a,b,c)` (array — joined with spaces)
/// - Positional values (no `=`): treated as `("1", val)`, `("2", val)` etc.
pub fn collect_properties(tokens: &[Token]) -> Vec<(String, String)> {
    let mut props = Vec::new();
    let mut i = 0;
    let mut positional = 1usize;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(key) => {
                // Check for `key=val` pattern.
                if i + 1 < tokens.len() && tokens[i + 1] == Token::Equals {
                    let key = key.clone();
                    i += 2; // skip key and `=`
                    let (val, consumed) = extract_value(&tokens[i..]);
                    props.push((key, val));
                    i += consumed;
                } else {
                    // Positional word value (e.g. the object name without explicit key).
                    props.push((positional.to_string(), key.clone()));
                    positional += 1;
                    i += 1;
                }
            }
            Token::Value(v) => {
                // Quoted value without a preceding key.
                props.push((positional.to_string(), v.clone()));
                positional += 1;
                i += 1;
            }
            Token::Comma | Token::Equals => {
                i += 1; // skip stray separators
            }
            _ => {
                i += 1;
            }
        }
    }

    props
}

/// Extract a single value (possibly an array) from a token slice.
/// Returns `(value_string, tokens_consumed)`.
fn extract_value(tokens: &[Token]) -> (String, usize) {
    if tokens.is_empty() {
        return (String::new(), 0);
    }

    match &tokens[0] {
        Token::LeftBracket => {
            // Array value: collect until matching RightBracket.
            let mut parts = Vec::new();
            let mut i = 1;
            let mut depth = 1usize;
            while i < tokens.len() {
                match &tokens[i] {
                    Token::RightBracket => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    Token::LeftBracket => {
                        depth += 1;
                        i += 1;
                    }
                    Token::Word(w) => {
                        parts.push(w.clone());
                        i += 1;
                    }
                    Token::Value(v) => {
                        parts.push(v.clone());
                        i += 1;
                    }
                    Token::Comma => {
                        // Commas are separators — keep as spaces in our string.
                        i += 1;
                    }
                    Token::Newline | Token::Continuation => break,
                    _ => {
                        i += 1;
                    }
                }
            }
            (parts.join(" "), i)
        }
        Token::Word(w) => (w.clone(), 1),
        Token::Value(v) => (v.clone(), 1),
        _ => (String::new(), 1),
    }
}

/// Extract a single bare word or quoted value from tokens (for Redirect/Compile path).
fn extract_single_value(tokens: &[Token]) -> String {
    for t in tokens {
        match t {
            Token::Word(w) => return w.clone(),
            Token::Value(v) => return v.clone(),
            _ => {}
        }
    }
    String::new()
}

/// Reconstruct a raw args string from tokens (for Unknown commands).
fn tokens_to_str(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|t| match t {
            Token::Word(w) | Token::Value(w) => w.as_str().to_string(),
            Token::Equals => "=".to_string(),
            Token::LeftBracket => "[".to_string(),
            Token::RightBracket => "]".to_string(),
            Token::Comma => ",".to_string(),
            Token::Newline => "\n".to_string(),
            Token::Continuation => "~".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dss::lexer::tokenize;

    fn parse(s: &str) -> Vec<DssCommand> {
        parse_commands(&tokenize(s))
    }

    #[test]
    fn new_circuit_parsed() {
        let cmds = parse("New Circuit.test basekv=4.16");
        assert!(matches!(
            &cmds[0],
            DssCommand::New { obj_type, obj_name, .. }
            if obj_type == "Circuit" && obj_name == "test"
        ));
    }

    #[test]
    fn new_object_equals_syntax() {
        // Alternate DSS form used by IEEE 34/37/123 feeders from dss-extensions
        let cmds = parse("New object=circuit.ieee34 basekv=69");
        assert!(matches!(
            &cmds[0],
            DssCommand::New { obj_type, obj_name, .. }
            if obj_type.to_lowercase() == "circuit" && obj_name == "ieee34"
        ));
    }

    #[test]
    fn new_element_equals_syntax() {
        let cmds = parse("New element=Line.L1 bus1=A bus2=B r1=0.1 x1=0.3");
        assert!(matches!(
            &cmds[0],
            DssCommand::New { obj_type, obj_name, .. }
            if obj_type.to_lowercase() == "line" && obj_name == "L1"
        ));
    }

    #[test]
    fn new_line_with_array_property() {
        let cmds = parse("New Line.L1 rmatrix=[0.1 0.05 0.1]");
        if let DssCommand::New { properties, .. } = &cmds[0] {
            let rmat = properties
                .iter()
                .find(|(k, _)| k == "rmatrix")
                .map(|(_, v)| v.as_str());
            assert_eq!(rmat, Some("0.1 0.05 0.1"));
        } else {
            panic!("Expected New command");
        }
    }

    #[test]
    fn continuation_becomes_more() {
        let cmds = parse("New Line.L1 bus1=A\n~ bus2=B");
        assert!(matches!(cmds.get(1), Some(DssCommand::More { .. })));
    }

    #[test]
    fn redirect_command() {
        let cmds = parse("Redirect feeders.dss");
        assert!(matches!(
            &cmds[0],
            DssCommand::Redirect { path } if path == "feeders.dss"
        ));
    }

    #[test]
    fn clear_and_solve_parsed() {
        let cmds = parse("Clear\nSolve");
        assert!(matches!(cmds[0], DssCommand::Clear));
        assert!(matches!(cmds[1], DssCommand::Solve));
    }
}
