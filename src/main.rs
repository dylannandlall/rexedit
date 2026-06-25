mod app;
mod model;
mod search;
mod ui;

use std::{env, fs, io::stdout, path::PathBuf};

use app::{App, Workspace};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            "usage: rexedit <binary-file> [binary-file ...]"
        ));
    }
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
    execute!(stdout(), EnableMouseCapture)?;
    let result = workspace.run(&mut terminal);
    execute!(stdout(), DisableMouseCapture)?;
    ratatui::restore();

    result?;
    Ok(())
}

fn print_help() {
    println!(
        "rexedit <binary-file> [binary-file ...]

Workspace:
  Ctrl+B, Right     activate the next binary
  Ctrl+B, Left      activate the previous binary
  Ctrl+B, S         toggle side-by-side comparison
  Mouse click       activate a binary tab
  Ctrl+N            open another binary with the system file picker
  Ctrl+D            toggle byte diff
  e                 toggle active-binary entropy graph

View Mode:
  i                 enter Overwrite Mode
  s                 open viewer settings
  t                 open theme customization
  Ctrl+F            search bytes asynchronously
  n / N             next / previous search match
  Ctrl+G            jump to an offset
  Ctrl+O / Ctrl+L   save / load an overlay

Overwrite Mode:
  hexadecimal keys  overwrite the selected byte
  Ctrl+U / Ctrl+R   undo / redo a byte overwrite
  Ctrl+S            save the edited binary
  Escape            return to View Mode

Both modes:
  arrows            navigate bytes
  ?                 show the full keybinding reference
  q                 quit"
    );
}
