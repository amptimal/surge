// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! PSS/E interchange helpers, organized by artifact type.

mod dyd_impl;
mod dyr_impl;
mod multi_terminal_dc;
#[path = "rawx.rs"]
mod rawx_impl;
mod reader;
mod seq_impl;
mod writer;

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use reader::{parse_file, parse_str};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use writer::{to_string, write_file};

pub mod raw {
    use std::path::Path;

    use surge_network::Network;

    pub use super::reader::PsseError as LoadError;
    pub use super::writer::PsseWriteError as SaveError;

    /// Supported RAW writer versions.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub struct Version(u32);

    impl Version {
        pub const V33: Self = Self(33);
        pub const V34: Self = Self(34);
        pub const V35: Self = Self(35);
        pub const V36: Self = Self(36);

        pub const fn new(value: u32) -> Self {
            Self(value)
        }

        pub const fn as_u32(self) -> u32 {
            self.0
        }
    }

    impl Default for Version {
        fn default() -> Self {
            Self::V33
        }
    }

    impl From<u32> for Version {
        fn from(value: u32) -> Self {
            Self::new(value)
        }
    }

    /// Load a PSS/E RAW case from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Network, LoadError> {
        super::reader::parse_file(path.as_ref())
    }

    /// Load a PSS/E RAW case from an in-memory string.
    pub fn loads(content: &str) -> Result<Network, LoadError> {
        super::reader::parse_str(content)
    }

    /// Save a PSS/E RAW case to disk.
    pub fn save(
        network: &Network,
        path: impl AsRef<Path>,
        version: impl Into<Version>,
    ) -> Result<(), SaveError> {
        super::writer::write_file(network, path.as_ref(), version.into().as_u32())
    }

    /// Serialize a PSS/E RAW case to an in-memory string.
    pub fn dumps(network: &Network, version: impl Into<Version>) -> Result<String, SaveError> {
        super::writer::to_string(network, version.into().as_u32())
    }
}

pub mod rawx {
    use std::path::Path;

    use surge_network::Network;

    pub use super::rawx_impl::RawxError as LoadError;

    /// Load a PSS/E RAWX case from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Network, LoadError> {
        super::rawx_impl::parse_file(path.as_ref())
    }

    /// Load a PSS/E RAWX case from an in-memory string.
    pub fn loads(content: &str) -> Result<Network, LoadError> {
        super::rawx_impl::parse_str(content)
    }
}

pub mod dyr {
    use std::path::Path;

    use surge_network::dynamics::DynamicModel;

    pub use super::dyr_impl::DyrError as LoadError;
    pub use super::dyr_impl::DyrWriteError as SaveError;

    /// Load a PSS/E DYR dynamics file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<DynamicModel, LoadError> {
        super::dyr_impl::parse_file(path.as_ref())
    }

    /// Load a PSS/E DYR dynamics file from an in-memory string.
    pub fn loads(content: &str) -> Result<DynamicModel, LoadError> {
        super::dyr_impl::parse_str(content)
    }

    /// Save a PSS/E DYR dynamics file to disk.
    pub fn save(model: &DynamicModel, path: impl AsRef<Path>) -> Result<(), SaveError> {
        super::dyr_impl::write_dyr(model, path.as_ref())
    }

    /// Serialize a PSS/E DYR dynamics file to an in-memory string.
    pub fn dumps(model: &DynamicModel) -> Result<String, SaveError> {
        super::dyr_impl::to_dyr_string(model)
    }
}

pub mod dyd {
    pub use super::dyd_impl::{DydRecord as Record, Error};

    /// Load PSS/E DYD records from an in-memory string.
    pub fn loads(content: &str) -> Result<Vec<Record>, Error> {
        super::dyd_impl::loads(content)
    }
}

pub mod sequence {
    use std::path::Path;

    use surge_network::Network;

    pub use super::seq_impl::{SeqError as Error, SeqStats as Stats};

    /// Apply a PSS/E sequence-data sidecar file to an existing network.
    pub fn apply(network: &mut Network, path: impl AsRef<Path>) -> Result<Stats, Error> {
        super::seq_impl::parse_file(network, path.as_ref())
    }

    /// Apply PSS/E sequence-data from an in-memory string to an existing network.
    pub fn apply_text(network: &mut Network, content: &str) -> Result<Stats, Error> {
        super::seq_impl::parse_str(network, content)
    }
}
