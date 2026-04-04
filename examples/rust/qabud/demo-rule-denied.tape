Output assets/demo/qabud-rule-denied.gif

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

# Navigate to the qabud example
Type "cd examples/rust/qabud"
Enter
Sleep 400ms

# RuleDenied — no_sensitive_files rule blocks .env before it is read
# Demonstrates: #[nanny::rule] with last_tool_args, content-based enforcement
# src/*.rs files read first (collector.rs, main.rs, rules.rs), then .env at root is blocked
# Shows successful reads before the rule fires, .env is never opened
Type "nanny run --limits=rule-demo"
Enter

Sleep 60s
