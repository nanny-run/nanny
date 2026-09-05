<!--
Keep this tight. Reviewers read the summary and the diff, not prose. Delete
any section below that doesn't apply rather than leaving it unanswered.
-->

# Summary

<!-- What changed, in a sentence or two per distinct concern. Bullet it if there's more than one. -->

## Context

<!-- The problem this solves or the decision behind it. Skip if the summary already makes it obvious. -->

## Related

<!-- Link an issue, doc, or prior PR this builds on. Delete if none. -->

## Test plan

<!-- Exact commands you ran and their result. "It works" is not a test plan. Then check
whatever below actually applies to this diff; delete any line that doesn't. -->

- [ ] `cargo test --workspace`:
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`:
- [ ] `uv run pytest -q` (SDK changed):
- [ ] `uv run ruff check .` and `uv run mypy nanny_sdk` (SDK changed):
- [ ] **A CLI flag, env var, or default changed**, `--help` re-read from the built binary, and the docs page for it updated in the same pass
- [ ] **Enforcement path touched** (allowlist, rules, the bridge), a denial is covered by a test, not only the allowed case
- [ ] **The runtime fails closed where it used to fail open**, or the reverse, called out explicitly in the summary
- [ ] **Anything a governor prints changed**, no credential in the output, and the assertions that pin those lines updated
- [ ] **Breaking**, called out with what an existing install has to do, and the version bump it implies
- [ ] **New dependency added**, checked nothing already vendored does the same job
