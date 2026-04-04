Output assets/demo/webdingo-budget-exhausted.gif

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

# Navigate to the webdingo example
Type "cd examples/rust/webdingo"
Enter
Sleep 400ms

# BudgetExhausted — fires before 3rd http_get executes (cost=25, cost_per_call=10:
# call1→spent=10, call2→spent=20, call3 pre-check: 20+10>25 → BudgetExhausted)
# Demonstrates: nanny::http_get cost metering, tool allowlist enforcement
Type 'nanny run --limits=budget-demo -- "best Rust HTTP clients"'
Enter

Sleep 80s
