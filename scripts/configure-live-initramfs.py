#!/usr/bin/env python3
"""Add the live boot splash to ArchISO's single- or multiline hook array."""

import pathlib
import re
import shlex
import sys


def configure(text):
    pattern = re.compile(r"(?m)^[ \t]*HOOKS\s*=\s*\(([^)]*)\)")
    entries = list(pattern.finditer(text))
    if len(entries) != 1:
        raise ValueError("expected one HOOKS array in the live initramfs configuration")
    entry = entries[0]
    hooks = shlex.split(entry.group(1), comments=True)
    if not hooks or any(not re.fullmatch(r"[A-Za-z0-9_-]+", hook) for hook in hooks):
        raise ValueError("live initramfs HOOKS must contain literal hook names")
    hooks = [hook for hook in hooks if hook != "plymouth"]
    # New releng profiles can omit kms; keep the splash before archiso
    # mounts the live image, after device discovery.
    after = next((hook for hook in ("kms", "udev", "systemd", "base") if hook in hooks), None)
    if after is None:
        raise ValueError("no device-discovery hook in the live initramfs configuration")
    hooks.insert(hooks.index(after) + 1, "plymouth")
    return text[:entry.start()] + "HOOKS=(" + " ".join(hooks) + ")" + text[entry.end():]


if __name__ == "__main__":
    path = pathlib.Path(sys.argv[1])
    try:
        updated = configure(path.read_text())
    except (OSError, ValueError) as error:
        sys.exit(f"could not configure the live initramfs: {error}")
    path.write_text(updated)
