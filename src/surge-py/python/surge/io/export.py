# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from os import fspath

from .. import _native


def write_network_csv(network, output_dir) -> None:
    """Export network data (buses, branches, generators) to CSV files in a directory.

    Args:
        network: Power system network to export.
        output_dir: Directory where CSV files will be written.
    """
    _native._io_export_write_network_csv(network, fspath(output_dir))


def write_solution_snapshot(solution, network, output_path) -> None:
    """Write a power flow solution snapshot to a file.

    Args:
        solution: Solved power flow result (AcPfResult or DcPfResult).
        network: The network associated with the solution.
        output_path: Destination file path for the snapshot.
    """
    _native._io_export_write_solution_snapshot(solution, network, fspath(output_path))
