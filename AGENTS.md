# Project Memory

## Terminal keyboard policy

- The application intentionally intercepts most `Ctrl`/`Alt` key combinations instead of forwarding them to the PTY.
- The current `Ctrl+C`, `Ctrl+A`, `Ctrl+V`, and `Alt+C` behavior is an explicit product requirement, not a bug.
- Do not change this shortcut policy or report it as a defect unless the user explicitly asks to revisit it.
