# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from os import fspath

from ... import _native


DynamicModel = _native._DynamicModel


def load(path) -> DynamicModel:
    """Load a PSS/E dynamic model from a .dyr file.

    Args:
        path: Path to the .dyr file.

    Returns:
        DynamicModel with generator and exciter dynamic data.
    """
    return _native._io_psse_dyr_load(fspath(path))


def loads(content: str) -> DynamicModel:
    """Parse a PSS/E dynamic model from DYR text content.

    Args:
        content: DYR file text.

    Returns:
        DynamicModel with generator and exciter dynamic data.
    """
    return _native._io_psse_dyr_loads(content)


def save(model: DynamicModel, path) -> None:
    """Save a PSS/E dynamic model to a .dyr file.

    Args:
        model: DynamicModel to save.
        path: Destination file path.
    """
    _native._io_psse_dyr_save(model, fspath(path))


def dumps(model: DynamicModel) -> str:
    """Serialize a PSS/E dynamic model to DYR text format.

    Args:
        model: DynamicModel to serialize.

    Returns:
        DYR format string.
    """
    return _native._io_psse_dyr_dumps(model)

