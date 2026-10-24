import os, json, sys
D = os.environ["SCREEN_WORK"] + "/exp"
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "common.py")).read())
rows = []
for ck in ("english", "typed-decisions", "2-english", "2-typed-decisions", "3-english", "3-typed-decisions", "4-english", "4-typed-decisions", "5-english", "5-typed-decisions", "6-english", "6-typed-decisions"):
    p = f"{D}/align{ck}.json" if ck[0] in "23456" else f"{D}/align-{ck}.json"
    if not os.path.exists(p): continue
    for key, r in json.load(open(p)).items():
        rows.append((metrics(r), ck, key))
rows.sort(key=lambda x: -x[0][0])
for m, ck, key in rows: print(f"{ck:16}{key:24}", fmt(m))
