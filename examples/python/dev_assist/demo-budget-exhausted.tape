Output assets/demo/dev-assist-budget-exhausted.gif

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

# BudgetExhausted — budget=12, file_reader costs 5 each, ripgrep costs 8 each.
# First file_reader spends 5 (total 5), second spends 5 (total 10).
# Third call of any kind (cost >= 2) fires BudgetExhausted before it runs.
# Demonstrates: @tool(cost=N) per-call metering, BudgetExhausted stop reason.
Type "nanny run --limits=demo-budget"
Enter

Sleep 12s
