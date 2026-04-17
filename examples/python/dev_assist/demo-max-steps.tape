Output assets/demo/dev-assist-max-steps.gif

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

# MaxStepsReached — cap at 3 steps so the ceiling fires quickly.
# The agent makes 3 tool calls then the 4th attempt hits MaxStepsReached.
# Demonstrates: step limit, MaxStepsReached stop reason.
Type "nanny run --limits=demo-steps"
Enter

Sleep 110s
