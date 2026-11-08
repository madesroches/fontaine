import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import probe
mode = sys.argv[1]
U1 = probe.user([{"type": "text", "text": "Read a.txt and b.txt."}], False)
A1 = {"type": "assistant", "message": {"role": "assistant", "content": [
    {"type": "tool_use", "id": "toolu_01A", "name": "read", "input": {"path": "a.txt"}},
    {"type": "tool_use", "id": "toolu_01B", "name": "read", "input": {"path": "b.txt"}}]}, "parent_tool_use_id": None}
blocks = [{"type": "tool_result", "tool_use_id": "toolu_01A", "content": "alpha contents"},
          {"type": "tool_result", "tool_use_id": "toolu_01B", "content": "beta contents"}]
if mode == "mixed":
    blocks.append({"type": "text", "text": "Also, a note from the user."})
U2 = probe.user(blocks, True)
print(json.dumps(probe.run([U1, A1, U2]))[:300])
