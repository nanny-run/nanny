Output assets/demo/qabud-budget-exhausted.gif

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

# Navigate to the qabud example
Type "cd examples/rust/qabud"
Enter
Sleep 400ms

# BudgetExhausted — fires on 3rd read_file attempt (cost=20, cost_per_call=10:
# read1→10, read2→0, read3 attempt: 0 < 10 → BudgetExhausted)
# Demonstrates: #[nanny::tool(cost=10)] per-call cost metering, BudgetExhausted stop reason
# ./src contains only .rs files — no sensitive patterns to trigger no_sensitive_files first
Type "/Users/evans/Documents/codes/a-nanny/nanny/target/release/nanny run --limits=budget-demo -- ./src"
Enter

Sleep 25s
