// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E DYD v34+ dynamic data format parser.
//!
//! The DYD format stores dynamic model parameters as text records, one model
//! per (non-blank, non-comment) line:
//!
//! ```text
//!  GENROU  "1" 1 /  Td0' Td0'' Tq0' Tq0'' H D Xd Xq Xd' Xq' Xd'' Xl S1 S12
//!  GENCLS  "1" 2 /  H D
//!  EXST1   "1" 1 /  Tr Ka Ta Tc Tb Vrmax Vrmin Kc
//! ```
//!
//! Comment lines begin with `@`.  Blank lines are ignored.  The format
//! is `MODEL_TYPE "MACHINE_ID" BUS_NUMBER / param1 param2 ...`

use thiserror::Error;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single parsed record from a DYD file.
#[derive(Debug, Clone, PartialEq)]
pub struct DydRecord {
    /// PSS/E model type string (e.g. `"GENROU"`, `"GENCLS"`, `"EXST1"`).
    pub model_type: String,
    /// Machine/device identifier (contents of the quoted ID field).
    pub machine_id: String,
    /// Bus number where this model is attached.
    pub bus_number: u32,
    /// Model parameters in order (everything after the `/`).
    pub params: Vec<f64>,
}

/// Errors that can occur when parsing a DYD file.
#[derive(Debug, Error)]
pub enum Error {
    /// A line could not be parsed as a valid DYD record.
    #[error("parse error on line {line}: {message}")]
    ParseError { line: usize, message: String },

    /// The model type is not recognised by the converter.
    #[error("unknown model type: {0}")]
    UnknownModel(String),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a DYD-format string and return all recognised records.
///
/// Lines beginning with `@` are treated as comments and skipped.
/// Blank lines are also skipped.  Each data line must follow the format:
///
/// ```text
/// MODEL_TYPE "ID" BUS_NUMBER / param1 param2 ...
/// ```
///
/// The leading and trailing whitespace on each token is ignored.
pub fn loads(text: &str) -> Result<Vec<DydRecord>, Error> {
    let mut records = Vec::new();

    for (line_idx, raw_line) in text.lines().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = raw_line.trim();

        // Skip blank lines and comment lines.
        if trimmed.is_empty() || trimmed.starts_with('@') {
            continue;
        }

        let record = parse_line(trimmed, line_num)?;
        records.push(record);
    }

    Ok(records)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn parse_line(line: &str, line_num: usize) -> Result<DydRecord, Error> {
    // Split at the `/` separator first.
    let (header, param_str) = match line.split_once('/') {
        Some(pair) => pair,
        None => {
            return Err(Error::ParseError {
                line: line_num,
                message: "missing '/' separator between header and parameters".into(),
            });
        }
    };

    // Parse header: MODEL_TYPE "ID" BUS_NUMBER
    let header = header.trim();

    // Extract model type (first token, no quotes).
    let mut tokens = header.splitn(2, char::is_whitespace);
    let model_type = match tokens.next() {
        Some(t) if !t.is_empty() => t.to_uppercase(),
        _ => {
            return Err(Error::ParseError {
                line: line_num,
                message: "missing model type".into(),
            });
        }
    };

    let rest = tokens.next().unwrap_or("").trim();

    // Extract quoted machine_id: find opening and closing quote.
    let machine_id;
    let after_id;
    if let Some(open_pos) = rest.find('"') {
        let after_open = &rest[open_pos + 1..];
        if let Some(close_pos) = after_open.find('"') {
            machine_id = after_open[..close_pos].to_string();
            after_id = after_open[close_pos + 1..].trim();
        } else {
            return Err(Error::ParseError {
                line: line_num,
                message: "unterminated machine ID quote".into(),
            });
        }
    } else {
        return Err(Error::ParseError {
            line: line_num,
            message: "missing quoted machine ID".into(),
        });
    }

    // The next token after the quoted ID is the bus number.
    let bus_number_str = after_id.split_whitespace().next().unwrap_or("");
    let bus_number: u32 = bus_number_str.parse().map_err(|_| Error::ParseError {
        line: line_num,
        message: format!("invalid bus number: '{bus_number_str}'"),
    })?;

    // Parse parameters (everything after `/`).
    let params: Result<Vec<f64>, _> = param_str
        .split_whitespace()
        .map(|tok| tok.parse::<f64>())
        .collect();

    let params = params.map_err(|e| Error::ParseError {
        line: line_num,
        message: format!("invalid parameter value: {e}"),
    })?;

    Ok(DydRecord {
        model_type,
        machine_id,
        bus_number,
        params,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that a GENROU record is parsed correctly.
    #[test]
    fn test_dyd_parse_genrou_record() {
        let text =
            r#" GENROU "1" 1 /  8.0 0.03 0.4 0.05 6.5 0.0 1.8 1.7 0.3 0.55 0.25 0.2 0.1 0.13"#;
        let records = loads(text).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model_type, "GENROU");
        assert_eq!(r.machine_id, "1");
        assert_eq!(r.bus_number, 1);
        assert_eq!(r.params.len(), 14);
        assert!((r.params[0] - 8.0).abs() < 1e-10); // Td0'
        assert!((r.params[4] - 6.5).abs() < 1e-10); // H
    }

    /// Comment lines starting with @ must be silently skipped.
    #[test]
    fn test_dyd_comment_lines_ignored() {
        let text = "@ This is a comment\n GENCLS \"1\" 2 /  6.5 0.0\n@ another comment\n";
        let records = loads(text).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model_type, "GENCLS");
    }

    /// Blank lines must be silently skipped.
    #[test]
    fn test_dyd_blank_lines_ignored() {
        let text = "\n\n GENCLS \"1\" 3 /  5.0 2.0\n\n";
        let records = loads(text).unwrap();
        assert_eq!(records.len(), 1);
    }

    /// Multiple records in one string.
    #[test]
    fn test_dyd_multiple_records() {
        let text = concat!(
            " GENROU \"1\" 1 /  8.0 0.03 0.4 0.05 6.5 0.0 1.8 1.7 0.3 0.55 0.25 0.2 0.1 0.13\n",
            " GENCLS \"1\" 2 /  5.0 0.0\n",
            " EXST1  \"1\" 1 /  0.01 200.0 0.02 0.0 0.0 5.0 -5.0 0.0\n",
        );
        let records = loads(text).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].model_type, "GENROU");
        assert_eq!(records[1].model_type, "GENCLS");
        assert_eq!(records[2].model_type, "EXST1");
    }

    /// Missing `/` separator should produce a ParseError.
    #[test]
    fn test_dyd_missing_separator_error() {
        let text = " GENROU \"1\" 1  8.0 0.03";
        let result = loads(text);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("separator"), "Got: {err}");
    }
}
