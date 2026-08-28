mod app;
mod entropy;
mod model;
mod python;
mod search;
mod ui;

use std::{env, fs, io::stdout, path::PathBuf};

use app::{App, Workspace};
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| matches!(argument.to_str(), Some("-h" | "--help")))
    {
        print_help();
        return Ok(());
    }
    let documents = arguments
        .into_iter()
        .map(|argument| {
            let path = PathBuf::from(argument);
            let bytes = fs::read(&path)?;
            Ok(App::new(path, bytes))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut workspace = Workspace::new(documents);

    let mut terminal = ratatui::init();
    execute!(stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let result = workspace.run(&mut terminal);
    execute!(stdout(), DisableBracketedPaste, DisableMouseCapture)?;
    ratatui::restore();

    result?;
    Ok(())
}

fn print_help() {
    println!(
        "rexedit [binary-file ...]

Workspace:
  Ctrl+B, Right     activate the next binary
  Ctrl+B, Left      activate the previous binary
  Ctrl+B, S         toggle side-by-side comparison
  Mouse click       activate a binary tab
  Ctrl+N            choose a system picker or type a binary path
  Ctrl+W            close the active binary (press again if unsaved)
  Ctrl+D            toggle byte diff
  e                 toggle active-binary entropy graph

View Mode:
  i                 enter byte edit mode (Overwrite initially)
  s                 open viewer settings
  t                 open theme customization
  p                 open the Python buffer console
  Ctrl+F            search bytes asynchronously
  n / N             next / previous search match
  Ctrl+G            jump to an offset
  Ctrl+C / Ctrl+Shift+C (Cmd+C on macOS)   copy the selection as continuous hexadecimal
  gg / G            jump to the start / end of the file
  Ctrl+U / Ctrl+R   undo / redo byte edits
  Ctrl+S            save the edited binary
  Ctrl+O / Ctrl+L   save / load an overlay (auto-saved per file)
  o                 toggle field overlays
  Ctrl+R            reset an open theme/settings menu with y/n confirmation
  Ctrl+Z            suspend to the shell on Unix (resume with fg)

Byte Edit Mode:
  hexadecimal keys  edit a byte using two hexadecimal digits
  Insert / i        switch between Overwrite and Insert Mode
  Backspace/Delete  delete the selected byte range
  Ctrl+C / Ctrl+Shift+C (Cmd+C on macOS)   copy the selection as continuous hexadecimal
  Ctrl+V / Ctrl+Shift+V (Cmd+V on macOS)   paste hexadecimal from the clipboard
  Ctrl+U / Ctrl+R   undo / redo overwrite, insertion, or deletion
  Ctrl+S            save the edited binary
  Escape            return to View Mode

Both modes:
  arrows            navigate bytes
  ?                 show the full keybinding reference
  q                 quit"
    );
}
