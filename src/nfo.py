import re


NFO_RE = re.compile(r"\.(?:nfo|txt)(?:\"|\s|$)", re.IGNORECASE)
TAG_RE = re.compile(r"<(?:title|name|release(?:name)?)>\s*(.*?)\s*</", re.IGNORECASE | re.DOTALL)


def is_nfo(subject):
    return bool(NFO_RE.search(subject or ""))

def display_name(data):
    if not data:
        return None

    if isinstance(data, bytes):
        text = data.decode("utf-8", "replace")
    else:
        text = str(data)

    candidates = TAG_RE.findall(text)

    for line in text.splitlines():
        match = re.match(r"\s*(?:release\s*name|release|title|movie|series)\s*:\s*(.+?)\s*$", line, re.IGNORECASE)

        if match:
            candidates.append(match.group(1))
        
    for candidate in candidates:
        name = re.sub(r"\s+", " ", candidate).strip(" .\t\r\n")

        if valid_name(name):
            return name

    return None

def valid_name(name):
    if not name or len(name) < 3 or len(name) > 240:
        return False
    
    if re.fullmatch(r"[-=_*# .]+", name):
        return False
    
    if name.lower() in {"nfo", "release", "title", "unknown", "none"}:
        return False
    
    return True
