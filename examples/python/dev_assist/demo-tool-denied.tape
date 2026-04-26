Output assets/demo/dev-assist-tool-denied.gif

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

# Navigate to the dev_assist example
Type "cd examples/python/dev_assist"
Enter
Sleep 400ms

# Step 1 — Remove write_file from the tool allowlist.
# When apply_patch_node calls write_file, Nanny checks the allowlist,
# finds it missing, and fires ToolDenied before the file body runs.
Type "sed -i.bak '/\"write_file\"/d' nanny.toml"
Enter
Sleep 300ms

# Step 2 — Run the pipeline. ToolDenied fires at apply_patch_node.
# read_files_node deduplicates paths so max_calls is never exhausted first.
# Demonstrates: allowlist enforcement, ToolDenied stop reason, @nanny_tool interception.
Type "nanny run --limits=demo-tool-denied"
Enter

Sleep 25s

# Step 3 — Restore nanny.toml after recording.
Type "git checkout nanny.toml && rm -f nanny.toml.bak"
Enter
Sleep 500ms
