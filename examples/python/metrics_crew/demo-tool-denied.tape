Output assets/demo/metrics-crew-tool-denied.gif

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

# PRE-RECORDING: manually remove "detect_anomalies" from [tools] allowed in nanny.toml,
# then restore it after recording. ToolDenied fires when the analysis agent calls it
# after compute_stats. Demonstrates: allowlist enforcement, ToolDenied stop reason.
Type "nanny run"
Enter

Sleep 120s

