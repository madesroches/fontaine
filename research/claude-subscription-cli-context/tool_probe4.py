import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import probe
end = json.loads(sys.argv[1]); shape = sys.argv[2]
U1 = probe.user([{"type": "text", "text": "Read a.txt."}], False)
A1 = {"type": "assistant", "message": {"role": "assistant", "content": [
    {"type": "tool_use", "id": "toolu_01A", "name": "read", "input": {"path": "a.txt"}}]}, "parent_tool_use_id": None}
text = "alpha contents" + end
if shape == "array": content = [{"type": "text", "text": text}]
elif shape == "array2": content = [{"type": "text", "text": "head"}, {"type": "text", "text": text}]
else: content = text
r = probe.run([U1, A1, probe.user([{"type": "tool_result", "tool_use_id": "toolu_01A", "content": content}], True)])
print(shape, repr(end), r.get("subtype"))
