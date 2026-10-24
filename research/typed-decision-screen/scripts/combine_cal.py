"""Combines two calibrated screens into one: each score becomes its percentile among the synthetic
allowed cases (so both are on the same scale), and the combined risk is their mean or max. Writes
cascade-calibration-<out>.json for cascade_live.py.

Usage: combine_cal.py <tag a | laya> <tag b> <mean|max> <out tag>
"""
import os, json, sys, bisect
D = os.environ["SCREEN_WORK"] + "/exp"
ids = json.load(open(os.environ["SCREEN_WORK"] + "/trainds/ids.json"))
def load(t): return json.load(open(D + "/cascade-calibration" + ("" if t == "laya" else "-" + t) + ".json"))
a, b = load(sys.argv[1]), load(sys.argv[2])
def pct(cal):
    allowed = sorted(v for k, v in cal["synthetic_risk"].items() if not k.startswith("aug-") and ids[k] == "allow")
    return lambda x: bisect.bisect_left(allowed, x) / len(allowed) + 1e-6 * x
pa, pb = pct(a), pct(b)
op = (lambda x, y: (x + y) / 2) if sys.argv[3] == "mean" else max
comb = lambda k, src: op(pa(a[src][k]), pb(b[src][k]))
syn = {k: comb(k, "synthetic_risk") for k in a["synthetic_risk"]}
gold = {k: comb(k, "golden_risk") for k in a["golden_risk"]}
allowed = [v for k, v in syn.items() if (k.startswith("aug-") is False and ids[k] == "allow")]
stop = [v for k, v in syn.items() if k.startswith("aug-") or ids[k] != "allow"]
ms = {k: a["screen_ms"][k] + b["screen_ms"][k] for k in ("p50", "p95", "mean")}
json.dump({"t_stop": max(allowed) + 1e-9, "t_skip": min(stop), "synthetic_risk": syn, "golden_risk": gold, "screen_ms": ms},
          open(f"{D}/cascade-calibration-{sys.argv[4]}.json", "w"))
