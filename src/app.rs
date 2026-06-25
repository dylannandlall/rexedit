use std::{
    collections::BTreeSet,
    env, fs,
    io::{self},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::{DefaultTerminal, layout::Rect};

use crate::{
    model::{
        ByteColorMode, DEFAULT_BYTES_PER_ROW, Field, FieldColor, NamedColor, Overlay, SearchMatch,
        Selection, Theme,
    },
    search::{self, SearchMessage, SearchWorker},
    ui,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Viewer,
    Fields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathAction {
    SaveOverlay,
    LoadOverlay,
    SaveBinary,
    SaveTheme,
    LoadTheme,
}

#[derive(Debug)]
pub struct PathDialog {
    pub action: PathAction,
    pub input: TextInput,
}

#[derive(Debug)]
pub enum Mode {
    Normal,
    Search(TextInput),
    Jump(TextInput),
    Field(FieldEditor),
    Path(PathDialog),
    Theme(ThemeEditor),
    Settings(SettingsEditor),
    Help(HelpViewer),
}

#[derive(Debug, Default)]
pub struct TextInput {
    pub value: String,
}

impl TextInput {
    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.value.push(character);
            }
            KeyCode::Backspace => {
                self.value.pop();
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct FieldEditor {
    pub editing: Option<usize>,
    pub name: String,
    pub description: String,
    pub start: String,
    pub end: String,
    pub color: FieldColor,
    pub active: usize,
}

impl FieldEditor {
    fn new(selection: Selection, number: usize) -> Self {
        Self {
            editing: None,
            name: format!("field_{number}"),
            description: String::new(),
            start: format!("0x{:X}", selection.start()),
            end: format!("0x{:X}", selection.end()),
            color: FieldColor::default(),
            active: 0,
        }
    }

    fn from_field(index: usize, field: &Field) -> Self {
        Self {
            editing: Some(index),
            name: field.name.clone(),
            description: field.description.clone(),
            start: format!("0x{:X}", field.start),
            end: format!("0x{:X}", field.end),
            color: field.color,
            active: 0,
        }
    }

    fn active_text_mut(&mut self) -> Option<&mut String> {
        match self.active {
            0 => Some(&mut self.name),
            1 => Some(&mut self.description),
            2 => Some(&mut self.start),
            3 => Some(&mut self.end),
            _ => None,
        }
    }

    fn handle_text_key(&mut self, key: KeyEvent) {
        let Some(input) = self.active_text_mut() else {
            return;
        };
        match key.code {
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                input.push(character);
            }
            KeyCode::Backspace => {
                input.pop();
            }
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
pub struct ThemeEditor {
    pub active: usize,
}

#[derive(Debug, Default)]
pub struct SettingsEditor {
    pub active: usize,
}

#[derive(Debug, Default)]
pub struct HelpViewer {
    pub scroll: usize,
}

#[derive(Clone, Debug)]
pub struct ViewerSettings {
    pub show_ascii: bool,
    pub bytes_per_row: usize,
    pub uppercase_hex: bool,
    pub show_offsets: bool,
    pub show_sidebar: bool,
}

impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            show_ascii: true,
            bytes_per_row: DEFAULT_BYTES_PER_ROW,
            uppercase_hex: true,
            show_offsets: true,
            show_sidebar: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditAction {
    offset: usize,
    before: u8,
    after: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingEdit {
    offset: usize,
    before: u8,
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<SearchMatch>,
    pub current: usize,
    pub has_active_result: bool,
    pub running: bool,
    pub scanned: usize,
    pub total: usize,
    worker: Option<SearchWorker>,
}

pub struct App {
    pub path: PathBuf,
    pub bytes: Arc<Vec<u8>>,
    saved_bytes: Arc<Vec<u8>>,
    pub scroll: usize,
    pub visible_rows: usize,
    pub selection: Option<Selection>,
    pub fields: Vec<Field>,
    pub selected_field: usize,
    pub focus: Focus,
    pub search: SearchState,
    pub mode: Mode,
    pub status: String,
    pub viewer_area: Rect,
    pub fields_area: Rect,
    pub theme: Theme,
    pub settings: ViewerSettings,
    pub edit_mode: bool,
    pub edit_high_nibble: bool,
    pub modified_offsets: BTreeSet<usize>,
    undo_stack: Vec<EditAction>,
    redo_stack: Vec<EditAction>,
    pending_edit: Option<PendingEdit>,
    mouse_dragging: bool,
    quit_armed: bool,
    pub entropy: Option<Vec<f64>>,
}

impl App {
    pub fn new(path: PathBuf, bytes: Vec<u8>) -> Self {
        let selection = (!bytes.is_empty()).then(|| Selection::new(0));
        Self {
            path,
            saved_bytes: Arc::new(bytes.clone()),
            bytes: Arc::new(bytes),
            scroll: 0,
            visible_rows: 1,
            selection,
            fields: Vec::new(),
            selected_field: 0,
            focus: Focus::Viewer,
            search: SearchState::default(),
            mode: Mode::Normal,
            status: "Ready".into(),
            viewer_area: Rect::default(),
            fields_area: Rect::default(),
            theme: Theme::default(),
            settings: ViewerSettings::default(),
            edit_mode: false,
            edit_high_nibble: true,
            modified_offsets: BTreeSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending_edit: None,
            mouse_dragging: false,
            quit_armed: false,
            entropy: None,
        }
    }

    pub fn entropy_profile(&mut self) -> &[f64] {
        self.entropy
            .get_or_insert_with(|| calculate_entropy(&self.bytes))
    }
}

pub struct Workspace {
    pub documents: Vec<App>,
    pub active: usize,
    pub side_by_side: bool,
    pub diff_mode: bool,
    pub show_entropy: bool,
    pub status: String,
    pub tab_hitboxes: Vec<(u16, u16)>,
    pub tab_row: u16,
    pub comparison_panes: Vec<Rect>,
    tab_switch_pending: bool,
}

impl Workspace {
    pub fn new(documents: Vec<App>) -> Self {
        Self {
            documents,
            active: 0,
            side_by_side: false,
            diff_mode: false,
            show_entropy: false,
            status: String::new(),
            tab_hitboxes: Vec::new(),
            tab_row: 0,
            comparison_panes: Vec::new(),
            tab_switch_pending: false,
        }
    }

    pub fn active(&self) -> &App {
        &self.documents[self.active]
    }

    pub fn active_mut(&mut self) -> &mut App {
        &mut self.documents[self.active]
    }

    pub fn comparison_index(&self) -> Option<usize> {
        if self.documents.len() > 1 {
            Some((self.active + 1) % self.documents.len())
        } else {
            None
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            for document in &mut self.documents {
                document.drain_search_messages();
            }
            terminal.draw(|frame| ui::render_workspace(frame, self))?;
            if !event::poll(Duration::from_millis(40))? {
                continue;
            }
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && self.handle_workspace_key(key)? =>
                {
                    for document in &mut self.documents {
                        document.cancel_search();
                    }
                    return Ok(());
                }
                Event::Mouse(mouse) => self.handle_workspace_mouse(mouse),
                _ => {}
            }
        }
    }

    fn handle_workspace_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if self.tab_switch_pending {
            self.tab_switch_pending = false;
            match key.code {
                KeyCode::Left => self.select_previous_document(),
                KeyCode::Right => self.select_next_document(),
                KeyCode::Char('s') if !self.active().edit_mode => self.toggle_side_by_side(),
                KeyCode::Char('s') => {
                    self.status = "Side-by-side comparison is available in View Mode".into();
                }
                _ => self.status = "Tab switch cancelled".into(),
            }
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('b') => {
                    self.tab_switch_pending = true;
                    self.status = "Ctrl+B: press Left or Right to switch binary".into();
                    return Ok(false);
                }
                KeyCode::Char('n')
                    if !self.active().edit_mode && matches!(self.active().mode, Mode::Normal) =>
                {
                    self.open_binary_picker();
                    return Ok(false);
                }
                KeyCode::Char('d')
                    if !self.active().edit_mode && matches!(self.active().mode, Mode::Normal) =>
                {
                    if self.documents.len() < 2 {
                        self.status = "Open at least two binaries to diff".into();
                    } else {
                        self.diff_mode = !self.diff_mode;
                        self.side_by_side |= self.diff_mode;
                    }
                    return Ok(false);
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Char('e')
            && !self.active().edit_mode
            && matches!(self.active().mode, Mode::Normal)
        {
            self.show_entropy = !self.show_entropy;
            if self.show_entropy {
                self.active_mut().entropy_profile();
            }
            return Ok(false);
        }
        self.active_mut().handle_key(key)
    }

    pub(crate) fn handle_workspace_mouse(&mut self, mouse: MouseEvent) {
        if mouse.row == self.tab_row
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(index) = self
                .tab_hitboxes
                .iter()
                .position(|(start, end)| (*start..*end).contains(&mouse.column))
        {
            self.select_document(index);
            return;
        }
        if self.side_by_side
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(index) = self
                .comparison_panes
                .iter()
                .position(|area| area.contains((mouse.column, mouse.row).into()))
        {
            self.select_document(index);
        }
        self.active_mut().handle_mouse(mouse);
    }

    fn toggle_side_by_side(&mut self) {
        if self.documents.len() < 2 {
            self.status = "Open at least two binaries for side-by-side view".into();
            return;
        }
        self.side_by_side = !self.side_by_side;
        if !self.side_by_side {
            self.diff_mode = false;
            self.comparison_panes.clear();
            self.status = "Side-by-side comparison disabled".into();
        } else {
            self.status = "Side-by-side comparison enabled".into();
        }
    }

    fn select_next_document(&mut self) {
        self.select_document((self.active + 1) % self.documents.len());
    }

    fn select_previous_document(&mut self) {
        let index = self
            .active
            .checked_sub(1)
            .unwrap_or(self.documents.len() - 1);
        self.select_document(index);
    }

    fn select_document(&mut self, index: usize) {
        if index >= self.documents.len() {
            return;
        }
        self.active = index;
        if self.show_entropy {
            self.active_mut().entropy_profile();
        }
        self.status = format!("Active binary: {}", self.active().path.display());
    }

    fn open_binary_picker(&mut self) {
        match pick_binary_file() {
            Ok(Some(path)) => match fs::read(&path) {
                Ok(bytes) => {
                    self.documents.push(App::new(path.clone(), bytes));
                    self.active = self.documents.len() - 1;
                    if self.show_entropy {
                        self.active_mut().entropy_profile();
                    }
                    self.status = format!("Opened {}", path.display());
                }
                Err(error) => {
                    let message = format!("Could not open {}: {error}", path.display());
                    self.status = message.clone();
                    self.active_mut().status = message;
                }
            },
            Ok(None) => self.status = "Open cancelled".into(),
            Err(error) => {
                self.status = error.clone();
                self.active_mut().status = error;
            }
        }
    }
}

impl App {
    pub fn row_count(&self) -> usize {
        self.bytes.len().div_ceil(self.settings.bytes_per_row)
    }

    pub fn max_scroll(&self) -> usize {
        self.row_count().saturating_sub(self.visible_rows)
    }

    pub fn current_bytes(&self) -> &[u8] {
        let Some(selection) = self.selection else {
            return &[];
        };
        let Some(last) = self.bytes.len().checked_sub(1) else {
            return &[];
        };
        let start = selection.start().min(last);
        let end = selection.end().min(last);
        &self.bytes[start..=end]
    }

    pub fn active_search_match(&self) -> Option<&SearchMatch> {
        if self.search.has_active_result {
            self.search.results.get(self.search.current)
        } else {
            None
        }
    }

    pub fn is_search_match(&self, offset: usize) -> bool {
        let insertion = self
            .search
            .results
            .partition_point(|found| found.start <= offset);
        insertion > 0 && self.search.results[insertion - 1].contains(offset)
    }

    pub fn ensure_visible(&mut self, offset: usize) {
        let row = offset / self.settings.bytes_per_row;
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll.saturating_add(self.visible_rows) {
            self.scroll = row.saturating_sub(self.visible_rows.saturating_sub(1));
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn theme_color_for_byte(&self, offset: usize, byte: u8, ascii: bool) -> NamedColor {
        if ascii {
            return self.theme.ascii;
        }
        match self.theme.byte_mode {
            ByteColorMode::Plain => self.theme.hex_primary,
            ByteColorMode::Alternating => {
                if offset.is_multiple_of(2) {
                    self.theme.hex_primary
                } else {
                    self.theme.hex_secondary
                }
            }
            ByteColorMode::ByteClass => {
                if byte == 0 {
                    self.theme.offset
                } else if byte.is_ascii_graphic() || byte == b' ' {
                    self.theme.hex_secondary
                } else if byte.is_ascii_control() {
                    self.theme.modified
                } else {
                    self.theme.hex_primary
                }
            }
            ByteColorMode::HighNibble => {
                if byte >> 4 < 8 {
                    self.theme.hex_primary
                } else {
                    self.theme.hex_secondary
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        match &self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Search(_) => {
                self.handle_search_key(key);
                Ok(false)
            }
            Mode::Jump(_) => {
                self.handle_jump_key(key);
                Ok(false)
            }
            Mode::Field(_) => {
                self.handle_field_key(key);
                Ok(false)
            }
            Mode::Path(_) => {
                self.handle_path_key(key);
                Ok(false)
            }
            Mode::Theme(_) => {
                self.handle_theme_key(key);
                Ok(false)
            }
            Mode::Settings(_) => {
                self.handle_settings_key(key);
                Ok(false)
            }
            Mode::Help(_) => {
                self.handle_help_key(key);
                Ok(false)
            }
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if key.code == KeyCode::Char('?') {
            self.mode = Mode::Help(HelpViewer::default());
            return Ok(false);
        }
        if self.edit_mode {
            return self.handle_overwrite_key(key);
        }
        self.handle_view_key(key)
    }

    fn handle_view_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') => {
                    self.mode = Mode::Search(TextInput {
                        value: self.search.query.clone(),
                    });
                }
                KeyCode::Char('g') => self.mode = Mode::Jump(TextInput::default()),
                KeyCode::Char('o') => self.open_path_dialog(PathAction::SaveOverlay),
                KeyCode::Char('l') => self.open_path_dialog(PathAction::LoadOverlay),
                KeyCode::Up => self.previous_search_result(),
                KeyCode::Down => self.next_search_result(),
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') => {
                if !self.modified_offsets.is_empty() && !self.quit_armed {
                    self.quit_armed = true;
                    self.status = "Unsaved byte changes: press q again to quit".into();
                } else {
                    return Ok(true);
                }
            }
            KeyCode::Char('i') if self.focus == Focus::Viewer => {
                self.edit_mode = true;
                self.edit_high_nibble = true;
                self.status =
                    "Overwrite Mode: type two hex digits per byte; Esc returns to View Mode".into();
            }
            KeyCode::Char('t') => self.mode = Mode::Theme(ThemeEditor::default()),
            KeyCode::Char('s') => self.mode = Mode::Settings(SettingsEditor::default()),
            KeyCode::Char('n') => self.next_search_result(),
            KeyCode::Char('N') => self.previous_search_result(),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Viewer => Focus::Fields,
                    Focus::Fields => Focus::Viewer,
                };
            }
            KeyCode::Char('a') => {
                if let Some(selection) = self.selection {
                    self.mode = Mode::Field(FieldEditor::new(selection, self.fields.len() + 1));
                }
            }
            KeyCode::Enter if self.focus == Focus::Fields => self.edit_selected_field(),
            KeyCode::Char('d') if self.focus == Focus::Fields => self.delete_selected_field(),
            KeyCode::Up if self.focus == Focus::Fields => self.select_previous_field(),
            KeyCode::Down if self.focus == Focus::Fields => self.select_next_field(),
            KeyCode::Char('[') => self.select_previous_field(),
            KeyCode::Char(']') => self.select_next_field(),
            KeyCode::Left => self.move_cursor(-1, key.modifiers.contains(KeyModifiers::SHIFT)),
            KeyCode::Right => self.move_cursor(1, key.modifiers.contains(KeyModifiers::SHIFT)),
            KeyCode::Up => self.move_cursor(
                -(self.settings.bytes_per_row as isize),
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::Down => self.move_cursor(
                self.settings.bytes_per_row as isize,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::PageUp => self.move_cursor(
                -(self
                    .visible_rows
                    .saturating_mul(self.settings.bytes_per_row) as isize),
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::PageDown => self.move_cursor(
                self.visible_rows
                    .saturating_mul(self.settings.bytes_per_row) as isize,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::Home => self.select_offset(0, key.modifiers.contains(KeyModifiers::SHIFT)),
            KeyCode::End if !self.bytes.is_empty() => {
                self.select_offset(
                    self.bytes.len() - 1,
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
            }
            _ => {}
        }
        if key.code != KeyCode::Char('q') {
            self.quit_armed = false;
        }
        Ok(false)
    }

    fn handle_overwrite_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => self.undo_overwrite(),
                KeyCode::Char('r') => self.redo_overwrite(),
                KeyCode::Char('s') => {
                    self.commit_pending_edit();
                    self.open_path_dialog(PathAction::SaveBinary);
                }
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Esc => {
                self.commit_pending_edit();
                self.edit_mode = false;
                self.edit_high_nibble = true;
                self.status = "View Mode".into();
            }
            KeyCode::Char('q') => {
                self.commit_pending_edit();
                if !self.modified_offsets.is_empty() && !self.quit_armed {
                    self.quit_armed = true;
                    self.status = "Unsaved byte changes: press q again to quit".into();
                } else {
                    return Ok(true);
                }
            }
            KeyCode::Char(character) if character.is_ascii_hexdigit() => {
                let nibble = character.to_digit(16).expect("checked hex digit") as u8;
                self.overwrite_nibble(nibble);
            }
            KeyCode::Left => {
                self.commit_pending_edit();
                self.move_cursor(-1, false);
            }
            KeyCode::Right => {
                self.commit_pending_edit();
                self.move_cursor(1, false);
            }
            KeyCode::Up => {
                self.commit_pending_edit();
                self.move_cursor(-(self.settings.bytes_per_row as isize), false);
            }
            KeyCode::Down => {
                self.commit_pending_edit();
                self.move_cursor(self.settings.bytes_per_row as isize, false);
            }
            KeyCode::PageUp => {
                self.commit_pending_edit();
                self.move_cursor(
                    -(self
                        .visible_rows
                        .saturating_mul(self.settings.bytes_per_row)
                        as isize),
                    false,
                );
            }
            KeyCode::PageDown => {
                self.commit_pending_edit();
                self.move_cursor(
                    self.visible_rows
                        .saturating_mul(self.settings.bytes_per_row) as isize,
                    false,
                );
            }
            KeyCode::Home => {
                self.commit_pending_edit();
                self.select_offset(0, false);
            }
            KeyCode::End if !self.bytes.is_empty() => {
                self.commit_pending_edit();
                self.select_offset(self.bytes.len() - 1, false);
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let query = match &self.mode {
                    Mode::Search(input) => input.value.clone(),
                    _ => return,
                };
                self.mode = Mode::Normal;
                self.start_search(query);
            }
            _ => {
                if let Mode::Search(input) = &mut self.mode {
                    input.handle_key(key);
                }
            }
        }
    }

    fn handle_jump_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let value = match &self.mode {
                    Mode::Jump(input) => input.value.clone(),
                    _ => return,
                };
                match parse_offset(&value) {
                    Ok(offset) if offset < self.bytes.len() => {
                        self.selection = Some(Selection::new(offset));
                        self.ensure_visible(offset);
                        self.status = format!("Jumped to 0x{offset:X}");
                        self.mode = Mode::Normal;
                    }
                    Ok(offset) => {
                        self.status = format!(
                            "Offset 0x{offset:X} is outside this {} byte file",
                            self.bytes.len()
                        );
                    }
                    Err(error) => self.status = error,
                }
            }
            _ => {
                if let Mode::Jump(input) = &mut self.mode {
                    input.handle_key(key);
                }
            }
        }
    }

    fn handle_field_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Tab | KeyCode::Down => {
                if let Mode::Field(editor) = &mut self.mode {
                    editor.active = (editor.active + 1) % 5;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Mode::Field(editor) = &mut self.mode {
                    editor.active = editor.active.checked_sub(1).unwrap_or(4);
                }
            }
            KeyCode::Left | KeyCode::Right => {
                if let Mode::Field(editor) = &mut self.mode
                    && editor.active == 4
                {
                    editor.color = editor.color.next();
                }
            }
            KeyCode::Enter => self.commit_field_editor(),
            _ => {
                if let Mode::Field(editor) = &mut self.mode {
                    editor.handle_text_key(key);
                }
            }
        }
    }

    fn handle_path_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let (action, value) = match &self.mode {
                    Mode::Path(dialog) => (dialog.action, dialog.input.value.clone()),
                    _ => return,
                };
                self.perform_path_action(action, PathBuf::from(value.trim()));
            }
            _ => {
                if let Mode::Path(dialog) = &mut self.mode {
                    dialog.input.handle_key(key);
                }
            }
        }
    }

    fn handle_theme_key(&mut self, key: KeyEvent) {
        let active = match &self.mode {
            Mode::Theme(editor) => editor.active,
            _ => return,
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => self.open_path_dialog(PathAction::SaveTheme),
                KeyCode::Char('l') => self.open_path_dialog(PathAction::LoadTheme),
                KeyCode::Char('u') if active == 0 => self.theme.name.clear(),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Tab | KeyCode::Down => {
                if let Mode::Theme(editor) = &mut self.mode {
                    editor.active = (editor.active + 1) % 10;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Mode::Theme(editor) = &mut self.mode {
                    editor.active = editor.active.checked_sub(1).unwrap_or(9);
                }
            }
            KeyCode::Left => self.change_theme_value(active, false),
            KeyCode::Right => self.change_theme_value(active, true),
            KeyCode::Backspace if active == 0 => {
                self.theme.name.pop();
            }
            KeyCode::Char(character) if active == 0 => self.theme.name.push(character),
            _ => {}
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        let active = match &self.mode {
            Mode::Settings(editor) => editor.active,
            _ => return,
        };
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Tab | KeyCode::Down => {
                if let Mode::Settings(editor) = &mut self.mode {
                    editor.active = (editor.active + 1) % 5;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Mode::Settings(editor) = &mut self.mode {
                    editor.active = editor.active.checked_sub(1).unwrap_or(4);
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                self.toggle_setting(active);
            }
            _ => {}
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        let Mode::Help(help) = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                help.scroll = help.scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                help.scroll = help.scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                help.scroll = help.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                help.scroll = help.scroll.saturating_add(10);
            }
            KeyCode::Home => help.scroll = 0,
            KeyCode::End => help.scroll = usize::MAX,
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(3).min(self.max_scroll());
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(offset) = self.byte_at(mouse.column, mouse.row) {
                    if self.edit_mode {
                        self.commit_pending_edit();
                    }
                    self.selection = Some(Selection::new(offset));
                    self.focus = Focus::Viewer;
                    self.mouse_dragging = true;
                    self.edit_high_nibble = true;
                } else if !self.edit_mode
                    && let Some(index) = self.field_at(mouse.column, mouse.row)
                {
                    self.selected_field = index;
                    self.focus = Focus::Fields;
                    self.activate_selected_field();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.mouse_dragging => {
                if let Some(offset) = self.byte_at(mouse.column, mouse.row)
                    && let Some(selection) = &mut self.selection
                {
                    selection.cursor = offset;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.mouse_dragging = false,
            _ => {}
        }
    }

    fn byte_at(&self, column: u16, row: u16) -> Option<usize> {
        let area = self.viewer_area;
        let inner_left = area.x.checked_add(1)?;
        let inner_top = area.y.checked_add(1)?;
        let inner_right = area.right().checked_sub(1)?;
        let inner_bottom = area.bottom().checked_sub(1)?;
        if column < inner_left || column >= inner_right || row < inner_top || row >= inner_bottom {
            return None;
        }
        let local_row = row.checked_sub(inner_top)?;
        let local_x = column.checked_sub(inner_left)?;
        let data_row = self.scroll.checked_add(usize::from(local_row))?;
        let prefix_width = if self.settings.show_offsets { 10 } else { 0 };
        let hex_width =
            self.settings.bytes_per_row * 3 + self.settings.bytes_per_row.saturating_sub(1) / 8;
        let local_x = usize::from(local_x);
        let byte_column = if (prefix_width..prefix_width + hex_width).contains(&local_x) {
            (0..self.settings.bytes_per_row).find(|index| {
                let start = prefix_width + index * 3 + index / 8;
                (start..start + 3).contains(&local_x)
            })
        } else if self.settings.show_ascii {
            let ascii_start = prefix_width + hex_width + 2;
            let ascii_end = ascii_start.checked_add(self.settings.bytes_per_row)?;
            if (ascii_start..ascii_end).contains(&local_x) {
                local_x.checked_sub(ascii_start)
            } else {
                None
            }
        } else {
            None
        }?;
        let offset = data_row
            .checked_mul(self.settings.bytes_per_row)?
            .checked_add(byte_column)?;
        if offset < self.bytes.len() {
            Some(offset)
        } else {
            None
        }
    }

    fn field_at(&self, column: u16, row: u16) -> Option<usize> {
        let area = self.fields_area;
        let inner_left = area.x.checked_add(1)?;
        let inner_top = area.y.checked_add(1)?;
        let inner_right = area.right().checked_sub(1)?;
        let inner_bottom = area.bottom().checked_sub(1)?;
        if column < inner_left || column >= inner_right || row < inner_top || row >= inner_bottom {
            return None;
        }
        let index = usize::from(row.checked_sub(inner_top)?);
        if index < self.fields.len() {
            Some(index)
        } else {
            None
        }
    }

    fn move_cursor(&mut self, delta: isize, extend: bool) {
        if self.bytes.is_empty() {
            return;
        }
        let current = self.selection.map_or(0, |selection| selection.cursor);
        let offset = current
            .saturating_add_signed(delta)
            .min(self.bytes.len() - 1);
        self.select_offset(offset, extend);
        self.edit_high_nibble = true;
    }

    fn select_offset(&mut self, offset: usize, extend: bool) {
        if self.bytes.is_empty() {
            return;
        }
        if extend {
            if let Some(selection) = &mut self.selection {
                selection.cursor = offset;
            } else {
                self.selection = Some(Selection::new(offset));
            }
        } else {
            self.selection = Some(Selection::new(offset));
        }
        self.ensure_visible(offset);
    }

    fn overwrite_nibble(&mut self, nibble: u8) {
        let Some(offset) = self.selection.map(|selection| selection.cursor) else {
            return;
        };
        self.cancel_search();
        self.search.results.clear();
        let bytes = Arc::make_mut(&mut self.bytes);
        let original = bytes[offset];
        if self.edit_high_nibble {
            self.pending_edit = Some(PendingEdit {
                offset,
                before: original,
            });
        }
        bytes[offset] = if self.edit_high_nibble {
            (nibble << 4) | (original & 0x0F)
        } else {
            (original & 0xF0) | nibble
        };
        self.entropy = None;
        self.update_dirty_offset(offset);
        if self.edit_high_nibble {
            self.edit_high_nibble = false;
        } else {
            self.commit_pending_edit();
            self.edit_high_nibble = true;
            let next = (offset + 1).min(self.bytes.len() - 1);
            self.selection = Some(Selection::new(next));
            self.ensure_visible(next);
        }
        self.status = format!("Modified byte at 0x{offset:X}; Ctrl+S saves");
    }

    fn commit_pending_edit(&mut self) {
        let Some(pending) = self.pending_edit.take() else {
            return;
        };
        let after = self.bytes[pending.offset];
        if pending.before != after {
            self.undo_stack.push(EditAction {
                offset: pending.offset,
                before: pending.before,
                after,
            });
            self.redo_stack.clear();
        }
        self.edit_high_nibble = true;
    }

    fn undo_overwrite(&mut self) {
        self.commit_pending_edit();
        let Some(action) = self.undo_stack.pop() else {
            self.status = "Nothing to undo".into();
            return;
        };
        self.cancel_search();
        self.search.results.clear();
        Arc::make_mut(&mut self.bytes)[action.offset] = action.before;
        self.entropy = None;
        self.update_dirty_offset(action.offset);
        self.redo_stack.push(action);
        self.selection = Some(Selection::new(action.offset));
        self.ensure_visible(action.offset);
        self.status = format!("Undid overwrite at 0x{:X}", action.offset);
    }

    fn redo_overwrite(&mut self) {
        self.commit_pending_edit();
        let Some(action) = self.redo_stack.pop() else {
            self.status = "Nothing to redo".into();
            return;
        };
        self.cancel_search();
        self.search.results.clear();
        Arc::make_mut(&mut self.bytes)[action.offset] = action.after;
        self.entropy = None;
        self.update_dirty_offset(action.offset);
        self.undo_stack.push(action);
        self.selection = Some(Selection::new(action.offset));
        self.ensure_visible(action.offset);
        self.status = format!("Redid overwrite at 0x{:X}", action.offset);
    }

    fn update_dirty_offset(&mut self, offset: usize) {
        if self.bytes[offset] == self.saved_bytes[offset] {
            self.modified_offsets.remove(&offset);
        } else {
            self.modified_offsets.insert(offset);
        }
    }

    fn toggle_setting(&mut self, active: usize) {
        match active {
            0 => self.settings.show_ascii = !self.settings.show_ascii,
            1 => {
                self.settings.bytes_per_row = if self.settings.bytes_per_row == 16 {
                    32
                } else {
                    16
                };
                if let Some(selection) = self.selection {
                    self.ensure_visible(selection.cursor);
                }
            }
            2 => self.settings.uppercase_hex = !self.settings.uppercase_hex,
            3 => self.settings.show_offsets = !self.settings.show_offsets,
            4 => self.settings.show_sidebar = !self.settings.show_sidebar,
            _ => {}
        }
    }

    fn edit_selected_field(&mut self) {
        if let Some(field) = self.fields.get(self.selected_field) {
            self.mode = Mode::Field(FieldEditor::from_field(self.selected_field, field));
        }
    }

    fn commit_field_editor(&mut self) {
        let Mode::Field(editor) = &self.mode else {
            return;
        };
        let start = match parse_offset(&editor.start) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("Invalid start: {error}");
                return;
            }
        };
        let end = match parse_offset(&editor.end) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("Invalid end: {error}");
                return;
            }
        };
        if self.bytes.is_empty() || start > end || end >= self.bytes.len() {
            self.status = "Field range must be ordered and inside the file".into();
            return;
        }
        let field = Field {
            name: if editor.name.trim().is_empty() {
                format!("field_{}", self.fields.len() + 1)
            } else {
                editor.name.trim().to_owned()
            },
            description: editor.description.trim().to_owned(),
            start,
            end,
            color: editor.color,
        };
        if let Some(index) = editor.editing {
            self.fields[index] = field;
            self.selected_field = index;
            self.status = "Field updated".into();
        } else {
            self.fields.push(field);
            self.selected_field = self.fields.len() - 1;
            self.status = "Field added".into();
        }
        self.mode = Mode::Normal;
        self.activate_selected_field();
    }

    fn delete_selected_field(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        let removed = self.fields.remove(self.selected_field);
        self.selected_field = self.selected_field.min(self.fields.len().saturating_sub(1));
        self.status = format!("Deleted field '{}'", removed.name);
    }

    fn select_previous_field(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        self.selected_field = self.selected_field.saturating_sub(1);
        self.focus = Focus::Fields;
        self.activate_selected_field();
    }

    fn select_next_field(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        self.selected_field = (self.selected_field + 1).min(self.fields.len() - 1);
        self.focus = Focus::Fields;
        self.activate_selected_field();
    }

    fn activate_selected_field(&mut self) {
        if let Some(field) = self.fields.get(self.selected_field) {
            let (start, end) = (field.start, field.end);
            self.selection = Some(Selection {
                anchor: start,
                cursor: end,
            });
            self.ensure_visible(start);
        }
    }

    fn start_search(&mut self, query: String) {
        self.cancel_search();
        match search::spawn(Arc::clone(&self.bytes), query.clone()) {
            Ok(worker) => {
                self.search.query = query;
                self.search.results.clear();
                self.search.current = 0;
                self.search.has_active_result = false;
                self.search.running = true;
                self.search.scanned = 0;
                self.search.total = self.bytes.len();
                self.search.worker = Some(worker);
                self.status =
                    "Searching in background; browse normally, n/N moves through matches".into();
            }
            Err(error) => self.status = error,
        }
    }

    fn drain_search_messages(&mut self) {
        let Some(worker) = self.search.worker.take() else {
            return;
        };
        let mut keep_worker = true;
        while let Ok(message) = worker.receiver.try_recv() {
            match message {
                SearchMessage::Batch(batch) => {
                    let first_batch = self.search.results.is_empty();
                    self.search.results.extend(batch);
                    if first_batch {
                        self.status = format!(
                            "Search running: {} matches so far; press n for next",
                            self.search.results.len()
                        );
                    }
                }
                SearchMessage::Progress(scanned) => self.search.scanned = scanned,
                SearchMessage::Done => {
                    self.search.running = false;
                    keep_worker = false;
                    self.status = if self.search.results.is_empty() {
                        "Search complete: no matches".into()
                    } else {
                        format!(
                            "Search complete: {} matches; n/N or Ctrl+Down/Up navigates",
                            self.search.results.len()
                        )
                    };
                }
                SearchMessage::Error(error) => {
                    self.search.running = false;
                    keep_worker = false;
                    self.status = format!("Search failed: {error}");
                }
            }
        }
        if keep_worker {
            self.search.worker = Some(worker);
        }
    }

    fn cancel_search(&mut self) {
        if let Some(worker) = self.search.worker.take() {
            worker.cancel();
        }
        self.search.running = false;
    }

    fn next_search_result(&mut self) {
        if self.search.results.is_empty() {
            self.status = if self.search.running {
                "Search is still running; no matches found yet".into()
            } else {
                "No search results; use Ctrl+F".into()
            };
            return;
        }
        if self.search.has_active_result {
            self.search.current = (self.search.current + 1) % self.search.results.len();
        } else {
            self.search.current = 0;
            self.search.has_active_result = true;
        }
        self.activate_search_result();
    }

    fn previous_search_result(&mut self) {
        if self.search.results.is_empty() {
            self.status = if self.search.running {
                "Search is still running; no matches found yet".into()
            } else {
                "No search results; use Ctrl+F".into()
            };
            return;
        }
        if self.search.has_active_result {
            self.search.current = self
                .search
                .current
                .checked_sub(1)
                .unwrap_or(self.search.results.len() - 1);
        } else {
            self.search.current = self.search.results.len() - 1;
            self.search.has_active_result = true;
        }
        self.activate_search_result();
    }

    fn activate_search_result(&mut self) {
        if let Some(found) = self.active_search_match() {
            let (start, end) = (found.start, found.end);
            self.selection = Some(Selection {
                anchor: start,
                cursor: end,
            });
            self.ensure_visible(start);
            self.status = format!(
                "Match {}/{} at 0x{:X}{}",
                self.search.current + 1,
                self.search.results.len(),
                start,
                if self.search.running {
                    " (search still running)"
                } else {
                    ""
                }
            );
        }
    }

    fn open_path_dialog(&mut self, action: PathAction) {
        let suggested = match action {
            PathAction::SaveOverlay | PathAction::LoadOverlay => {
                suggested_path(&self.path, "rexedit-overlay.json")
            }
            PathAction::SaveBinary => self.path.clone(),
            PathAction::SaveTheme | PathAction::LoadTheme => suggested_named_path(&format!(
                "{}.rexedit-theme.json",
                safe_name(&self.theme.name)
            )),
        };
        self.mode = Mode::Path(PathDialog {
            action,
            input: TextInput {
                value: suggested.display().to_string(),
            },
        });
    }

    fn perform_path_action(&mut self, action: PathAction, path: PathBuf) {
        if path.as_os_str().is_empty() {
            self.status = "A file path is required".into();
            return;
        }
        let result = match action {
            PathAction::SaveOverlay => self.save_overlay_to(&path),
            PathAction::LoadOverlay => self.load_overlay_from(&path),
            PathAction::SaveBinary => self.save_binary_to(&path),
            PathAction::SaveTheme => self.save_theme_to(&path),
            PathAction::LoadTheme => self.load_theme_from(&path),
        };
        match result {
            Ok(message) => {
                self.status = message;
                self.mode = Mode::Normal;
            }
            Err(error) => self.status = error,
        }
    }

    fn save_overlay_to(&self, path: &Path) -> Result<String, String> {
        let json = serde_json::to_string_pretty(&Overlay {
            fields: self.fields.clone(),
        })
        .map_err(|error| format!("Could not serialize overlay: {error}"))?;
        fs::write(path, json)
            .map_err(|error| format!("Could not save overlay to {}: {error}", path.display()))?;
        Ok(format!("Saved overlay to {}", path.display()))
    }

    fn load_overlay_from(&mut self, path: &Path) -> Result<String, String> {
        let json = fs::read_to_string(path)
            .map_err(|error| format!("Could not read overlay {}: {error}", path.display()))?;
        let mut overlay: Overlay = serde_json::from_str(&json)
            .map_err(|error| format!("Invalid overlay JSON: {error}"))?;
        overlay
            .fields
            .retain(|field| field.start <= field.end && field.end < self.bytes.len());
        self.fields = overlay.fields;
        self.selected_field = 0;
        Ok(format!(
            "Loaded {} fields from {}",
            self.fields.len(),
            path.display()
        ))
    }

    fn save_binary_to(&mut self, path: &Path) -> Result<String, String> {
        self.commit_pending_edit();
        fs::write(path, self.bytes.as_slice())
            .map_err(|error| format!("Could not save binary to {}: {error}", path.display()))?;
        self.path = path.to_owned();
        self.saved_bytes = Arc::clone(&self.bytes);
        self.modified_offsets.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.quit_armed = false;
        Ok(format!("Saved binary to {}", path.display()))
    }

    fn save_theme_to(&self, path: &Path) -> Result<String, String> {
        let json = serde_json::to_string_pretty(&self.theme)
            .map_err(|error| format!("Could not serialize theme: {error}"))?;
        fs::write(path, json)
            .map_err(|error| format!("Could not save theme to {}: {error}", path.display()))?;
        Ok(format!("Saved theme to {}", path.display()))
    }

    fn load_theme_from(&mut self, path: &Path) -> Result<String, String> {
        let json = fs::read_to_string(path)
            .map_err(|error| format!("Could not read theme {}: {error}", path.display()))?;
        self.theme =
            serde_json::from_str(&json).map_err(|error| format!("Invalid theme JSON: {error}"))?;
        Ok(format!("Loaded theme '{}'", self.theme.name))
    }

    fn change_theme_value(&mut self, active: usize, forward: bool) {
        if active == 1 {
            self.theme.byte_mode = if forward {
                self.theme.byte_mode.next()
            } else {
                self.theme.byte_mode.previous()
            };
            return;
        }
        let color = match active {
            2 => &mut self.theme.hex_primary,
            3 => &mut self.theme.hex_secondary,
            4 => &mut self.theme.ascii,
            5 => &mut self.theme.offset,
            6 => &mut self.theme.border,
            7 => &mut self.theme.selection_background,
            8 => &mut self.theme.search_background,
            9 => &mut self.theme.modified,
            _ => return,
        };
        *color = if forward {
            color.next()
        } else {
            color.previous()
        };
    }
}

fn suggested_path(binary: &Path, suffix: &str) -> PathBuf {
    let file_name = binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("binary");
    suggested_named_path(&format!("{file_name}.{suffix}"))
}

fn suggested_named_path(file_name: &str) -> PathBuf {
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(file_name)
}

fn safe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "custom".into()
    } else {
        safe
    }
}

