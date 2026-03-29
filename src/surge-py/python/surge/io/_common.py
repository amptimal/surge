# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from os import fspath

from .. import _native


def as_path(path: str | object) -> str:
    return fspath(path)


def load_as(path: str | object, format_name: str):
    return _native._load_as(as_path(path), format_name)


def loads_as(content: str, format_name: str):
    return _native._loads(content, format_name)


def loads_bytes_as(content: bytes, format_name: str):
    return _native._loads_bytes(content, format_name)


def save_as(network, path: str | object, format_name: str, version: int | None = None) -> None:
    _native._save_as(network, as_path(path), format_name, version)


def dumps_as(network, format_name: str, version: int | None = None) -> str:
    return _native._dumps(network, format_name, version)


def dumps_bytes_as(network, format_name: str) -> bytes:
    return _native._dumps_bytes(network, format_name)


def save_surge_json(network, path: str | object, *, pretty: bool = False) -> None:
    _native._io_json_save(network, as_path(path), pretty)


def dumps_surge_json(network, *, pretty: bool = False) -> str:
    return _native._io_json_dumps(network, pretty)
