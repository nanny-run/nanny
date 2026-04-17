Output assets/demo/metrics-crew-max-steps.gif

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

# MaxStepsReached — cap at 4 steps, fires before analysis completes.
# Ingestion tools use 2-3 steps; the ceiling fires mid-pipeline.
# Demonstrates: step limit across a multi-agent pipeline, MaxStepsReached stop reason.
Type "nanny run --limits=demo-steps"
Enter

Sleep 90s
