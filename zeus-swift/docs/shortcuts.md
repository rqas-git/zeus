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
| `Tab` | Open path completion or accept the highlighted path suggestion. In chat mode, `@` at a token boundary opens file-reference completion automatically. |
| `Ctrl+Enter` | Insert a newline in the input field. |
| `Up Arrow` | Move the highlighted path suggestion up, recall the previous submitted message while the input field is focused, open the focused footer menu, or move the highlighted option up while a footer dropdown is open. |
| `Down Arrow` | Move the highlighted path suggestion down, recall the next submitted message while the input field is focused, move to the footer controls when already at the current draft, exit footer controls, move the highlighted option down while a footer dropdown is open, or close that dropdown when already on its last option. |
| `Left Arrow` / `Right Arrow` | Move between footer controls while footer navigation is active. |
| `Return` / `Enter` | Accept the highlighted path suggestion, activate the focused footer control, or select the highlighted option while a footer dropdown is open. |
| `Esc` | Close path completion, cancel the current response, close transcript search, footer navigation, or the open footer dropdown menu. |

## Commands

| Command | Action |
| --- | --- |
| `/login` | Start rust-agent authorization. |
| `/restore <session id>` | Restore a local rust-agent transcript session by numeric id. |
| `/show-cache` | Toggle per-response token and prompt-cache stats below assistant messages. |

Implementation details live in `docs/architecture/TERMINAL-UI.md` and
`docs/architecture/CHAT-STATE.md`.
