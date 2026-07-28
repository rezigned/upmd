Output .github/pages/demos/workflow.gif

Source ./.github/vhs/config.tape

# Launch upmd with verify-all pre-selected
Type "upmd -b verify-all"
Enter
Sleep 1s

# Run verify-all (depends on build → clippy | fmt-check → test)
# Workflow graph appears automatically
Enter
Sleep 5s
