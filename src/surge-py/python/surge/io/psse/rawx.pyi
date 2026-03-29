# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from os import PathLike

from ... import Network


def load(path: str | PathLike[str]) -> Network: ...
def loads(content: str) -> Network: ...
