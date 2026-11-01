"""Offline, from the cache: every answer recorded for the same request body (the first pass plus the
fresh repeats of jev_variance.py). Shows how the synthetic-set thresholds hold on each repeat, and
whether averaging repeats (a self-ensemble) helps.

Usage: JEV_WORK=... python3 -I jev_repeats.py name[,name...]
"""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import jev_common as J
from jev_framings import FRAMINGS, body
from jev_synth import transfer

ALL = {}
for l in open(J.CACHE):
    r = json.loads(l); ALL.setdefault(r["k"], []).append(r["resp"])

for n in sys.argv[1].split(","):
    syn = json.load(open(f"{J.W}/synth_risks/{n}.json"))
    reps = []
    for c in J.cases:
        rs = [x for x in ALL[J._hash(body(n, c))] if x.get("model") == "typesafe/jev-1.14-20261014"]
        reps.append((c["id"], [FRAMINGS[n][2](x["answers"]["q"]) for x in rs]))
    k = min(len(v) for _, v in reps)
    print(f"{n}: {k} pinned passes per case")
    for j in range(k):
        g = {i: v[j] for i, v in reps}
        t = transfer(syn, g)
        print(f"  pass {j + 1}: {J.fmt(J.metrics(g))} | synth->golden clr@0miss {t['clr0']:.1%} (mnc cleared {t['miss0']}) stop@5% {t['c5']:.1%}/{t['b5']:.1%}")
    g = {i: sum(v[:k]) / k for i, v in reps}
    t = transfer(syn, g)
    print(f"  mean of {k}: {J.fmt(J.metrics(g))} | synth->golden clr@0miss {t['clr0']:.1%} (mnc cleared {t['miss0']}) stop@5% {t['c5']:.1%}/{t['b5']:.1%}")
