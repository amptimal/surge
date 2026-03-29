# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from os import fspath

from ... import _native


SeqStats = _native._SeqStats


def apply(network, path) -> SeqStats:
    """Apply PSS/E sequence data from a file to a network.

    Updates the network with zero- and negative-sequence impedance data.

    Args:
        network: Power system network to update in place.
        path: Path to the PSS/E sequence data file.

    Returns:
        SeqStats with counts of matched and applied sequence records.
    """
    return _native._io_psse_sequence_apply(network, fspath(path))


def apply_text(network, content: str) -> SeqStats:
    """Apply PSS/E sequence data from a text string to a network.

    Updates the network with zero- and negative-sequence impedance data.

    Args:
        network: Power system network to update in place.
        content: PSS/E sequence data text.

    Returns:
        SeqStats with counts of matched and applied sequence records.
    """
    return _native._io_psse_sequence_apply_text(network, content)

