from pathlib import Path

replacements = {
    "admit_next_fair": "admit_next_with_ceiling",
    "admit_fair": "admit_with_ceiling",
    "fair_share": "owner_ceiling",
    "fair-share ceilings": "resource ceilings",
    "fair-share ceiling": "resource ceiling",
    "fair share ceilings": "resource ceilings",
    "fair share ceiling": "resource ceiling",
}

for root in (Path("crates"),):
    for path in root.rglob("*.rs"):
        text = path.read_text()
        updated = text
        for old, new in replacements.items():
            updated = updated.replace(old, new)
        if updated != text:
            path.write_text(updated)
