# rexedit

`rexedit` is an interactive terminal hex viewer, editor, and binary-analysis
workspace written in Rust with [Ratatui].

It supports mouse and keyboard selection, in-memory byte editing, asynchronous
search, typed value inspection, annotated binary fields, multiple open files,
side-by-side diffs, customizable themes, and entropy visualization.

## Features

- Synchronized hexadecimal and ASCII views
- Mouse and keyboard byte-range selection, including Ctrl + mouse-drag additive ranges
- In-memory byte overwrite mode with undo and redo
- Signed, unsigned, floating-point, binary, ASCII, and endian-aware inspection
- Asynchronous hexadecimal, decimal, binary, wildcard, and regex search
- Named and colored binary fields with descriptions
- JSON overlay save and load
- Multiple open binaries with independent editor state
- Side-by-side comparison and byte-difference highlighting
- Per-file Shannon entropy graph
- Custom themes and byte-coloring patterns
- Configurable 16- or 32-byte rows, casing, offsets, ASCII, and side panes
- Optional compression of long runs of uniform byte rows
- Persistent Python analysis console with a mutable byte-buffer snapshot
- Position-aware vertical scrollbars for the hex viewer and Python console
- Clickable and draggable scrollbars plus Python command history

## Requirements

- A terminal with color and mouse-event support
- [Rust] with Cargo
- Git, if cloning the repository

Rust 2024 edition support is required. Installing the current stable Rust
toolchain through `rustup` is recommended.

## Installation

### Windows

1. Install the Microsoft C++ build tools.

   Download **Visual Studio Build Tools** and select the **Desktop development
   with C++** workload. This provides the linker required by the default Rust
   MSVC toolchain.

2. Install Rust using [rustup for Windows], then open a new PowerShell window:

   ```powershell
   rustc --version
   cargo --version
   ```

3. Clone and enter the project:

   ```powershell
   git clone <repository-url>
   cd rexedit
   ```

4. Build an optimized executable:

   ```powershell
   cargo build --release
   ```

5. Run it:

   ```powershell
   .\target\release\rexedit.exe C:\path\to\file.bin
   ```

To install `rexedit` into Cargo's executable directory:

```powershell
cargo install --path .
rexedit C:\path\to\file.bin
```

The Windows file picker used by `Ctrl+N` is provided by Windows Forms and does
not require an additional package. The open dialog also lets you type a full or
relative path manually.

### Linux

1. Install compiler prerequisites.

   Debian, Ubuntu, and related distributions:

   ```bash
   sudo apt update
   sudo apt install build-essential curl git
   ```

   Fedora:

   ```bash
   sudo dnf install gcc gcc-c++ make curl git
   ```

   Arch Linux:

   ```bash
   sudo pacman -S base-devel curl git
   ```

