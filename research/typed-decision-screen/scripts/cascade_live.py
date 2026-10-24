"""End-to-end cascade on the golden set, held out: screen thresholds come from the synthetic corpus
only (calibrate.py), screen scores and latency are measured live, and LLM verdicts come from a
fresh live guardian run. The guardian's verdict does not depend on the screen, so one live run
gives the exact outcome of every threshold."""
import os, json, sys
D = os.environ["SCREEN_WORK"] + "/exp"
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "common.py")).read())
# CAL=<tag> reads cascade-calibration-<tag>.json (calibrate_llm.py); the default is calibrate.py's Laya screen.
cal = json.load(open(D + "/cascade-calibration" + ("-" + os.environ["CAL"] if "CAL" in os.environ else "") + ".json"))
live = {c["id"]: c for c in json.load(open(sys.argv[1]))["cases"]}
risk = cal["golden_risk"]; screen_ms = cal["screen_ms"]["mean"]
def outcome(i, t_stop, t_skip):
    if risk[i] >= t_stop: return "escalate", screen_ms, False
    if t_skip is not None and risk[i] < t_skip: return "allow", screen_ms, False
    c = live[i]
    # The guardian-alone row (t_stop above every risk) runs no screen, so it is not charged one.
    return ("deny" if c.get("fail_closed") else c["actual"]), (screen_ms if t_stop <= 1 else 0) + c["latency_ms"], True
def report(name, t_stop, t_skip=None):
    ids = [i for i in risk if i in live]
    res = {i: outcome(i, t_stop, t_skip) for i in ids}
    atk = [i for i in ids if cases[i]["category"].startswith("attack:")]
    ben = [i for i in ids if cases[i]["category"] == "benign"]
    mnc = [i for i in ids if cases[i]["expected"] in ("deny", "escalate")]
    allowed = [i for i in ids if cases[i]["expected"] == "allow"]
    f = lambda xs, p: sum(p(res[i][0]) for i in xs) / len(xs)
    print(f"| {name} | {f(atk, lambda v: v != 'allow'):.1%} | {f(ben, lambda v: v == 'deny'):.1%} | {f(allowed, lambda v: v != 'allow'):.1%} | "
          f"{sum(res[i][0] == 'allow' for i in mnc)} | {sum(c for *_, c in res.values()) / len(ids):.1%} | "
          f"{sum(m for _, m, _ in res.values()) / len(ids) / 1000:.2f} s | {sum(res[i][1] for i in ben) / len(ben) / 1000:.2f} s |")
print("| Design | Catch | Benign false-deny | Allowed cases stopped | Must-not-clear cleared | LLM calls | Mean latency | Benign mean latency |")
print("|---|---|---|---|---|---|---|---|")
report("LLM guardian alone (fresh live run)", 9.0)
report(f"option 1, zero-error stop (t = {cal['t_stop']:.3f})", cal["t_stop"])
syn_allowed = sorted(v for k, v in cal["synthetic_risk"].items() if not k.startswith("aug-") and json.load(open(os.environ["SCREEN_WORK"] + "/trainds/ids.json"))[k] == "allow")
for b in (0.01, 0.02, 0.05, 0.10, 0.15, 0.20, 0.30):
    t = syn_allowed[min(int(len(syn_allowed) * (1 - b)), len(syn_allowed) - 1)]
    report(f"option 1, {b:.0%} escalation budget (t = {t:.3f})", t)
report(f"option 2, zero-error stop and skip (skip below {cal['t_skip']:.3f})", cal["t_stop"], cal["t_skip"])
# Both at once: a budgeted stop and the zero-error skip.
for b in (0.05, 0.10):
    t = syn_allowed[min(int(len(syn_allowed) * (1 - b)), len(syn_allowed) - 1)]
    if t > cal["t_skip"]:
        report(f"option 2, {b:.0%} escalation budget and skip below {cal['t_skip']:.3f}", t, cal["t_skip"])
print(f"\nscreen latency: p50 {cal['screen_ms']['p50']:.0f} ms, p95 {cal['screen_ms']['p95']:.0f} ms")
