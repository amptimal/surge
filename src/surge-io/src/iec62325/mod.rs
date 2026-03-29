// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! IEC 62325 ESMP XML parser — Energy Market Data documents.
//!
//! Parses the flat `TimeSeries / Period / Point` XML structure used in
//! ENTSO-E transparency platform publications, balancing documents, and
//! schedule/bid/offer exchanges.
//!
//! This parser is standalone and is **not** part of the CGMES pipeline.

mod parser;

use std::path::Path;

use surge_network::network::market_data::MarketData;

/// Errors that can occur during IEC 62325 XML parsing.
#[derive(Debug, thiserror::Error)]
pub enum Iec62325Error {
    #[error("XML parse error: {0}")]
    Xml(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error(
        "document metadata mismatch in {path}: field {field} expected {expected}, found {found}"
    )]
    DocumentMismatch {
        path: String,
        field: &'static str,
        expected: String,
        found: String,
    },
}

/// Parse a single IEC 62325 market document from an XML string.
pub fn parse_market_document(xml: &str) -> Result<MarketData, Iec62325Error> {
    parser::parse_document(xml)
}

/// Parse one or more IEC 62325 market document files and merge into a single
/// [`MarketData`] container.
///
/// All files must agree on document-level identity fields (`mRID`, `type`,
/// sender, and receiver). Mismatched documents are rejected instead of being
/// silently merged.
pub fn parse_market_files(paths: &[impl AsRef<Path>]) -> Result<MarketData, Iec62325Error> {
    fn format_participant(
        participant: Option<&surge_network::network::market_data::MarketParticipant>,
    ) -> String {
        match participant {
            Some(participant) => format!(
                "{} / {} / {}",
                participant.mrid,
                participant.name,
                participant.role.as_deref().unwrap_or("<missing>")
            ),
            None => "<missing>".to_string(),
        }
    }

    fn merge_metadata(
        merged: &mut MarketData,
        doc: &MarketData,
        path: &Path,
    ) -> Result<(), Iec62325Error> {
        let path_display = path.display().to_string();

        if let (Some(expected), Some(found)) = (
            merged.document_mrid.as_deref(),
            doc.document_mrid.as_deref(),
        ) && expected != found
        {
            return Err(Iec62325Error::DocumentMismatch {
                path: path_display,
                field: "document_mrid",
                expected: expected.to_string(),
                found: found.to_string(),
            });
        }
        if merged.document_mrid.is_none() {
            merged.document_mrid = doc.document_mrid.clone();
        }

        if let (Some(expected), Some(found)) = (
            merged.document_type.as_deref(),
            doc.document_type.as_deref(),
        ) && expected != found
        {
            return Err(Iec62325Error::DocumentMismatch {
                path: path.display().to_string(),
                field: "document_type",
                expected: expected.to_string(),
                found: found.to_string(),
            });
        }
        if merged.document_type.is_none() {
            merged.document_type = doc.document_type.clone();
        }

        if merged.sender.is_some()
            && doc.sender.is_some()
            && !same_participant(merged.sender.as_ref(), doc.sender.as_ref())
        {
            return Err(Iec62325Error::DocumentMismatch {
                path: path.display().to_string(),
                field: "sender",
                expected: format_participant(merged.sender.as_ref()),
                found: format_participant(doc.sender.as_ref()),
            });
        }
        if merged.sender.is_none() {
            merged.sender = doc.sender.clone();
        }

        if merged.receiver.is_some()
            && doc.receiver.is_some()
            && !same_participant(merged.receiver.as_ref(), doc.receiver.as_ref())
        {
            return Err(Iec62325Error::DocumentMismatch {
                path: path.display().to_string(),
                field: "receiver",
                expected: format_participant(merged.receiver.as_ref()),
                found: format_participant(doc.receiver.as_ref()),
            });
        }
        if merged.receiver.is_none() {
            merged.receiver = doc.receiver.clone();
        }

        Ok(())
    }

    fn same_participant(
        left: Option<&surge_network::network::market_data::MarketParticipant>,
        right: Option<&surge_network::network::market_data::MarketParticipant>,
    ) -> bool {
        match (left, right) {
            (Some(left), Some(right)) => {
                left.mrid == right.mrid && left.name == right.name && left.role == right.role
            }
            (None, None) => true,
            _ => false,
        }
    }

    let mut merged = MarketData::default();
    for p in paths {
        let xml = std::fs::read_to_string(p.as_ref())
            .map_err(|e| Iec62325Error::Io(format!("{}: {}", p.as_ref().display(), e)))?;
        let doc = parser::parse_document(&xml)?;
        merge_metadata(&mut merged, &doc, p.as_ref())?;
        merged.participants.extend(doc.participants);
        merged.time_series.extend(doc.time_series);
        merged.energy_schedules.extend(doc.energy_schedules);
        merged.bid_offers.extend(doc.bid_offers);
        merged
            .transmission_allocations
            .extend(doc.transmission_allocations);
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_market_doc(dir: &tempfile::TempDir, name: &str, xml: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, xml).unwrap();
        path
    }

    fn market_doc(doc_mrid: &str, sender_mrid: &str, ts_mrid: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<MarketDocument>
  <mRID>{doc_mrid}</mRID>
  <type>A25</type>
  <sender_MarketParticipant.mRID>{sender_mrid}</sender_MarketParticipant.mRID>
  <sender_MarketParticipant.marketRole.type>A04</sender_MarketParticipant.marketRole.type>
  <receiver_MarketParticipant.mRID>10X-RECEIVER</receiver_MarketParticipant.mRID>
  <receiver_MarketParticipant.marketRole.type>A08</receiver_MarketParticipant.marketRole.type>
  <TimeSeries>
    <mRID>{ts_mrid}</mRID>
    <businessType>A01</businessType>
    <Period>
      <timeInterval>
        <start>2024-01-01T00:00Z</start>
        <end>2024-01-01T01:00Z</end>
      </timeInterval>
      <resolution>PT60M</resolution>
      <Point>
        <position>1</position>
        <quantity>100.0</quantity>
      </Point>
    </Period>
  </TimeSeries>
</MarketDocument>"#
        )
    }

    #[test]
    fn test_parse_market_files_merges_compatible_documents() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_market_doc(
            &dir,
            "a.xml",
            &market_doc("doc-001", "10X-SENDER", "ts-001"),
        );
        let second = write_market_doc(
            &dir,
            "b.xml",
            &market_doc("doc-001", "10X-SENDER", "ts-002"),
        );

        let merged = parse_market_files(&[first.as_path(), second.as_path()]).unwrap();
        assert_eq!(merged.document_mrid.as_deref(), Some("doc-001"));
        assert_eq!(merged.time_series.len(), 2);
    }

    #[test]
    fn test_parse_market_files_rejects_mismatched_document_identity() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_market_doc(
            &dir,
            "a.xml",
            &market_doc("doc-001", "10X-SENDER", "ts-001"),
        );
        let second = write_market_doc(
            &dir,
            "b.xml",
            &market_doc("doc-002", "10X-SENDER", "ts-002"),
        );

        let err = parse_market_files(&[first.as_path(), second.as_path()]).unwrap_err();
        assert!(
            matches!(
                err,
                Iec62325Error::DocumentMismatch {
                    field: "document_mrid",
                    ..
                }
            ),
            "expected document mismatch error, got: {err}"
        );
    }
}
