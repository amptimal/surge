// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
//! Transfer sensitivity matrix types and entry points.

use faer::Mat;
use surge_network::Network;
use tracing::info;

use crate::dfax::PreparedTransferModel;
use crate::error::TransferError;

/// Generation Shift Factor matrix.
///
/// `GSF[l, g]` = change in per-unit flow on branch `l` per 1 p.u. injection
/// increase at generator `g`'s bus (slack absorbs the difference).
///
/// Because PTDF[l, slack] = 0 by definition this is simply:
///   `GSF[l, g] = PTDF[l, bus_g_idx]`
///
/// Dimensions: **n_branches × n_in_service_generators** (row = branch, column = generator).
///
/// This matches the standard branch-by-generator layout used in transmission
/// planning tools and external study exports.
pub struct GsfMatrix {
    /// Values (n_branches × n_generators), row-major.
    pub values: Mat<f64>,
    /// Branch index for row `l`.
    pub branch_ids: Vec<usize>,
    /// External bus number for generator column `g`.
    pub gen_buses: Vec<u32>,
}

/// Bus Load Distribution Factor matrix.
///
/// `BLDF[b, l]` = change in per-unit flow on branch `l` per 1 p.u. load
/// increase at bus `b`. A load increase withdraws power, so:
///   `BLDF[b, l] = -PTDF[l, b]`
///
/// Dimensions: **n_buses × n_branches**.
pub struct BldfMatrix {
    /// Values (n_buses × n_branches), row-major.
    pub values: Mat<f64>,
}

/// Compute Generation Shift Factors in canonical branch-by-generator orientation.
pub fn compute_gsf(network: &Network) -> Result<GsfMatrix, TransferError> {
    let n_gen_total = network.generators.iter().filter(|g| g.in_service).count();
    info!(
        generators = n_gen_total,
        branches = network.n_branches(),
        "computing GSF matrix"
    );
    PreparedTransferModel::new(network)?.compute_gsf()
}

/// Compute Bus Load Distribution Factors.
pub fn compute_bldf(network: &Network) -> Result<BldfMatrix, TransferError> {
    info!(
        buses = network.n_buses(),
        branches = network.n_branches(),
        "computing BLDF matrix"
    );
    PreparedTransferModel::new(network)?.compute_bldf()
}
