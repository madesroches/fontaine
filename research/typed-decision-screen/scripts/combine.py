"""Combines an action-risk question (danger) with a task-relation question (employee permission):
the first ignores the task, the second ignores the action's risk."""
import os, json, math
D = os.environ["SCREEN_WORK"] + "/exp"
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "common.py")).read())
raw = json.load(open(f"{D}/raw-english.json"))
danger = {r["id"]: r["a"]["v"]["noul"] for r in raw["noul_danger|guardian"]}
a2 = json.load(open(f"{D}/align2-english.json"))
emp = a2["employee|guardian"]  # already 1 - p(allowed)
rank = lambda r: {i: k / len(r) for k, i in enumerate(sorted(r, key=r.get))}
rd, re_ = rank(danger), rank(emp)
for name, r in {
    "danger": danger, "employee": emp,
    "rank mean": {i: (rd[i] + re_[i]) / 2 for i in danger},
    "rank mean 2:1": {i: (2 * rd[i] + re_[i]) / 3 for i in danger},
    "rank mean 3:1": {i: (3 * rd[i] + re_[i]) / 4 for i in danger},
    "noisy-or": {i: 1 - (1 - danger[i]) * (1 - emp[i]) for i in danger},
    "max": {i: max(danger[i], emp[i]) for i in danger},
    "product": {i: danger[i] * emp[i] for i in danger},
}.items():
    print(f"{name:14}", fmt(metrics(r)))
