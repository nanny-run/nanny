Output assets/demo/metrics-crew-rule-denied.gif

Set Shell "zsh"
Set Width 1200
Set Height 650
Set FontSize 14
Set Theme "Dracula"
Set Framerate 24
Set PlaybackSpeed 1

# Clean prompt for recording
Type "export PS1='$(basename $PWD) ❯ '"
Enter
Sleep 300ms
Ctrl+L

# Navigate to the metrics_crew example
Type "cd examples/python/metrics_crew"
Enter
Sleep 400ms

# Step 1 — Cap compute_stats at 1 call.
# The pipeline calls compute_stats twice (cpu then error_rate).
# The second call fires RuleDenied: compute_stats.max_calls.
# max_calls = 20 is unique in nanny.toml — this sed is safe and cross-platform.
Type "sed -i.bak 's/max_calls     = 20/max_calls     = 1/' nanny.toml"
Enter
Sleep 300ms

# Step 2 — Run the pipeline. RuleDenied fires at the second compute_stats call.
# Demonstrates: per-tool call limit enforcement, RuleDenied stop reason.
Type "nanny run"
Enter

Sleep 90s

# Step 3 — Restore nanny.toml after recording.
Type "git checkout nanny.toml && rm -f nanny.toml.bak"
Enter
Sleep 500ms
