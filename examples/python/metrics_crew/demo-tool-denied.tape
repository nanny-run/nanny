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

# Step 1 — Remove detect_anomalies from the tool allowlist.
# When the analysis agent calls detect_anomalies, Nanny checks the allowlist,
# finds it missing, and fires ToolDenied before the function body runs.
Type "sed -i.bak '/\"detect_anomalies\"/d' nanny.toml"
Enter
Sleep 300ms

# Step 2 — Run the pipeline. ToolDenied fires at the detect_anomalies_task.
# With single-tool tasks the LLM must call detect_anomalies — it cannot
# route around it. Enforcement is deterministic regardless of model.
Type "nanny run"
Enter

Sleep 120s

# Step 3 — Restore nanny.toml after recording.
Type "git checkout nanny.toml && rm -f nanny.toml.bak"
Enter
Sleep 500ms
