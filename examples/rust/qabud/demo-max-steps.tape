Output assets/demo/qabud-max-steps.gif

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

# MaxStepsReached — fires on 3rd read_file attempt (steps=2)
# Demonstrates: step ceiling, MaxStepsReached stop reason, structured event log
# ./src contains only .rs files — no sensitive patterns to trigger no_sensitive_files first
Type "/Users/evans/Documents/codes/a-nanny/nanny/target/release/nanny run --limits=max-steps-demo -- ./src"
Enter

Sleep 25s
