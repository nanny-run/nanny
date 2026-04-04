Output assets/demo/webdingo-agent-scopes.gif

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

# Navigate to the webdingo example
Type "cd examples/rust/webdingo"
Enter
Sleep 400ms

# Agent scopes — shows AgentScopeEntered/AgentScopeExited for all 3 pipeline stages
# Demonstrates: #[nanny::agent], named limit sets, scope enter/exit events in NDJSON log
Type '/Users/evans/Documents/codes/a-nanny/nanny/target/release/nanny run --limits=agent-scopes-demo -- "best Rust HTTP clients"'
Enter

Sleep 95s
