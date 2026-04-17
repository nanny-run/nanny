Output assets/demo/metrics-crew-agent-scopes.gif

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

# Navigate to the metrics_crew example
Type "cd examples/python/metrics_crew"
Enter
Sleep 400ms

# Agent scopes — all four agents run under their own named limit sets.
# AgentScopeEntered/AgentScopeExited events show ingestion → analysis →
# visualization → reporter, each governed under [limits.<role>].
# Demonstrates: @agent decorator, per-role limits, scope enter/exit events.
Type "nanny run"
Enter

Sleep 130s
