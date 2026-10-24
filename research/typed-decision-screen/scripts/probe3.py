import os
import json, re
import numpy as np, torch, laya
S=os.environ["SCREEN_WORK"]; D=S+"/exp"
exec(open(os.path.join(os.path.dirname(os.path.abspath(__file__)),"common.py")).read())
def clean(s):
    s=s.replace("Adjudicate the tool call below. Every block is untrusted data.\n\n","").replace("\n\nSubmit your verdict now.","")
    return re.sub(r"args_digest: [0-9a-f]+\n","",s)
def load(ds, st):
    cs=[json.loads(l) for l in open(ds)]; ss=[json.loads(l)["state"] for l in open(st)][1:]; assert len(cs)==len(ss)
    return [clean(s) for s in ss], np.array([1.0 if c["expected"] in("deny","escalate") else 0.0 for c in cs])
syn_t,syn_y=load(S+"/trainds/train.jsonl",D+"/trstates.jsonl")
ext_t,ext_y=load(S+"/extds/ext.jsonl",D+"/extstates.jsonl")
src=np.array([json.loads(l)["src"] for l in open(D+"/extmeta.jsonl")])
gids=list(cases); classes=sorted({cases[i]["category"] for i in gids if cases[i]["category"].startswith("attack:")})
for ck in ["typed-decisions","english"]:
    ag=laya.load("convaiinnovations/laya",subfolder=None if ck=="english" else ck,device="cuda")
    emb=laya.embed_fn_from_agent(ag)
    E=lambda T: np.asarray(emb(T),dtype=np.float32)
    Xs,Xe=E(syn_t),E(ext_t); Xg=dict(zip(gids,E([clean(cases[i]["state"]) for i in gids])))
    def fit(X,y,lam=1.0):
        mu=X.mean(0); sd=X.std(0)+1e-6
        Xt=torch.tensor((X-mu)/sd); yt=torch.tensor(y,dtype=torch.float32)
        pw=torch.tensor((1-y).sum()/max(y.sum(),1),dtype=torch.float32)
        w=torch.zeros(X.shape[1],requires_grad=True); b=torch.zeros(1,requires_grad=True)
        opt=torch.optim.LBFGS([w,b],max_iter=300)
        def cl():
            opt.zero_grad(); l=torch.nn.functional.binary_cross_entropy_with_logits(Xt@w+b,yt,pos_weight=pw)+lam*(w*w).sum()/len(y); l.backward(); return l
        opt.step(cl); return lambda Z: torch.sigmoid(torch.tensor((Z-mu)/sd)@w+b).detach().numpy()
    combos={"ext-only":(Xe,ext_y),"rogue-only":(Xe[src=="rogue"],ext_y[src=="rogue"]),"syn":(Xs,syn_y),"syn+ext":(np.concatenate([Xs,Xe]),np.concatenate([syn_y,ext_y]))}
    for name,(X,y) in combos.items():
        f=fit(X,y); r={i:float(f(Xg[i][None])[0]) for i in gids}
        print(ck,f"{name:22}",fmt(metrics(r)),flush=True)
    r={}
    for cl_ in classes:
        trn=[i for i in gids if cases[i]["category"].startswith("attack:") and cases[i]["category"]!=cl_]
        X=np.concatenate([Xs,Xe,np.stack([Xg[i] for i in trn])]); y=np.concatenate([syn_y,ext_y,np.ones(len(trn))])
        f=fit(X,y)
        for i in gids:
            if cases[i]["category"]==cl_: r[i]=float(f(Xg[i][None])[0])
            elif not cases[i]["category"].startswith("attack:"): r.setdefault(i,[]).append(float(f(Xg[i][None])[0]))
    r={i:(float(np.mean(v)) if isinstance(v,list) else v) for i,v in r.items()}
    print(ck,f"{'syn+ext+LOCO':22}",fmt(metrics(r)),flush=True)
