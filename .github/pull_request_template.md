<!--
The convention below is what every PR in this repo already does. It is written
down so it stops depending on whoever opened the last one.

Delete this comment and the guidance under each heading as you fill it in.
-->

<!--
Open with how this was found, in a sentence or two. A staging log, a failing
deploy, a review. Not a summary of the diff: the diff is on the Files tab.
-->

## <name the first problem>

<!--
One `##` per distinct defect, named after the problem rather than the change.
"The token was printed in full", not "Changes to logging". Number them
(`## 1. ...`) when there are several and the order matters.

Say what was wrong, what it cost, and why the fix is the one chosen. Where a
decision could reasonably have gone the other way, say why it did not.
-->

## Verification

<!--
Always last, always present. What you ran, and what you added.

- `cargo test --workspace` and `cargo clippy --workspace --all-targets`
- new tests, and which of the problems above each one pins
- anything observed and deliberately not fixed
-->
