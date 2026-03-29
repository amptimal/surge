# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from os import PathLike

from ... import Network
from ..._surge import _SeqStats as SeqStats


def apply(network: Network, path: str | PathLike[str]) -> SeqStats: ...
def apply_text(network: Network, content: str) -> SeqStats: ...
