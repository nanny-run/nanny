"""Rule packs load from disk and carry their provenance."""

from __future__ import annotations

from nanny_sdk import _decorators
from nanny_sdk.packs import LoadedRule, declare_all, load_installed_packs


def install(root, name: str, version: str, body: str, dirname: str | None = None) -> None:
    d = root / ".nanny" / "rules" / (dirname or f"{name.replace(':', '-')}@{version}")
    (d / "python").mkdir(parents=True)
    (d / "pack.toml").write_text(
        f'name = "{name}"\nversion = "{version}"\npython = "python"\n'
        'rules = ["no_send_after_read"]\n'
    )
    (d / "python" / "rules.py").write_text(body)


RULE_BODY = """
from nanny_sdk import rule


@rule("no_send_after_read")
def no_send_after_read(ctx):
    return True
"""


def test_an_installed_pack_registers_its_rules(tmp_path):
    install(tmp_path, "nanny:owasp", "2.1.0", RULE_BODY)
    loaded = load_installed_packs(tmp_path)

    assert loaded == [LoadedRule(name="no_send_after_read", version="2.1.0", pack="nanny:owasp")]
    assert "no_send_after_read" in _decorators._RULES


def test_provenance_comes_from_the_manifest_not_the_decorator(tmp_path):
    """The rule author writes ``@rule("name")`` and nothing else.

    Asking a person to type a version correctly guarantees it eventually
    disagrees with what actually shipped.
    """
    install(tmp_path, "nanny:owasp", "2.1.0", RULE_BODY)
    (loaded,) = load_installed_packs(tmp_path)

    assert "version" not in RULE_BODY
    assert loaded.version == "2.1.0"
    assert loaded.pack == "nanny:owasp"


def test_a_hand_written_rule_has_no_pack_and_no_version(tmp_path):
    @_decorators.rule("my_own_rule")
    def mine(ctx):
        return True

    declarations = declare_all(tmp_path)
    mine_decl = next(d for d in declarations if d["name"] == "my_own_rule")

    assert mine_decl == {"name": "my_own_rule"}, (
        "inventing a version would imply provenance a hand-written rule does not have"
    )


def test_pack_and_local_rules_declare_together(tmp_path):
    install(tmp_path, "nanny:owasp", "2.1.0", RULE_BODY)

    @_decorators.rule("my_own_rule")
    def mine(ctx):
        return True

    load_installed_packs(tmp_path)
    by_name = {d["name"]: d for d in declare_all(tmp_path)}

    assert by_name["no_send_after_read"]["pack"] == "nanny:owasp"
    assert "pack" not in by_name["my_own_rule"]


def test_no_packs_directory_is_not_an_error(tmp_path):
    assert load_installed_packs(tmp_path) == []


def test_a_pack_without_a_python_implementation_is_skipped(tmp_path):
    d = tmp_path / ".nanny" / "rules" / "rust-only@1.0.0"
    d.mkdir(parents=True)
    (d / "pack.toml").write_text('name = "rust:only"\nversion = "1.0.0"\n')
    assert load_installed_packs(tmp_path) == []


def test_loading_reads_only_the_project_directory(tmp_path, monkeypatch):
    """Loading must never reach the network.

    A pack that could be fetched during a run would break the offline
    guarantee, the ban on remote dependencies, and determinism at once.
    """
    import socket

    def explode(*a, **k):  # pragma: no cover - only runs on regression
        raise AssertionError("pack loading opened a socket")

    monkeypatch.setattr(socket, "socket", explode)
    monkeypatch.setattr(socket, "create_connection", explode)

    install(tmp_path, "nanny:owasp", "2.1.0", RULE_BODY)
    assert len(load_installed_packs(tmp_path)) == 1


def test_the_first_governed_call_declares_the_rules(monkeypatch, mock_bridge):
    """Without this the rules half of declared authority never reaches the log.

    The governor cannot see rule bodies for itself, so a run would record what
    was refused and never what could have refused.
    """
    from nanny_sdk import _client, _decorators

    declared: list = []
    monkeypatch.setattr(_client, "declare_rules", declared.append)
    monkeypatch.setattr(_client, "call_tool", lambda *a, **k: None)

    @_decorators.rule("my_own_rule")
    def mine(ctx):
        return True

    @_decorators.tool()
    def do_thing() -> str:
        return "ok"

    do_thing()
    do_thing()

    assert len(declared) == 1, "declared once, not on every call"
    assert declared[0] == [{"name": "my_own_rule"}]
