Output assets/demo/webdingo-timeout.gif

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

# TimeoutExpired — fires after 6s wall-clock, no SDK instrumentation needed
# Demonstrates: process-level timeout works for any language, zero code changes
Type 'nanny run --limits=timeout-demo -- "best Rust HTTP clients"'
Enter

Sleep 12s
