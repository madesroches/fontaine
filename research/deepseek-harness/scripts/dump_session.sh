#!/usr/bin/env bash
# Decompress a run's dsh session log (zstd) to runs/<run>/session.jsonl using node's zlib inside the image.
set -euo pipefail
R=$(cd "$(dirname "$0")/../runs/$1" && pwd)
docker run --rm -v "$R:/x" --entrypoint node dsh-study:22ddee37 -e '
const z=require("zlib"),fs=require("fs"),path=require("path");
const find=d=>fs.readdirSync(d,{withFileTypes:true}).flatMap(e=>e.isDirectory()?find(path.join(d,e.name)):[path.join(d,e.name)]);
const out=[];for(const f of find("/x/dsh-home/sessions").filter(f=>f.endsWith(".zstd")||f.endsWith(".jsonl")))
 out.push(f.endsWith(".zstd")?z.zstdDecompressSync(fs.readFileSync(f)).toString():fs.readFileSync(f,"utf8"));
fs.writeFileSync("/x/session.jsonl",out.join(""))'