2. Install Rust through `rustup`:

   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source "$HOME/.cargo/env"
   ```

3. Optionally install a graphical file picker for `Ctrl+N`:

   ```bash
   sudo apt install zenity
   ```

   `kdialog` is also supported. On WSL, `rexedit` uses the Windows file picker
   and translates the selected path with `wslpath`. You can always choose the
   manual-path option instead, including when no graphical picker is installed.

4. Clone, build, and run:

   ```bash
   git clone <repository-url>
   cd rexedit
   cargo build --release
   ./target/release/rexedit /path/to/file.bin
   ```

To install it into `~/.cargo/bin`:

```bash
cargo install --path .
rexedit /path/to/file.bin
```

## Usage

Open one binary:

```bash
rexedit firmware.bin
```

Launch without a file and open one later with `Ctrl+N`:

```bash
rexedit
```

Open several binaries in one workspace:

```bash
rexedit firmware-old.bin firmware-new.bin resources.dat
```

When running directly through Cargo:

```bash
cargo run --release -- firmware.bin
```

Command-line help:

```bash
rexedit --help
```

Press `?` inside the application to open the complete, scrollable keybinding
reference.

## Essential controls

### Workspace

| Keys | Action |
| --- | --- |
| `Ctrl+B`, then Right | Activate the next binary |
| `Ctrl+B`, then Left | Activate the previous binary |
| `Ctrl+B`, then `S` | Toggle side-by-side comparison |
| `Ctrl+N` | Choose the system file picker or type a full or relative path; Tab lists and cycles matches, while arrows/Page Up/Page Down or the mouse wheel navigate all suggestions |
| `Ctrl+W` | Close the active binary (press again to discard unsaved byte changes) |
| `Ctrl+D` | Toggle byte-difference highlighting |
| `e` / `Esc` | Show / hide entropy. In side-by-side diff mode, the panel shows absolute entropy differences. Calculations run in the background with progress feedback. |
| Mouse click on a tab or pane | Activate that binary |

Turning off side-by-side comparison also disables diff mode. Comparison panes
share the active file's row width and scroll offset so equivalent offsets remain
aligned.

### View Mode

| Keys | Action |
| --- | --- |
| Arrow keys | Move the byte cursor |
| Shift + arrows | Extend the selection |
| `gg` / `Shift+G` | Vim-style jump to the start or end of the file |
| Mouse drag | Select a byte range |
| Ctrl + mouse drag | Add a separate byte range (mouse-only, so it does not conflict with zsh or PowerShell keybindings) |
| Mouse wheel | Scroll |
| `i` | Enter Overwrite Mode |
| `Ctrl+F` | Search the binary |
| `n` / `N` | Next or previous search result |
| `Ctrl+G` | Jump to an offset |
| `Ctrl+C` / `Ctrl+Shift+C` (`Cmd+C` on macOS) | Copy all selected ranges as continuous hexadecimal, in file order (for example, `DEADBEEF`) |
| `Ctrl+U` / `Ctrl+R` | Undo or redo a byte overwrite |
| `Ctrl+S` | Save the edited binary |
| `a` | Create a field from the selection |
| `Tab` | Switch between the viewer and field pane |
| `Enter` | Edit the selected field |
| `d` / `Delete` | Delete the selected field |
| `Ctrl+O` / `Ctrl+L` | Save or load an overlay |
| `o` | Toggle field overlays |
| `p` | Open the Python buffer console |
| `s` | Open viewer settings |
| `t` | Open theme customization |
| `Ctrl+Z` | Suspend on Unix; resume with the shell built-in `fg` |

### Overwrite Mode

| Keys | Action |
| --- | --- |
| `0`–`9`, `A`–`F` | Overwrite the selected byte |
| `Insert` / `i` | Toggle between Overwrite and Insert Mode |
| `Backspace` / `Delete` | Delete the selected bytes |
| Arrow keys | Move the byte cursor |
| `Ctrl+C` / `Ctrl+Shift+C` (`Cmd+C` on macOS) | Copy all selected ranges as continuous hexadecimal |
| `Ctrl+V` / `Ctrl+Shift+V` (`Cmd+V` on macOS) | Paste hexadecimal from the system clipboard at the cursor |
| `Ctrl+U` / `Ctrl+R` | Undo or redo a byte overwrite |
| `Ctrl+S` | Save the edited binary |
| Escape | Return to View Mode |

`i` toggles Overwrite/Insert Mode the same as the `Insert` key, for keyboards
without a dedicated Insert key.

Paste reads the system clipboard directly (PowerShell's clipboard on Windows;
`pbpaste`, `wl-paste`, `xclip`, or `xsel` elsewhere) rather than relying on the
terminal to relay pasted text, and decodes every hexadecimal digit it finds
(ignoring spaces and other separators) as one batched edit. This is
deliberate: terminal-relayed "bracketed paste" is unreliable on several
platforms — most notably Windows, where it still arrives as a flood of
individual keystrokes — so a paste through the terminal can be slow on a large
clipboard. `Ctrl+V` / `Ctrl+Shift+V` (`Cmd+V` on macOS) is the fast, reliable
path regardless of terminal support. Both the plain and Shift-modified chords
do the same thing; use whichever matches habit on your platform (Windows and
Linux terminals conventionally use `Ctrl+Shift+C`/`V`, macOS uses `Cmd+C`/`V`).
Paste requires Overwrite or Insert Mode; from View Mode, press `i` first.

Byte edits remain in memory until the binary is saved. Modified bytes are
highlighted, and quitting with unsaved edits requires confirmation.

## Search syntax

Searches run on a worker thread and stream matches back to the interface, so
the binary remains navigable during long searches.

The search dialog briefly lists the available formats: hexadecimal, decimal,
binary, and regular expressions.

```text
DE AD BE EF
0xDEADBEEF
hex: DE AD ?? EF
dec: 65535
bin: 01001101 01011010
re:\x4D\x5A.{2}
```

| Format | Meaning |
| --- | --- |
| `DE AD BE EF` | Spaced hexadecimal bytes |
| `0xDEADBEEF` | Compact hexadecimal bytes |
| `hex: DE ?? BE` | Hexadecimal bytes with wildcards |
| `dec: 65535` | Unsigned decimal value |
| `bin: 01001101` | Binary byte sequence |
| `re:\x4D\x5A.` | Byte-oriented regular expression |

## Fields and overlays

Select a byte range and press `a` to create a named field. New field inputs are
blank; leaving start and end blank keeps the current selection. With separate
selections, rexedit creates one identically colored field per range. Fields support:

- editable start and end offsets;
- a name and description;
- a display color;
- selection, updating, and deletion.

Overlays are stored as JSON files and can be saved or loaded using user-selected
paths. The suggested filename is:

```text
<binary-name>.rexedit-overlay.json
```

## Themes and settings

Themes can customize hexadecimal, ASCII, offset, border, selection, search,
and modified-byte colors. Available byte-coloring modes include plain,
alternating bytes, byte classes, high- and low-nibble bands, zero-byte and
printable-byte emphasis, and four value bands.

Press `Ctrl+R` inside either the theme or settings menu to open a `y`/`n`
confirmation before restoring defaults.

Viewer settings include:

- showing or hiding the ASCII column;
- 16 or 32 bytes per row;
- uppercase or lowercase hexadecimal;
- showing or hiding offsets;
- showing or hiding the field and inspector panes.
- compressing runs of at least three full rows containing one repeated byte.

## Python buffer console

Press `p` in View Mode to start a persistent Python 3 subprocess for the
active binary. Python must be available as `python`, `python3`, or through the
`PYTHON` environment variable.

The console provides:

- `buffer`, a mutable `bytearray` snapshot of the complete binary;
- `selected`, a memory view for one range or a list of memory views for separate ranges;
- `selected_ranges` and `selection_ranges`, the individual views and their offset pairs;
- `selection_start` and `selection_end`;
- every open binary as `buffer_0`, `buffer_1`, and so on, with matching
  `selected_N`, `selected_ranges_N`, `selection_start_N`, and `selection_end_N`
  names; `buffers`, `selected_buffers`, and `selected_range_buffers` provide
  the same data as dictionaries;
- preloaded `struct`, `binascii`, `hashlib`, `base64`, `zlib`, `math`, `re`,
  `signal`, and `pathlib` modules.

Expressions print their result and variables persist between commands. Enter a
line ending in `:` to start a Python-style multi-line block, then submit a
blank continuation line to execute it. Run `rexedit_help()` for the complete
namespace. Enter `:apply` to copy same-length changes from every buffer back
into rexedit. A blank interpreter prompt records another prompt line without
executing code. Continuation history restores each line and its indentation;
block headers automatically indent the next line. Length
changes are rejected because the editor currently supports byte overwrite,
not insertion or deletion. Press `Ctrl+L` to clear the pane and Escape to
close it. Page Up/Page Down and the mouse wheel scroll output history;
`Ctrl+Home` and `Ctrl+End` jump to the oldest and newest output. Tab and
Shift+Tab cycle focus between the hex viewer, fields pane, and Python console
without ending the interpreter session. A vertical scrollbar tracks the
currently visible section of the interpreter history. Click or drag either
pane's scrollbar to jump through its content. Up and Down recall Python
commands, restoring unfinished input after moving past the newest command.
Command history survives closing and reopening the pane until rexedit exits.
You can enter hex overwrite mode (`i`) while the Python pane is open and the
viewer has focus. On `:apply`, independent Python and hex edits merge; when
both changed the same byte differently, the direct hex edit is retained and
rexedit reports the conflict.

Console output is bounded and wrapped into scrollable rows so large values
such as full binary buffers cannot hide the prompt. When output is truncated,
use a Python slice or summary (for example, `buffer[:64]` or `len(buffer)`).
`Ctrl+C` interrupts the currently running Python command without terminating
rexedit.

This keybinding is safe on both supported terminal families. Crossterm raw mode
delivers `Ctrl+C` to rexedit as keyboard input instead of terminating the
application. Rexedit forwards `SIGINT` to Python on Unix; on Windows, Python is
placed in its own process group and receives a console break event mapped to
`KeyboardInterrupt`.

The console executes code with the same operating-system permissions as
rexedit, so only run Python code you trust.

## Unix suspension

On Unix terminals, `Ctrl+Z` restores the terminal and suspends rexedit through
normal shell job control. Resume it with the shell built-in `fg`.

A separate `rfg` executable cannot reliably foreground the stopped process:
the parent shell owns the terminal's foreground process group and job table.
Windows terminals do not provide the equivalent POSIX job-control mechanism,
so `Ctrl+Z` reports that suspension is unavailable there.

## Development

Run the test suite:

```bash
cargo test
```

Check formatting:

```bash
cargo fmt -- --check
```

Run Clippy with warnings treated as errors:

```bash
cargo clippy --all-targets -- -D warnings
```

Build the optimized release executable:

```bash
cargo build --release
```

## License

Copyright (c) Dylan Nandlall.

This project is licensed under the MIT License. See [LICENSE](LICENSE).

[Ratatui]: https://ratatui.rs/
[Rust]: https://www.rust-lang.org/tools/install
[rustup for Windows]: https://rustup.rs/
