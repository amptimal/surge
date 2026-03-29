#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""
Compare OpenDSS and Surge regulator tap positions for IEEE test feeders.
Also print per-bus per-phase voltage comparison to identify systematic errors.
"""
from __future__ import annotations
import os, sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
DSS_DIR = REPO_ROOT / "tests" / "data" / "dss"

FEEDERS = {
    "ieee13": {"path": "ieee13/IEEE13Nodeckt.dss"},
    "ieee34": {"path": "ieee34/ieee34Mod1.dss"},
    "ieee123": {"path": "ieee123/IEEE123Master.dss"},
}

def get_opendss_taps_and_voltages(feeder: str):
    import opendssdirect as dss

    cfg = FEEDERS[feeder]
    dss_file = DSS_DIR / cfg["path"]
    dss_dir = dss_file.parent
    orig_dir = os.getcwd()
    os.chdir(str(dss_dir))

    try:
        dss.Text.Command("Clear")
        try:
            dss.Text.Command(f"Redirect {dss_file.name}")
        except Exception:
            dss.Text.Command("Clear")
            for line in dss_file.read_text().splitlines():
                stripped = line.strip()
                if not stripped or stripped.startswith("!"):
                    continue
                if stripped.lower().startswith("buscoords"):
                    continue
                try:
                    dss.Text.Command(stripped)
                except Exception:
                    pass

        dss.Text.Command("Solve")

        # Get regulator tap positions
        taps = {}
        reg_names = []
        dss.RegControls.First()
        while True:
            name = dss.RegControls.Name()
            if not name:
                break
            tap = dss.RegControls.TapNumber()
            xfmr = dss.RegControls.Transformer()
            winding = dss.RegControls.Winding()
            vreg = dss.RegControls.ForwardVreg()
            band = dss.RegControls.ForwardBand()
            reg_names.append(name)
            taps[name] = {
                "tap_number": tap,
                "transformer": xfmr,
                "winding": winding,
                "vreg": vreg,
                "band": band,
            }
            if not dss.RegControls.Next():
                break

        # Get per-bus voltages
        voltages = {}
        for bus_name in dss.Circuit.AllBusNames():
            dss.Circuit.SetActiveBus(bus_name)
            vmag_angle = dss.Bus.puVmagAngle()
            nodes = dss.Bus.Nodes()
            phases = {}
            for p in range(len(nodes)):
                phase = int(nodes[p])
                mag = vmag_angle[2 * p]
                phases[phase] = mag
            voltages[bus_name.lower()] = phases

        loss_kw = dss.Circuit.Losses()[0] / 1000.0

        return taps, voltages, loss_kw
    finally:
        os.chdir(orig_dir)

feeder = sys.argv[1] if len(sys.argv) > 1 else "ieee13"
print(f"\n{'='*60}")
print(f"  {feeder} — OpenDSS Regulator Tap Positions")
print(f"{'='*60}")

taps, voltages, loss_kw = get_opendss_taps_and_voltages(feeder)
for name, info in taps.items():
    print(f"  {name}: tap_number={info['tap_number']}, "
          f"xfmr={info['transformer']}, winding={info['winding']}, "
          f"vreg={info['vreg']}, band={info['band']}")

print(f"\n  Total losses: {loss_kw:.3f} kW")

# Print top-10 buses with highest voltage and lowest voltage
print(f"\n  Highest voltage buses:")
all_phases = []
for bus, phases in voltages.items():
    for ph, mag in phases.items():
        all_phases.append((bus, ph, mag))
all_phases.sort(key=lambda x: -x[2])
for bus, ph, mag in all_phases[:5]:
    print(f"    {bus} ph{ph}: {mag:.6f} pu")

print(f"\n  Lowest voltage buses:")
all_phases.sort(key=lambda x: x[2])
for bus, ph, mag in all_phases[:10]:
    print(f"    {bus} ph{ph}: {mag:.6f} pu")
