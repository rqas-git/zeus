# Keyboard Shortcuts

| Shortcut | Action |
| --- | --- |
| `Cmd+N` | Open a new Zeus window with a fresh rust-agent session. |
| `Cmd+B` | Open the branch dropdown menu. Branch switches run through rust-agent. |
| `Cmd+M` | Open the model dropdown menu. |
| `Cmd+E` | Open the reasoning effort dropdown menu. |
| `Cmd+P` | Open the permissions dropdown menu. |
| `Cmd+F` | Open transcript search. |
| `Cmd+G` | Move to the next transcript search match. |
| `Cmd+Shift+G` | Move to the previous transcript search match. |
| `Cmd+T` | Toggle terminal passthrough for the input field. Commands run through rust-agent, are recorded in the active session context, and do not change the selected model permissions. |
| `Ctrl+C` | Clear the input field. |
| `Ctrl+Enter` | Insert a newline in the input field. |
| `Up Arrow` | Recall the previous submitted message while the input field is focused, open the focused footer menu, or move the highlighted option up while a footer dropdown is open. |
| `Down Arrow` | Recall the next submitted message while the input field is focused, move to the footer controls when already at the current draft, exit footer controls, or close the open footer menu without selecting. |
| `Left Arrow` / `Right Arrow` | Move between footer controls while footer navigation is active. |
| `Return` / `Enter` | Activate the focused footer control, or select the highlighted option while a footer dropdown is open. |
| `Esc` | Cancel the current response, close transcript search, footer navigation, or the open footer dropdown menu. |

## Commands

| Command | Action |
| --- | --- |
| `/restore <session id>` | Restore a local rust-agent transcript session by id. |
