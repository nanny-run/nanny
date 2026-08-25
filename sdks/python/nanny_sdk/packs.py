"""Load installed rule packs from ``.nanny/rules/``.

Packs are vendored into the project and committed. Loading walks what is on
disk, imports each pack's Python module, and registers its rules alongside any
the developer wrote themselves. Nothing here fetches, resolves a version, or
reaches the network: the engine reads files, and everything else is
``nanny rules add`` at a terminal.

**Version and pack id come from the manifest, never from the decorator.** A rule
author writes ``@rule("no_send_after_read")`` and nothing else. Provenance is a
property of where the rule came from, not something a person should have to type
correctly, and asking them to would guarantee it eventually disagrees with
reality.
"""

from __future__ import annotations

import importlib.util
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

PACK_DIR = Path(".nanny") / "rules"


@dataclass(frozen=True)
class LoadedRule:
    """One rule and where it came from."""

    name: str
    version: str | None = None
    pack: str | None = None

    def to_declaration(self) -> dict[str, str]:
        """The shape ``POST /rules`` expects."""
        out = {"name": self.name}
        if self.version:
            out["version"] = self.version
        if self.pack:
            out["pack"] = self.pack
        return out


def _import_module_from(path: Path, module_name: str) -> None:
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:  # pragma: no cover - unreachable for real files
        return
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)


def load_installed_packs(project_root: Path | str = ".") -> list[LoadedRule]:
    """Import every installed pack and return what it registered.

    Returns the rules with their provenance attached, ready to declare. Rules
    register through the ordinary ``@rule`` decorator as each module imports, so
    a pack rule and a hand-written rule are the same kind of thing from the
    moment they are loaded, which is what keeps evaluation order and denial
    semantics identical for both.
    """
    root = Path(project_root)
    packs_dir = root / PACK_DIR
    if not packs_dir.is_dir():
        return []

    from nanny_sdk._decorators import _RULES

    loaded: list[LoadedRule] = []
    for pack_path in sorted(p for p in packs_dir.iterdir() if p.is_dir()):
        manifest_path = pack_path / "pack.toml"
        if not manifest_path.is_file():
            continue
        manifest = tomllib.loads(manifest_path.read_text())
        name = manifest.get("name", pack_path.name)
        version = manifest.get("version")
        python_dir = manifest.get("python")
        if not python_dir:
            continue

        before = set(_RULES)
        target = pack_path / python_dir
        if target.is_dir():
            for module_file in sorted(target.glob("*.py")):
                _import_module_from(module_file, f"_nanny_pack_{pack_path.name}_{module_file.stem}")
        elif target.is_file():
            _import_module_from(target, f"_nanny_pack_{pack_path.name}")

        # Provenance comes from the manifest, not from what this call happened to
        # register. Diffing the registry is wrong on the second call: importing
        # again registers nothing new, so every pack rule would silently lose its
        # version and pack, which is exactly the field an auditor reads.
        declared = manifest.get("rules")
        rule_names = declared if declared else sorted(set(_RULES) - before)
        for rule_name in rule_names:
            if rule_name in _RULES:
                loaded.append(LoadedRule(name=rule_name, version=version, pack=name))

    return loaded


def declare_all(project_root: Path | str = ".") -> list[dict[str, str]]:
    """Load packs and return every registered rule as a declaration.

    Hand-written rules are included with no version and no pack, which is the
    honest answer rather than a fabricated provenance.
    """
    from nanny_sdk._decorators import _RULES

    pack_rules = {r.name: r for r in load_installed_packs(project_root)}
    return [
        pack_rules.get(name, LoadedRule(name=name)).to_declaration()
        for name in _RULES
    ]
