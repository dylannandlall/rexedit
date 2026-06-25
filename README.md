# rexedit

`rexedit` is an interactive terminal hex viewer, editor, and binary-analysis
workspace written in Rust with [Ratatui].

It supports mouse and keyboard selection, in-memory byte editing, asynchronous
search, typed value inspection, annotated binary fields, multiple open files,
side-by-side diffs, customizable themes, and entropy visualization.

## Features

- Synchronized hexadecimal and ASCII views
- Mouse and keyboard byte-range selection
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
not require an additional package.

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
   and translates the selected path with `wslpath`.

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
| `Ctrl+N` | Open another binary with the system file picker |
| `Ctrl+D` | Toggle byte-difference highlighting |
| `e` | Toggle the active binary's entropy graph |
| Mouse click on a tab or pane | Activate that binary |

Turning off side-by-side comparison also disables diff mode. Comparison panes
share the active file's row width and scroll offset so equivalent offsets remain
aligned.

### View Mode

| Keys | Action |
| --- | --- |
| Arrow keys | Move the byte cursor |
| Shift + arrows | Extend the selection |
| Mouse drag | Select a byte range |
| Mouse wheel | Scroll |
| `i` | Enter Overwrite Mode |
| `Ctrl+F` | Search the binary |
| `n` / `N` | Next or previous search result |
| `Ctrl+G` | Jump to an offset |
| `a` | Create a field from the selection |
| `Tab` | Switch between the viewer and field pane |
| `Enter` | Edit the selected field |
| `d` | Delete the selected field |
| `Ctrl+O` / `Ctrl+L` | Save or load an overlay |
| `s` | Open viewer settings |
| `t` | Open theme customization |

### Overwrite Mode

| Keys | Action |
| --- | --- |
| `0`–`9`, `A`–`F` | Overwrite the selected byte |
| Arrow keys | Move the byte cursor |
| `Ctrl+U` | Undo an overwrite |
| `Ctrl+R` | Redo an overwrite |
| `Ctrl+S` | Save the edited binary |
| Escape | Return to View Mode |

Byte edits remain in memory until the binary is saved. Modified bytes are
highlighted, and quitting with unsaved edits requires confirmation.

## Search syntax

Searches run on a worker thread and stream matches back to the interface, so
the binary remains navigable during long searches.

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

Select a byte range and press `a` to create a named field. Fields support:

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
alternating bytes, byte classes, and high-nibble bands.

Viewer settings include:

- showing or hiding the ASCII column;
- 16 or 32 bytes per row;
- uppercase or lowercase hexadecimal;
- showing or hiding offsets;
- showing or hiding the field and inspector panes.

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
