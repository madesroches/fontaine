#!/usr/bin/env python3
import json,sys
for l in open(sys.argv[1]):
    try: r=json.loads(l)
    except Exception: continue
    m=r.get('message',{}); cont=m.get('content') if isinstance(m,dict) else None
    for c in (cont if isinstance(cont,list) else []):
        if c.get('type')=='tool_use': print('CALL',json.dumps(c['input'])[:240])
        elif c.get('type')=='tool_result': print('RESULT',json.dumps(c.get('content'))[:360])
        elif c.get('type')=='text' and r.get('type')=='assistant': print('TEXT',c['text'][:300].replace('\n',' '))
    if r.get('type')=='user' and isinstance(cont,str): print('USERTXT',cont[:300])
    if r.get('type')=='system' and r.get('subtype') not in ('init',): print('SYSTEM',json.dumps(r)[:240])
