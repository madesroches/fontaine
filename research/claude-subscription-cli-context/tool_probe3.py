import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import probe
mode = sys.argv[1]
U1 = probe.user([{"type": "text", "text": "Read a.txt."}], False)
A1 = {"type": "assistant", "message": {"role": "assistant", "content": [
    {"type": "tool_use", "id": "toolu_01A", "name": "read", "input": {"path": "a.txt"}}]}, "parent_tool_use_id": None}
if mode == "empty_text_after":
    blocks = [{"type": "tool_result", "tool_use_id": "toolu_01A", "content": [{"type": "text", "text": "alpha"}]}, {"type": "text", "text": ""}]
elif mode == "empty_string":
    blocks = [{"type": "tool_result", "tool_use_id": "toolu_01A", "content": ""}]
elif mode == "absent":
    blocks = [{"type": "tool_result", "tool_use_id": "toolu_01A"}]
elif mode == "empty_array":
    blocks = [{"type": "tool_result", "tool_use_id": "toolu_01A", "content": []}]
print(mode, json.dumps(probe.run([U1, A1, probe.user(blocks, True)]))[:200])
