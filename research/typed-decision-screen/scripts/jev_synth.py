"""Out-of-sample check of the framings on the synthetic corpus (training/*.jsonl, written from the
guardian policy without reading the golden set), and threshold transfer from it to the golden set.

Usage: JEV_WORK=... JEV_CASES=$JEV_WORK/synthds/cases.json python3 -I jev_synth.py [name,...]
Needs $JEV_WORK/risks/<name>.json from jev_framings.py for the transfer step.
Writes $JEV_WORK/synth_risks/<name>.json.
"""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from jev_common import cases, auroc, spent, W
from jev_framings import FRAMINGS, score

GOLD = {c["id"]: c for c in json.load(open(f"{W}/exp/cases.json"))}
SYN_CASES = json.load(open(f"{W}/synthds/cases.json"))
key = lambda c: (c["tool"], json.dumps(c["args"], sort_keys=True))
base = [c for c in SYN_CASES if not c["id"].startswith("aug-")]
orig = {key(c): c["id"] for c in base if c["expected"] == "allow"}
pairs = [(orig[key(c)], c["id"]) for c in SYN_CASES if c["id"].startswith("aug-") and key(c) in orig]

def mnc(c): return c["expected"] in ("deny", "escalate")

def transfer(syn, gold):
    """Thresholds set on the synthetic base set only, applied to the golden set."""
    s_pos = [syn[c["id"]] for c in base if mnc(c)]
    s_neg = sorted(syn[c["id"]] for c in base if not mnc(c))
    t0 = min(s_pos)
    q = lambda f: s_neg[min(int(len(s_neg) * (1 - f)), len(s_neg) - 1)] + 1e-12
    g = lambda pred: [i for i, c in GOLD.items() if pred(c)]
    ben, atk, gm = g(lambda c: c["category"] == "benign"), g(lambda c: c["category"].startswith("attack:")), g(lambda c: c["expected"] != "allow")
    out = {"clr0": sum(gold[i] < t0 for i in ben) / len(ben), "miss0": sum(gold[i] < t0 for i in gm)}
    for f in (0.05, 0.10, 0.20):
        t = q(f)
        out[f"c{int(f*100)}"] = sum(gold[i] >= t for i in atk) / len(atk)
        out[f"b{int(f*100)}"] = sum(gold[i] >= t for i in ben) / len(ben)
    return out

if __name__ == "__main__":
    names = sys.argv[1].split(",") if len(sys.argv) > 1 else list(FRAMINGS)
    os.makedirs(f"{W}/synth_risks", exist_ok=True)
    print(f"synthetic base: {sum(mnc(c) for c in base)} must-not-clear, {sum(not mnc(c) for c in base)} allowed; {len(pairs)} intent pairs")
    for n in names:
        r, bad, _ = score(n)
        json.dump(r, open(f"{W}/synth_risks/{n}.json", "w"))
        pos = [r[c["id"]] for c in base if mnc(c)]; neg = [r[c["id"]] for c in base if not mnc(c)]
        a_all = auroc(pos + [r[b] for _, b in pairs], neg)
        rise = sum(r[b] > r[a] for a, b in pairs) / len(pairs)
        line = f"{n:18} fail={len(bad)} synth AUROC {auroc(pos, neg):.3f} (+swaps {a_all:.3f}) intent-rise {rise:.0%}"
        gp = f"{W}/risks/{n}.json"
        if os.path.exists(gp):
            t = transfer(r, json.load(open(gp)))
            line += (f" | synth->golden: clr@0miss {t['clr0']:.1%} (mnc cleared {t['miss0']}) "
                     f"stop@5% catch {t['c5']:.1%} ben {t['b5']:.1%}; @10% {t['c10']:.1%}/{t['b10']:.1%}; @20% {t['c20']:.1%}/{t['b20']:.1%}")
        print(line, flush=True)
    print(f"spent ${spent():.4f}")
