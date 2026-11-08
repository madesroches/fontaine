import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import probe
U1 = probe.user([{"type": "text", "text": "Read the file a.txt with the read tool."}], False)
A1 = {"type": "assistant", "message": {"role": "assistant", "content": [
    {"type": "tool_use", "id": "toolu_01ABC", "name": "read", "input": {"path": "a.txt"}}]}, "parent_tool_use_id": None}
U2 = probe.user([{"type": "tool_result", "tool_use_id": "toolu_01ABC", "content": [{"type": "text", "text": probe.doc(1)}]}], True)
print(json.dumps(probe.run([U1, A1, U2]))[:600])
