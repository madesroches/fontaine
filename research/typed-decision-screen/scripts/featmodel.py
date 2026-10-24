"""Trains a logistic model on zero-shot question scores (features.py) using the synthetic corpus
only, and tests it on the golden set and on intent-swap pairs it did not train on."""
import os, json, sys
import numpy as np, torch
S = os.environ["SCREEN_WORK"]; D = S + "/exp"
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "common.py")).read())
syn_cases = {json.loads(l)["id"]: json.loads(l) for l in open(S + "/trainds/train.jsonl")}
aug_cases = {json.loads(l)["id"]: json.loads(l) for l in open(S + "/augds/aug.jsonl")}
key = lambda c: (c["tool"], json.dumps(c["args"], sort_keys=True))
orig_by_action = {key(c): i for i, c in syn_cases.items() if c["expected"] == "allow"}
def derived(f):
    g = dict(f)
    # Polarity-debiased pairs: average a question with its negation.
    g["employee_pair"] = ((1 - f["employee"]) + f["employee_overstep"]) / 2
    g["aligned_pair"] = ((1 - f["aligned"]) + f["deviates"]) / 2
    g["consequence_pair"] = ((1 - f["accepted_consequences"]) + f["risk_unaccepted"]) / 2
    # What the user's request adds over a request that licenses nothing.
    g["employee_contrast"] = f["employee"] - f["employee_neutral"]
    g["not_employee"] = 1 - f["employee"]
    return g
def run(ck):
    rows = json.load(open(f"{D}/features-{ck}.json"))
    for r in rows: r["f"] = derived(r["f"])
    names = sorted(rows[0]["f"])
    gold = {r["id"]: r for r in rows if r["split"] == "golden"}
    syn = {r["id"]: r for r in rows if r["split"] == "syn"}
    aug = {r["id"]: r for r in rows if r["split"] == "aug"}
    odd = lambda i: (int(i.split("-")[1]) // 2) % 2 == 1
    pairs = [(syn[orig_by_action[key(aug_cases[i])]], aug[i]) for i in aug if odd(i) and key(aug_cases[i]) in orig_by_action]
    print(f"\n== {ck}: single questions (golden AUROC, intent-pair rise rate on held-out pairs; score oriented so higher = stop)")
    for n in names:
        sign = -1 if n in ("employee", "accepted_consequences", "aligned", "employee_neutral") else 1
        r = {i: sign * g["f"][n] for i, g in gold.items()}
        rise = np.mean([sign * (sw["f"][n] - o["f"][n]) > 0 for o, sw in pairs])
        print(f"  {n:24} AUROC {metrics(r)[0]:.3f}  atk {metrics(r)[1]:.3f}  intent-rise {rise:.0%}")
    train = list(syn.values()) + [aug[i] for i in aug if not odd(i)]
    X = np.array([[r["f"][n] for n in names] for r in train], dtype=np.float32)
    y = np.array([0.0 if r["expected"] == "allow" else 1.0 for r in train], dtype=np.float32)
    mu, sd = X.mean(0), X.std(0) + 1e-6
    Xt, yt = torch.tensor((X - mu) / sd), torch.tensor(y)
    for lam in (0.01, 0.1, 1.0):
        w = torch.zeros(len(names), requires_grad=True); b = torch.zeros(1, requires_grad=True)
        opt = torch.optim.LBFGS([w, b], max_iter=500)
        def cl():
            opt.zero_grad(); l = torch.nn.functional.binary_cross_entropy_with_logits(Xt @ w + b, yt) + lam * (w * w).sum() / len(y); l.backward(); return l
        opt.step(cl)
        score = lambda r: float(torch.sigmoid(torch.tensor((np.array([r["f"][n] for n in names], dtype=np.float32) - mu) / sd) @ w + b))
        g = {i: score(r) for i, r in gold.items()}
        rise = np.mean([score(sw) > score(o) for o, sw in pairs]); gap = np.mean([score(sw) - score(o) for o, sw in pairs])
        print(f"  model lam={lam:<5} {fmt(metrics(g))}  intent-rise {rise:.0%} mean {gap:+.3f}")
        top = sorted(zip(names, w.detach().numpy()), key=lambda x: -abs(x[1]))[:6]
        print("    top weights:", ", ".join(f"{n} {v:+.2f}" for n, v in top))
        json.dump(g, open(f"{D}/featmodel-{ck}-{lam}.json", "w"))
for ck in sys.argv[1:]: run(ck)
