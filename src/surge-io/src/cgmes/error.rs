// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
use std::path::PathBuf;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum CgmesError {
    #[error("cannot read CGMES directory {path}: {source}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read directory entry in {path}: {source}")]
    ReadDirectoryEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("no CGMES XML profiles found in {path}")]
    NoProfiles { path: PathBuf },
    #[error("cannot open CGMES archive {path}: {source}")]
    OpenArchive {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read CGMES zip archive {path}: {source}")]
    ReadArchive {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
    #[error("cannot read zip entry in {path}: {source}")]
    ReadArchiveEntry {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
    #[error("cannot create temporary directory for CGMES archive extraction: {0}")]
    CreateTempDir(#[source] std::io::Error),
    #[error("cannot extract archive entry to {path}: {source}")]
    ExtractArchiveEntry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("zip archive {archive_path} contains an unsafe CGMES entry path: {entry_name}")]
    InvalidArchiveEntryPath {
        archive_path: PathBuf,
        entry_name: String,
    },
    #[error("zip archive {archive_path} contains duplicate CGMES profile paths: {entry_name}")]
    DuplicateArchiveEntryPath {
        archive_path: PathBuf,
        entry_name: String,
    },
    #[error("unsupported CGMES input: {0}")]
    UnsupportedInput(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error(
        "CGMES dataset is missing the SSH operating-point profile; include SSH for generator/load/converter set-points"
    )]
    MissingSshProfile,
    #[error(
        "CGMES dataset contains {count} unresolved BaseVoltage reference(s): {examples:?}. \
         Include the referenced EQ/EQBD BaseVoltage objects before loading."
    )]
    MissingBaseVoltageReferences { count: usize, examples: Vec<String> },
    #[error(
        "no TopologicalNodes found — include a TP profile, or provide a bus-breaker model \
         (ConnectivityNode + Switch) so topology can be synthesized automatically"
    )]
    NoTopology,
    /// CIM-03: object store size cap exceeded (protects against >1 GB allocations from
    /// malformed or adversarial CIM files).
    #[error("CIM file contains too many objects: {0}")]
    TooManyObjects(String),
}