const WINDOWS_PICKER_SCRIPT: &str = "Add-Type -AssemblyName System.Windows.Forms; \
$dialog = New-Object System.Windows.Forms.OpenFileDialog; \
$dialog.Title = 'Open binary file'; \
$dialog.Filter = 'All files (*.*)|*.*'; \
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { \
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
Write-Output $dialog.FileName }";

#[cfg(windows)]
fn pick_binary_file() -> Result<Option<PathBuf>, String> {
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-STA", "-Command", WINDOWS_PICKER_SCRIPT])
        .creation_flags(0x0800_0000);
    run_picker_command(&mut command).map(|path| path.map(PathBuf::from))
}

#[cfg(target_os = "linux")]
fn pick_binary_file() -> Result<Option<PathBuf>, String> {
    if env::var_os("WSL_INTEROP").is_some() || env::var_os("WSL_DISTRO_NAME").is_some() {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-STA", "-Command", WINDOWS_PICKER_SCRIPT]);
        let Some(windows_path) = run_picker_command(&mut command)? else {
            return Ok(None);
        };
        let output = Command::new("wslpath")
            .args(["-u", &windows_path])
            .output()
            .map_err(|error| format!("Could not translate the selected Windows path: {error}"))?;
        if !output.status.success() {
            return Err("Could not translate the selected Windows path with wslpath".into());
        }
        return Ok(nonempty_output(&output.stdout).map(PathBuf::from));
    }

    let mut zenity = Command::new("zenity");
    zenity.args([
        "--file-selection",
        "--title=Open binary file",
        "--file-filter=All files | *",
    ]);
    match run_picker_command(&mut zenity) {
        Ok(path) => return Ok(path.map(PathBuf::from)),
        Err(error) if !error.contains("not found") => return Err(error),
        Err(_) => {}
    }

    let mut kdialog = Command::new("kdialog");
    kdialog.args(["--getopenfilename", ".", "All files (*)"]);
    run_picker_command(&mut kdialog)
        .map(|path| path.map(PathBuf::from))
        .map_err(|_| {
            "No graphical file picker was found. Install zenity or kdialog, then retry.".into()
        })
}

