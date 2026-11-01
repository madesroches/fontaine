"""Repeat-run variance and alias stability: re-asks framings with the cache bypassed, and through
`~typesafe/jev-latest`, and compares with the first (cached) pass and the committed scorecard.

Usage: JEV_WORK=... python3 -I jev_variance.py name[,name...] [repeats]
"""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import jev_common as J
from jev_framings import FRAMINGS, body

def risks(name, model=J.PINNED, fresh=False):
    resps = [J.post(body(name, c, model), fresh=fresh) for c in J.cases]
    r = {c["id"]: FRAMINGS[name][2](x["answers"]["q"]) for c, x in zip(J.cases, resps) if "answers" in x}
    return r, sorted({x.get("model") for x in resps})

def compare(tag, a, b):
    ids = [i for i in a if i in b]
    d = [abs(a[i] - b[i]) for i in ids]
    same = sum(x == 0 for x in d)
    print(f"  {tag:34} identical {same}/{len(ids)}  mean|d| {sum(d)/len(d):.4f}  max|d| {max(d):.3f}  "
          f"-> {J.fmt(J.metrics(b))}")

if __name__ == "__main__":
    reps = int(sys.argv[2]) if len(sys.argv) > 2 else 2
    for n in sys.argv[1].split(","):
        first, m0 = risks(n)
        print(f"{n}: first pass {m0} {J.fmt(J.metrics(first))}")
        if n == "choice3_raw":
            compare("committed scorecard vs first", first, J.baseline())
        for k in range(reps):
            r, m = risks(n, fresh=True)
            compare(f"fresh repeat {k + 1} {m}", first, r)
        r, m = risks(n, model="~typesafe/jev-latest", fresh=True)
        compare(f"alias {m}", first, r)
    print(f"spent ${J.spent():.4f}")
