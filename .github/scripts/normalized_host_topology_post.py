from pathlib import Path
import re


def blocks(text: str, needle: str):
    pos = 0
    while True:
        start = text.find(needle, pos)
        if start < 0:
            return
        brace = text.find("{", start)
        depth = 0
        end = None
        for index in range(brace, len(text)):
            if text[index] == "{":
                depth += 1
            elif text[index] == "}":
                depth -= 1
                if depth == 0:
                    end = index
                    break
        if end is None:
            raise RuntimeError(f"unterminated {needle}")
        yield start, end
        pos = end + 1


# The topology schema adds DeviceSpec.host_parent. Existing literal fixtures
# that do not model host locality retain the legacy Machine parent explicitly.
for path in Path("crates").rglob("*.rs"):
    text = path.read_text()
    offset = 0
    changed = False
    for raw_start, raw_end in list(blocks(text, "DeviceSpec {")):
        start = raw_start + offset
        end = raw_end + offset
        block = text[start : end + 1]
        has_host_parent = re.search(r"(?m)^\s*(?:pub\s+)?host_parent\s*:", block)
        if not has_host_parent:
            line_start = text.rfind("\n", 0, end) + 1
            close_indent = text[line_start:end]
            field_indent = close_indent + "    "
            insertion = field_indent + "host_parent: None,\n"
            text = text[:line_start] + insertion + text[line_start:]
            offset += len(insertion)
            changed = True
    if changed:
        path.write_text(text)

# Synthetic Welcome fixtures predate the normalized host inventory. Their
# explicitly shaped CPU/memory summaries intentionally exercise legacy flat
# topology, so carry an empty host inventory rather than inventing locality.
for path in Path("crates").rglob("*.rs"):
    text = path.read_text()
    offset = 0
    changed = False
    for raw_start, raw_end in list(blocks(text, "AgentResponse::Welcome {")):
        start = raw_start + offset
        end = raw_end + offset
        block = text[start : end + 1]
        if re.search(r"(?m)^\s*host_nodes\s*:", block):
            continue
        devices = re.search(r"(?m)^(?P<indent>\s*)devices\s*:", block)
        if devices is None:
            continue
        insert_at = start + devices.start()
        indent = devices.group("indent")
        insertion = indent + "host_nodes: Vec::new(),\n"
        text = text[:insert_at] + insertion + text[insert_at:]
        offset += len(insertion)
        changed = True
    if changed:
        path.write_text(text)
