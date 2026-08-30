# Rule packs

First-party rule packs, published by us only.

A pack is rule code plus a manifest. `nanny rules add name@version` vendors it
into a project's `.nanny/rules/` and writes a pinned entry into `[rules]
extends`. It never edits your source: `@rule` stays the decorator you use for
your own private rules.

## Why packs are code, and published by us

A rule is a security control running inside the agent's process, receiving real
tool arguments. A compromised one fails **silent**, returning allow forever while
every dashboard stays green, which is strictly worse than a library failing loud.
Open publishing would make this an attack path into the exact thing Nanny sells,
so phase one is first-party only and phase two is community submission by pull
request. Review, never upload.

## Why they carry both languages

A pack must exist in Python and Rust. Two separately published packages are two
implementations of the same control that can drift, and a rule meaning one thing
in Python and another in Rust is worse than no shared rule at all. One bundle,
both implementations, one version.

## Why versions are pinned and never auto-updated

An auto-updating control changes without anyone deciding to change it. That
destroys determinism, and it makes compliance evidence change meaning after the
fact: a report saying "governed by nanny:owasp" is worthless if the pack behind
it moved. Cloud may notice a newer version exists and show you the command to
run. Upgrading is always a human act, reviewed in your own repository.

## Rules reference labels, never tool names

`send_outreach`, `post_message` and `charge_card` are different names for the
same hazards. A rule naming any of them governs one application. A rule reading
`external_effect` governs every application whose operator labelled their tools,
which is what makes a shared corpus possible at all. The rule holds the logic;
the config holds the facts about this app.
