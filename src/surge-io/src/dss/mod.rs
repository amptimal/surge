// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! OpenDSS (.dss) file parser for Surge.
//!
//! Parses OpenDSS distribution system simulation script files into a
//! `surge_network::Network` suitable for power flow analysis.
//!
//! ## Supported elements
//!
//! | Element        | Notes                                      |
//! |----------------|--------------------------------------------|
//! | Circuit        | Source bus (slack), base kV, frequency     |
//! | Line           | r/x sequences, full matrix, linecode ref   |
//! | LineCode       | Named impedance library entries            |
//! | LineGeometry   | Tower geometry → Carson equations          |
//! | WireData       | Overhead conductor properties              |
//! | CNData         | Concentric-neutral underground cable       |
//! | TSData         | Tape-shield underground cable              |
//! | Transformer    | 2- and 3-winding, delta/wye connections    |
//! | AutoTrans      | Autotransformer (same model)               |
//! | XfmrCode       | Named transformer library entries          |
//! | Load           | Models 1–8, ZIP, daily/yearly shapes       |
//! | Generator      | Dispatchable generator (PV bus)            |
//! | PVSystem       | Solar inverter                             |
//! | Storage        | Battery with charge/discharge              |
//! | Capacitor      | Shunt capacitor bank (bus admittance)      |
//! | Reactor        | Shunt reactor (bus admittance)             |
//! | SwtControl     | Switch control device                      |
//! | Recloser       | Recloser protective device                 |
//! | VSConverter    | Voltage-source converter                   |
//! | LoadShape      | Per-unit time-series multipliers           |
//! | Fault          | Fault element (structural, not simulated)  |
//! | GicLine        | GIC line element                           |
//! | GicTransformer | GIC transformer element                    |
//!
//! ## Usage
//! ```no_run
//! use surge_io::dss::{load, loads};
//! use std::path::Path;
//!
//! // From a file:
//! let network = load(Path::new("ieee13.dss")).unwrap();
//!
//! // From a string:
//! let dss = r#"
//!     Clear
//!     New Circuit.test basekv=4.16
//!     New Line.L1 Bus1=SourceBus Bus2=bus2 r1=0.1 x1=0.3 length=1 units=mi
//!     New Load.LD1 Bus1=bus2 kv=4.16 kw=1000 kvar=300
//!     Solve
//! "#;
//! let network = loads(dss).unwrap();
//! ```

mod command;
mod lexer;
mod load_shape;
mod objects;
mod resolve;
mod to_network;
mod writer;

use std::path::Path;

use surge_network::Network;

pub use load_shape::LoadShape;
pub use to_network::DssParseError as LoadError;
pub use writer::DssWriteError as SaveError;

/// Load an OpenDSS case from disk.
pub fn load(path: impl AsRef<Path>) -> Result<Network, LoadError> {
    to_network::parse_dss(path.as_ref())
}

/// Load an OpenDSS case from an in-memory string.
pub fn loads(content: &str) -> Result<Network, LoadError> {
    to_network::parse_dss_str(content)
}

/// Save an OpenDSS case to disk.
pub fn save(network: &Network, path: impl AsRef<Path>) -> Result<(), SaveError> {
    writer::write_dss(network, path.as_ref())
}

/// Serialize an OpenDSS case to an in-memory string.
pub fn dumps(network: &Network) -> Result<String, SaveError> {
    writer::to_dss_string(network)
}
