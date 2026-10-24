import os
import json, sys, itertools
sys.argv=[sys.argv[0]]+sys.argv[1:]
D=os.environ["SCREEN_WORK"]+"/exp"
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)),"common.py")).read())
sig={}
for ck in ["typed-decisions","english"]:
    raw=json.load(open(f"{D}/raw-{ck}.json"))
    for key,rows in raw.items():
        q,s=key.split("|")
        if q=="noul_ok": continue
        r={row["id"]:risk(q,row["a"]) for row in rows}
        # rank-normalize
        ids=sorted(r,key=r.get); sig[f"{ck}:{key}"]={i:k/len(ids) for k,i in enumerate(ids)}
names=list(sig)
best=[]
for k in (2,3,4):
    for combo in itertools.combinations(names,k):
        r={i:sum(sig[n][i] for n in combo)/k for i in cases}
        best.append((metrics(r),combo))
best.sort(key=lambda x:-x[0][0])
for m,c in best[:8]: print(fmt(m),c)
