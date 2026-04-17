Output assets/demo/metrics-crew-budget-exhausted.gif

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

# BudgetExhausted — budget=5, validate_schema costs 1, load_metrics costs 3.
# validate_schema(1) + load_metrics(3) = 4 spent; next call (cost >= 2) fires BudgetExhausted.
# Demonstrates: per-call cost metering across multiple tool types, BudgetExhausted stop reason.
Type "nanny run --limits=demo-budget"
Enter

Sleep 60s
