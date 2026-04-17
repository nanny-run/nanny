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

# ToolDenied — write_file is absent from [tools] allowed.
# Plan mode reads the trace files, diagnoses the bug, then deterministically
# calls write_file to apply the fix. ToolDenied fires before any file is written.
# Demonstrates: allowlist enforcement, ToolDenied stop reason, @nanny_tool interception.
Type "nanny run --limits=demo-tool-denied -- --mode plan"
Enter

Sleep 70s
