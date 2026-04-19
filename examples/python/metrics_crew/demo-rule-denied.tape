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

# RuleDenied — no_analysis_loop rule fires after 5 consecutive compute_stats calls.
# Generous budget (cost=1000, steps=100) ensures only the rule fires, not a limit.
# Demonstrates: client-side rule evaluation, RuleDenied stop reason, loop detection.
Type "nanny run --limits=demo-rule"
Enter

Sleep 120s

