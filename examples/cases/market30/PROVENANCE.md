# market30 Bundle Provenance

Modified IEEE 30-bus system designed for wholesale electricity market
simulation tutorials and regression testing.

## Derivation

- **Base case**: IEEE 30-bus system (`surge.case30()`)
- **Modifications** applied by `build.py`:
  - Replaced 6 original generators with a 13-resource fleet covering
    coal, gas CC, gas CT (x2), nuclear, wind, solar, BESS, and pumped hydro
  - Added VSC-HVDC tie between bus 12 (area 2) and bus 30 (area 3)
  - Added 3 dispatchable loads (curtailable, interruptible, elastic)
  - Added flowgate "North-South" and interface "Area1-Area2"
  - Doubled all branch ratings for feasibility with scaled load
  - Scaled loads 1.8x (total ~340 MW)

## Regeneration

```bash
source .venv/bin/activate
python examples/cases/market30/build.py
```
