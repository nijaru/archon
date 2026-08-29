from pathlib import Path
import re

# The topology schema adds DeviceSpec.host_parent. Existing literal fixtures
# that do not model host locality retain the legacy Machine parent explicitly.
needle = "DeviceSpec {"
for path in Path("crates").rglob("*.rs"):
    text = path.read_text()
    pos = 0
    changed = False
    while True:
        start = text.find(needle, pos)
        if start < 0:
            break
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
            raise RuntimeError(f"unterminated DeviceSpec in {path}")
        block = text[start : end + 1]
        if not re.search(r"(?m)^\s*host_parent\s*:", block):
            line_start = text.rfind("\n", 0, end) + 1
            close_indent = text[line_start:end]
            field_indent = close_indent + "    "
            insertion = field_indent + "host_parent: None,\n"
            text = text[:line_start] + insertion + text[line_start:]
            end += len(insertion)
            changed = True
        pos = end + 1
    if changed:
        path.write_text(text)
