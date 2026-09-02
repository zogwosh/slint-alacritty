# Project Memory

## Terminal keyboard policy

- The application intentionally intercepts most `Ctrl`/`Alt` key combinations instead of forwarding them to the PTY.
- The current `Ctrl+C`, `Ctrl+A`, `Ctrl+V`, and `Alt+C` behavior is an explicit product requirement, not a bug.
- Do not change this shortcut policy or report it as a defect unless the user explicitly asks to revisit it.

## Module organization policy

- Do not introduce domain models or domain-driven layering when reorganizing this project.
- Split modules only by concrete implementation responsibility.

## UI controls policy

- Implement every user-visible control as a project-owned Slint component built from Slint primitives.
- Do not import controls or themes from `std-widgets.slint`, and do not use external or native widget toolkits.
- Source all GUI visual values from the project-owned `DesignTokens` global; do not introduce a parallel widget palette.
- Keep the terminal's default background, foreground, selection, and renderer clear colors synchronized with `DesignTokens`.

## Settings file policy

- Store user settings in a directly editable TOML file while retaining the GUI settings interface.
- GUI saves overwrite the complete TOML settings file directly; do not introduce a domain repository, database, transactional persistence abstraction, or format-preserving patch layer.
- On first launch, use the built-in defaults and generate the settings file.
- Watch the TOML settings file and hot-reload valid external changes.
- If the settings file contains syntax errors or invalid fields, report the problem, continue running with built-in defaults, and preserve the invalid file for the user to correct instead of silently replacing it.