#[cfg(target_os = "macos")]
fn pick_binary_file() -> Result<Option<PathBuf>, String> {
    let mut command = Command::new("osascript");
    command.args([
        "-e",
        "POSIX path of (choose file with prompt \"Open binary file\")",
    ]);
    run_picker_command(&mut command).map(|path| path.map(PathBuf::from))
}

fn run_picker_command(command: &mut Command) -> Result<Option<String>, String> {
    let output = command
        .output()
        .map_err(|error| format!("File picker command not found or could not start: {error}"))?;
    if output.status.success() {
        return Ok(nonempty_output(&output.stdout));
    }
    let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if error.is_empty() {
        Ok(None)
    } else {
        Err(format!("File picker failed: {error}"))
    }
}

fn nonempty_output(output: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(output).trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

fn calculate_entropy(bytes: &[u8]) -> Vec<f64> {
    if bytes.is_empty() {
        return vec![0.0];
    }
    let target_buckets = 128usize;
    let window = bytes.len().div_ceil(target_buckets).max(256);
    bytes
        .chunks(window)
        .map(|chunk| {
            let mut counts = [0usize; 256];
            for byte in chunk {
                counts[usize::from(*byte)] += 1;
            }
            counts
                .into_iter()
                .filter(|count| *count > 0)
                .map(|count| {
                    let probability = count as f64 / chunk.len() as f64;
                    -probability * probability.log2()
                })
                .sum()
        })
        .collect()
}

pub fn parse_offset(input: &str) -> Result<usize, String> {
    let input = input.trim().replace('_', "");
    if input.is_empty() {
        return Err("offset is empty".into());
    }
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        usize::from_str_radix(hex, 16).map_err(|_| "invalid hexadecimal offset".into())
    } else {
        input
            .parse::<usize>()
            .map_err(|_| "use decimal or a 0x-prefixed hexadecimal offset".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_file(name: &str) -> PathBuf {
        env::temp_dir().join(format!("rexedit-{}-{name}", std::process::id()))
    }

    #[test]
    fn parses_decimal_and_hex_offsets() {
        assert_eq!(parse_offset("42").unwrap(), 42);
        assert_eq!(parse_offset("0x2A").unwrap(), 42);
    }

    #[test]
    fn locates_search_highlights_with_sorted_results() {
        let mut app = App::new("sample.bin".into(), vec![0; 32]);
        app.search.results = vec![
            SearchMatch { start: 2, end: 4 },
            SearchMatch { start: 10, end: 12 },
        ];
        assert!(app.is_search_match(3));
        assert!(app.is_search_match(12));
        assert!(!app.is_search_match(5));
    }

    #[test]
    fn suggested_overlay_path_uses_the_working_directory() {
        let path = suggested_path(Path::new("C:/protected/sample.bin"), "rexedit-overlay.json");
        assert_eq!(path.file_name().unwrap(), "sample.bin.rexedit-overlay.json");
    }

    #[test]
    fn overwrite_mode_changes_nibbles_and_tracks_dirty_bytes() {
        let mut app = App::new("sample.bin".into(), vec![0xAB, 0]);
        app.overwrite_nibble(0x1);
        app.overwrite_nibble(0x2);
        assert_eq!(app.bytes[0], 0x12);
        assert_eq!(app.selection.unwrap().cursor, 1);
        assert!(app.modified_offsets.contains(&0));
    }

    #[test]
    fn undo_and_redo_restore_complete_byte_overwrites() {
        let mut app = App::new("sample.bin".into(), vec![0xAB, 0]);
        app.overwrite_nibble(0x1);
        app.overwrite_nibble(0x2);

        app.undo_overwrite();
        assert_eq!(app.bytes[0], 0xAB);
        assert!(app.modified_offsets.is_empty());

        app.redo_overwrite();
        assert_eq!(app.bytes[0], 0x12);
        assert!(app.modified_offsets.contains(&0));
    }

    #[test]
    fn row_width_setting_changes_navigation_geometry() {
        let mut app = App::new("sample.bin".into(), vec![0; 64]);
        assert_eq!(app.row_count(), 4);
        app.toggle_setting(1);
        assert_eq!(app.settings.bytes_per_row, 32);
        assert_eq!(app.row_count(), 2);
    }

    #[test]
    fn entropy_distinguishes_uniform_and_varied_data() {
        let uniform = calculate_entropy(&vec![0; 1024]);
        let varied = calculate_entropy(&(0..=255).cycle().take(1024).collect::<Vec<_>>());
        assert_eq!(uniform[0], 0.0);
        assert!(varied[0] > 7.9);
    }

    #[test]
    fn workspace_cycles_between_documents() {
        let mut workspace = Workspace::new(vec![
            App::new("one.bin".into(), vec![1]),
            App::new("two.bin".into(), vec![2]),
        ]);
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .unwrap();
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(workspace.active, 1);
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .unwrap();
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(workspace.active, 0);
    }

    #[test]
    fn workspace_chord_toggles_comparison_and_disables_diff() {
        let mut workspace = Workspace::new(vec![
            App::new("one.bin".into(), vec![1]),
            App::new("two.bin".into(), vec![2]),
        ]);
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .unwrap();
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
            .unwrap();
        assert!(workspace.side_by_side);

        workspace.diff_mode = true;
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .unwrap();
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
            .unwrap();
        assert!(!workspace.side_by_side);
        assert!(!workspace.diff_mode);
    }

    #[test]
    fn current_bytes_clamps_invalid_selection_ranges() {
        let mut app = App::new("sample.bin".into(), vec![1, 2, 3]);
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 500,
        });
        assert_eq!(app.current_bytes(), &[2, 3]);
    }

    #[test]
    fn mouse_coordinates_outside_viewer_never_underflow() {
        let mut app = App::new("sample.bin".into(), vec![0; 256]);
        app.viewer_area = Rect::new(40, 10, 30, 12);
        app.fields_area = Rect::new(80, 10, 20, 12);

        for (column, row) in [(0, 0), (39, 10), (40, 9), (u16::MAX, 11), (41, u16::MAX)] {
            assert_eq!(app.byte_at(column, row), None);
            assert_eq!(app.field_at(column, row), None);
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            });
        }
    }

    #[test]
    fn mouse_in_separator_before_ascii_never_evaluates_negative_offset() {
        let mut app = App::new("sample.bin".into(), vec![0; 256]);
        app.viewer_area = Rect::new(0, 0, 100, 12);
        app.settings.show_offsets = true;
        app.settings.show_ascii = true;
        app.settings.bytes_per_row = 16;

        let prefix_width = 10usize;
        let hex_width = 16 * 3 + 15 / 8;
        let ascii_start = prefix_width + hex_width + 2;
        let separator_column = 1 + ascii_start - 1;

        assert_eq!(
            app.byte_at(separator_column as u16, 1),
            None,
            "separator immediately before ASCII must not subtract ascii_start"
        );
        app.mouse_dragging = true;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: separator_column as u16,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn question_mark_opens_and_scrolls_keybinding_help() {
        let mut app = App::new("sample.bin".into(), vec![0; 16]);
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(app.mode, Mode::Help(_)));

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
            .unwrap();
        let Mode::Help(help) = &app.mode else {
            panic!("help should remain open");
        };
        assert_eq!(help.scroll, 10);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn degenerate_viewer_rectangles_are_safe() {
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(u16::MAX, u16::MAX, 0, 0),
            Rect::new(u16::MAX, u16::MAX, 1, 1),
        ] {
            let mut app = App {
                viewer_area: area,
                fields_area: area,
                ..App::new("sample.bin".into(), vec![0; 16])
            };
            assert_eq!(app.byte_at(0, 0), None);
            assert_eq!(app.field_at(0, 0), None);
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
        }
    }

    #[test]
    fn tab_mouse_click_selects_a_document() {
        let mut workspace = Workspace::new(vec![
            App::new("one.bin".into(), vec![1]),
            App::new("two.bin".into(), vec![2]),
        ]);
        workspace.tab_row = 0;
        workspace.tab_hitboxes = vec![(10, 20), (20, 30)];
        workspace.handle_workspace_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 22,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(workspace.active, 1);
    }

    #[test]
    fn trims_file_picker_output() {
        assert_eq!(
            nonempty_output(b" C:\\temp\\sample.bin \r\n"),
            Some("C:\\temp\\sample.bin".into())
        );
        assert_eq!(nonempty_output(b" \n"), None);
    }

    #[test]
    fn first_next_command_activates_the_first_search_result() {
        let mut app = App::new("sample.bin".into(), vec![0; 32]);
        app.search.results = vec![
            SearchMatch { start: 4, end: 5 },
            SearchMatch { start: 12, end: 13 },
        ];
        app.next_search_result();
        assert_eq!(app.selection.unwrap().start(), 4);
        app.next_search_result();
        assert_eq!(app.selection.unwrap().start(), 12);
    }

    #[test]
    fn overlay_and_binary_paths_round_trip() {
        let overlay_path = temporary_file("overlay.json");
        let binary_path = temporary_file("saved.bin");
        let mut app = App::new("sample.bin".into(), vec![0xCA, 0xFE]);
        app.fields.push(Field {
            name: "magic".into(),
            description: "test".into(),
            start: 0,
            end: 1,
            color: FieldColor::Cyan,
        });

        app.save_overlay_to(&overlay_path).unwrap();
        app.fields.clear();
        app.load_overlay_from(&overlay_path).unwrap();
        app.save_binary_to(&binary_path).unwrap();

        assert_eq!(app.fields[0].name, "magic");
        assert_eq!(fs::read(&binary_path).unwrap(), vec![0xCA, 0xFE]);
        fs::remove_file(overlay_path).unwrap();
        fs::remove_file(binary_path).unwrap();
    }
}
