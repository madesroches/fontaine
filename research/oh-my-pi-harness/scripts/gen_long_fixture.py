#!/usr/bin/env python3
"""Regenerate fixtures/long/events.log (1200 lines, ~112 KB), the large input for E6. Deterministic. usage: gen_long_fixture.py <fixtures/long dir>"""
import random, sys
random.seed(1)
with open(sys.argv[1].rstrip("/") + "/events.log", "w") as f:
    for i in range(1, 1201):
        f.write(f"2026-10-26T10:{i//60%60:02d}:{i%60:02d}Z svc-{i%7} level={'ERROR' if i%97==0 else 'INFO'} request_id=r{i:05d} latency_ms={random.randint(5,900)} path=/api/v{i%3}/items/{i}\n")
