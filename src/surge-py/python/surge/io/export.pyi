# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from os import PathLike

from .. import AcPfResult, Network


def write_network_csv(network: Network, output_dir: str | PathLike[str]) -> None: ...
def write_solution_snapshot(
    solution: AcPfResult,
    network: Network,
    output_path: str | PathLike[str],
) -> None: ...
