# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
from __future__ import annotations

from enum import Enum

from .. import Network
from . import bin, cgmes, dss, epc, export, geo, ieee_cdf, json, matpower, profiles, psse, pypsa_nc, ucte, xiidm


class Format(str, Enum):
    MATPOWER = "matpower"
    PSSE_RAW = "psse"
    XIIDM = "xiidm"
    UCTE = "ucte"
    SURGE_JSON = "surge-json"
    DSS = "dss"
    EPC = "epc"


def loads(content: str, format: Format | str) -> Network: ...
def dumps(network: Network, format: Format | str, *, version: int | None = None) -> str: ...
