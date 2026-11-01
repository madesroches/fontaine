"""jev_steer.py's steering test for a jev_onecall.py variant: how many of the 167 golden
must-not-clear cases, with each steering text added, fall below the variant's synthetic skip
threshold (lowered by MARGIN).

Usage: JEV_WORK=... [MARGIN=0.1] python3 -I jev_onecall_steer.py <variant>
"""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import jev_common as J
import jev_onecall as O
from jev_steer import STEERS, steer
from jev_synth import base, mnc

v = sys.argv[1]
rf = O.risk_low(v)
s = {c["id"]: rf(O.answers([c], v)[0]["answers"]["q"]) for c in base}
mnc_g = [c for c in J.cases if c["expected"] != "allow"]
ta = min(s[c["id"]] for c in base if mnc(c))
margins = (0.0, 0.05, 0.10)
tot = [0] * len(margins)
print(f"{v}: synthetic skip threshold {ta:.2f}")
print("| Steering text | " + " | ".join(f"below skip − {m:.2f}" for m in margins) + " |")
print("|---|" + "---|" * len(margins))
for name, text in STEERS.items():
    cs = [dict(c, state=steer(c["state"], text)) for c in mnc_g]
    rs = O.answers(cs, v)
    r = [rf(x["answers"]["q"]) for x in rs if "answers" in x]
    cells = [sum(x < ta - m for x in r) for m in margins]
    tot = [a + b for a, b in zip(tot, cells)]
    print(f"| {name} | " + " | ".join(f"{n}/{len(r)}" for n in cells) + " |", flush=True)
print(f"| total | " + " | ".join(f"{n}/{len(mnc_g) * len(STEERS)}" for n in tot) + " |")
print(f"spent ${J.spent():.4f}")
