Zeus
---
Synchronous personal coding agent.\
MacOS-native SwiftUI frontend with custom Rust agent harness as the backend.

Features
---

**Complete Keyboard Navigability**
* The application can entirely be navigated without a mouse.
* Intuitive keyboard shortcuts for ease-of-use.
* Use ```↑```, ```↓```, ```→```, ```←```, and ```⏎```to navigate.

**Easy Configuration View**
* Relevant configurations in easy view with dropdown menus for quick changes.
* Configuration menus can also be accessed with keyboard shortcuts and arrow keys + enter for navigation.
* Model Configuration: Use the keyboard shortcut ```cmd+m``` to configure the active model and the keyboard shortcut ```cmd+e``` to configure the effort level.

**Provider Support**
* Currently only supports Codex subscription via OAuth.
* Planning more support in the future.

**Compaction Strategy**
* Summarizes old history into a structured local checkpoint.
* Preserves the most recent work exactly.
* Persists the checkpoint in SQLite.
* Makes every successive prompt look like ```summary + recent tail```

**Session Storage**
* Session transcripts are kept in a local, durable SQLite-backed store.

**Prompt Caching**
* Uses stable, per-session cache keys for consistent caching behavior.
* Custom option to display cache statistics after each message.

**Available Tools**
* ```read_file```, ```read_file_range```, ```list_dir```\
<sup> read tools </sup>
* ```search_files```, ```search_text```\
<sup> search tools, fff-backed </sup>
* apply_patch```\
<sup> edit tool </sup>
* ```exec_command```\
<sup> bash tool </sup>

**Configurable Permission Modes**
* ```read-only```
  * Allows access to ```read_file```, ```read_file_range```, ```list_dir```, ```search_files```, ```search_text```
* ```workspace-write```
  * Allows access to read-only tools and ```apply_patch```
* ```workspace-exec```
  * Allows access to all tools, including ```exec_command```
* Use the keyboard shortcut ```cmd+p``` and keyboard navigation to configure permission modes.

**Multi-Instance Support**
* Multiple sessions can be independently ran on the same machine without conflict.
* Use the keyboard shortcut ```cmd+n``` to start a new session.

**Terminal Passthrough**
* Switch to terminal mode with ```cmd+t``` to run terminal commands in the working directory.

**Transcript Search**
* Use the keyboard shortcut ```cmd+f``` to search the active transcript.

**Request Cancellation**
* Use the keyboard shortcut ```esc``` to cancel an active request.

Development
---
TODO.
