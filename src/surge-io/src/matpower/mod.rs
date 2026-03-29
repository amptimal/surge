// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! MATPOWER (.m) format reader and writer.

mod reader;
mod writer;

use std::path::Path;

use surge_network::Network;

pub use reader::MatpowerError as LoadError;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use reader::{parse_file, parse_str};
pub use writer::MatpowerWriteError as SaveError;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use writer::{to_string, write_file};

/// Load a MATPOWER case from disk.
pub fn load(path: impl AsRef<Path>) -> Result<Network, LoadError> {
    reader::parse_file(path.as_ref())
}

/// Load a MATPOWER case from an in-memory string.
pub fn loads(content: &str) -> Result<Network, LoadError> {
    reader::parse_str(content)
}

/// Save a MATPOWER case to disk.
pub fn save(network: &Network, path: impl AsRef<Path>) -> Result<(), SaveError> {
    writer::write_file(network, path.as_ref())
}

/// Serialize a MATPOWER case to an in-memory string.
pub fn dumps(network: &Network) -> Result<String, SaveError> {
    writer::to_string(network)
}
