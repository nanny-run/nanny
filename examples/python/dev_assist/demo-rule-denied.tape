Output assets/demo/dev-assist-rule-denied.gif

Set Shell "zsh"
Set Width 1200
Set Height 650
Set FontSize 14
Set Theme "Dracula"
Set Framerate 24
Set PlaybackSpeed 1.5

# Clean prompt for recording
Type "export PS1='$(basename $PWD) ❯ '"
Enter
Sleep 300ms
Ctrl+L

# Navigate to the dev_assist example
Type "cd examples/python/dev_assist"
Enter
Sleep 400ms

# Step 1 — Lower file_reader max_calls to 3 so RuleDenied fires on the 4th read.
# The trace has 5 unique files; the 4th read is denied before the agent finishes.
# Demonstrates: max_calls enforcement, RuleDenied stop reason, @tool interception.
Type "sed -i.bak 's/max_calls.*=.*15/max_calls     = 3/' nanny.toml"
Enter
Sleep 300ms

# Step 2 — Run the pipeline. RuleDenied fires at the 4th file_reader call.
Type "nanny run --limits=demo-rule"
Enter

Sleep 15s

# Step 3 — Restore nanny.toml after recording.
Type "git checkout nanny.toml && rm -f nanny.toml.bak"
Enter
Sleep 500ms
