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

# RuleDenied — no_read_loop fires on the 5th total file_reader call.
# The complex trace references 5 unique source files. The agent reads them
# one by one; on the 5th call the client-side rule fires before the bridge
# is even contacted. The rule detects a stuck agent (too many reads = no progress).
# Demonstrates: @rule decorator, client-side enforcement, RuleDenied stop reason.
Type "nanny run --limits=demo-rule"
Enter

Sleep 90s
