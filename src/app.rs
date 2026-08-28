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
use std::{io::Write, process::Stdio};

#[cfg(not(windows))]
use std::{io::Write, process::Stdio};

#[cfg(not(windows))]
use std::time::Instant;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(not(windows))]
use crossterm::{clipboard::CopyToClipboard, execute};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::{DefaultTerminal, layout::Rect};

use crate::{
    entropy::{self, EntropyMessage, EntropyWorker},
    model::{
        ByteColorMode, DEFAULT_BYTES_PER_ROW, Field, FieldColor, NamedColor, Overlay, SearchMatch,
        Selection, Theme,
    },
    python::{PythonDocument, PythonSession, PythonSnapshot},
    search::{self, SearchMessage, SearchWorker},
    ui,
};

const PYTHON_CONSOLE_LINE_WIDTH: usize = 72;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Viewer,
    Fields,
    Python,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollbarDrag {
    Viewer,
    Fields,
    Python,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathAction {
    SaveOverlay,
    LoadOverlay,
    SaveBinary,
    SaveTheme,
    LoadTheme,
}

pub const PATH_SUGGESTION_PAGE_SIZE: usize = 12;

#[derive(Debug)]
pub struct PathDialog {
    pub action: PathAction,
    pub input: TextInput,
    pub suggestions: Vec<PathBuf>,
    pub active_suggestion: Option<usize>,
    pub suggestion_scroll: usize,
}

#[derive(Debug)]
pub enum OpenFileDialog {
    Choice {
        active: usize,
    },
    ManualPath {
        input: TextInput,
        suggestions: Vec<PathBuf>,
        active_suggestion: Option<usize>,
        suggestion_scroll: usize,
    },
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
    ConfirmReset(ResetTarget),
    Python(PythonPane),
    Help(HelpViewer),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetTarget {
    Theme,
    Settings,
}

#[derive(Debug, Default)]
pub struct TextInput {
    pub value: String,
    cursor: usize,
    pub selected: bool,
}

impl TextInput {
    fn with_value(value: String) -> Self {
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            selected: true,
        }
    }

    fn clear_selection(&mut self) {
        self.selected = false;
    }

    fn set_value(&mut self, value: String) {
        self.cursor = value.chars().count();
        self.value = value;
        self.selected = false;
    }

    fn take_value(&mut self) -> String {
        self.cursor = 0;
        self.selected = false;
        std::mem::take(&mut self.value)
    }

    fn replace_selection(&mut self) {
        if self.selected {
            self.value.clear();
            self.cursor = 0;
            self.selected = false;
        }
    }

    fn byte_index(&self, character_index: usize) -> usize {
        self.value
            .char_indices()
            .nth(character_index)
            .map_or(self.value.len(), |(index, _)| index)
    }

    pub fn cursor_byte_index(&self) -> usize {
        self.byte_index(self.cursor)
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
                self.selected = false;
            }
            // Some terminals send Ctrl+H instead of Backspace.
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.backspace();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.replace_selection();
                let index = self.byte_index(self.cursor);
                self.value.insert(index, character);
                self.cursor += 1;
            }
            KeyCode::Backspace => {
                self.backspace();
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                self.selected = false;
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.value.chars().count());
                self.selected = false;
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.selected = false;
            }
            KeyCode::End => {
                self.cursor = self.value.chars().count();
                self.selected = false;
            }
            _ => {}
        }
    }

    fn backspace(&mut self) {
        if self.selected {
            self.replace_selection();
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }
}

#[derive(Debug)]
pub struct FieldEditor {
    pub editing: Option<usize>,
    pub name: TextInput,
    pub description: TextInput,
    pub start: TextInput,
    pub end: TextInput,
    pub color: FieldColor,
    pub active: usize,
    ranges: Vec<Selection>,
}

impl FieldEditor {
    fn new() -> Self {
        Self {
            editing: None,
            name: TextInput::default(),
            description: TextInput::default(),
            start: TextInput::default(),
            end: TextInput::default(),
            color: FieldColor::default(),
            active: 0,
            ranges: Vec::new(),
        }
    }

    fn from_field(index: usize, field: &Field) -> Self {
        Self {
            editing: Some(index),
            name: TextInput::with_value(field.name.clone()),
            description: TextInput::with_value(field.description.clone()),
            start: TextInput::with_value(format!("0x{:X}", field.start)),
            end: TextInput::with_value(format!("0x{:X}", field.end)),
            color: field.color,
            active: 0,
            ranges: Vec::new(),
        }
    }

    fn active_text_mut(&mut self) -> Option<&mut TextInput> {
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
        input.handle_key(key);
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

#[derive(Debug)]
pub struct PythonPane {
    pub input: TextInput,
    pub repl_lines: Vec<String>,
    pub output: Vec<String>,
    pub session: PythonSession,
    pub pending: usize,
    pub scroll: usize,
    pub visible_output_lines: usize,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub history_draft: String,
}

impl PythonPane {
    pub fn max_scroll(&self) -> usize {
        self.output
            .len()
            .saturating_add(self.repl_lines.len())
            .saturating_add(1)
            .saturating_sub(self.visible_output_lines)
    }

    pub(crate) fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(self.max_scroll());
    }
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
    pub compress_repeated_rows: bool,
    pub show_overlays: bool,
}

impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            show_ascii: true,
            bytes_per_row: DEFAULT_BYTES_PER_ROW,
            uppercase_hex: true,
            show_offsets: true,
            show_sidebar: true,
            compress_repeated_rows: false,
            show_overlays: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRow {
    Bytes {
        offset: usize,
    },
    Repeated {
        start: usize,
        end: usize,
        byte: u8,
        physical_rows: usize,
    },
}

impl DisplayRow {
    pub fn start(self) -> usize {
        match self {
            Self::Bytes { offset } => offset,
            Self::Repeated { start, .. } => start,
        }
    }

    pub fn end(self, bytes_per_row: usize, byte_len: usize) -> usize {
        match self {
            Self::Bytes { offset } => offset
                .saturating_add(bytes_per_row)
                .min(byte_len)
                .saturating_sub(1),
            Self::Repeated { end, .. } => end,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditKind {
    #[default]
    Overwrite,
    Insert,
}

impl EditKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Overwrite => "Overwrite",
            Self::Insert => "Insert",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::Overwrite => Self::Insert,
            Self::Insert => Self::Overwrite,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EditAction {
    Overwrite {
        offset: usize,
        before: u8,
        after: u8,
    },
    OverwriteMany {
        offset: usize,
        before: Vec<u8>,
        after: Vec<u8>,
    },
    Insert {
        offset: usize,
        byte: u8,
    },
    InsertMany {
        offset: usize,
        bytes: Vec<u8>,
    },
    Delete {
        offset: usize,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingEdit {
    Overwrite { offset: usize, before: u8 },
    Insert { offset: usize, high_nibble: u8 },
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
    pub additional_selections: Vec<Selection>,
    pub fields: Vec<Field>,
    pub selected_field: usize,
    pub fields_scroll: usize,
    pub visible_fields: usize,
    pub focus: Focus,
    pub search: SearchState,
    pub mode: Mode,
    pub status: String,
    pub viewer_area: Rect,
    pub fields_area: Rect,
    pub python_area: Rect,
    pub theme: Theme,
    pub settings: ViewerSettings,
    pub display_rows: Vec<DisplayRow>,
    python_history: Vec<String>,
    pub edit_mode: bool,
    pub edit_kind: EditKind,
    pub edit_high_nibble: bool,
    insert_at_end: bool,
    pub modified_offsets: BTreeSet<usize>,
    undo_stack: Vec<EditAction>,
    redo_stack: Vec<EditAction>,
    pending_edit: Option<PendingEdit>,
    vim_g_pending: bool,
    mouse_dragging: bool,
    scrollbar_dragging: Option<ScrollbarDrag>,
    quit_armed: bool,
    pub entropy: Option<Vec<f64>>,
    entropy_worker: Option<EntropyWorker>,
    pub entropy_scanned: usize,
    pub entropy_total: usize,
}

impl App {
    pub fn new(path: PathBuf, bytes: Vec<u8>) -> Self {
        let selection = (!bytes.is_empty()).then(|| Selection::new(0));
        let mut app = Self {
            path,
            saved_bytes: Arc::new(bytes.clone()),
            bytes: Arc::new(bytes),
            scroll: 0,
            visible_rows: 1,
            selection,
            additional_selections: Vec::new(),
            fields: Vec::new(),
            selected_field: 0,
            fields_scroll: 0,
            visible_fields: 1,
            focus: Focus::Viewer,
            search: SearchState::default(),
            mode: Mode::Normal,
            status: "Ready".into(),
            viewer_area: Rect::default(),
            fields_area: Rect::default(),
            python_area: Rect::default(),
            theme: Theme::default(),
            settings: ViewerSettings::default(),
            display_rows: Vec::new(),
            python_history: Vec::new(),
            edit_mode: false,
            edit_kind: EditKind::Overwrite,
            edit_high_nibble: true,
            insert_at_end: false,
            modified_offsets: BTreeSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending_edit: None,
            vim_g_pending: false,
            mouse_dragging: false,
            scrollbar_dragging: None,
            quit_armed: false,
            entropy: None,
            entropy_worker: None,
            entropy_scanned: 0,
            entropy_total: 0,
        };
        if app.path.is_file() {
            match app.restore_automatic_overlay() {
                Ok(true) => {
                    app.status = format!("Restored {} saved overlay field(s)", app.fields.len());
                }
                Ok(false) => {}
                Err(error) => app.status = format!("Could not restore saved overlay: {error}"),
            }
        }
        app.rebuild_display_rows();
        app
    }

    #[cfg(test)]
    pub fn entropy_profile(&mut self) -> &[f64] {
        self.entropy
            .get_or_insert_with(|| entropy::calculate(&self.bytes))
    }

    pub fn request_entropy(&mut self) {
        if self.entropy.is_some() || self.entropy_worker.is_some() {
            return;
        }
        self.entropy_total = self.bytes.len();
        self.entropy_scanned = 0;
        self.entropy_worker = Some(entropy::spawn(Arc::clone(&self.bytes)));
        self.status = "Calculating entropy in the background…".into();
    }

    pub fn entropy_running(&self) -> bool {
        self.entropy_worker.is_some()
    }

    fn drain_entropy_messages(&mut self) {
        let Some(worker) = self.entropy_worker.take() else {
            return;
        };
        let mut keep_worker = true;
        while let Ok(message) = worker.receiver.try_recv() {
            match message {
                EntropyMessage::Progress(scanned) => {
                    self.entropy_scanned = scanned.min(self.entropy_total);
                }
                EntropyMessage::Done(profile) => {
                    self.entropy = Some(profile);
                    self.entropy_scanned = self.entropy_total;
                    self.status = "Entropy calculation complete".into();
                    keep_worker = false;
                }
            }
        }
        if keep_worker {
            self.entropy_worker = Some(worker);
        }
    }

    fn invalidate_entropy(&mut self) {
        if let Some(worker) = self.entropy_worker.take() {
            worker.cancel();
        }
        self.entropy = None;
        self.entropy_scanned = 0;
        self.entropy_total = 0;
    }

    fn automatic_overlay_path(&self) -> PathBuf {
        overlay_storage_dir().join(format!("{}.json", content_identity(&self.saved_bytes)))
    }

    fn restore_automatic_overlay(&mut self) -> Result<bool, String> {
        let path = self.automatic_overlay_path();
        if !path.is_file() {
            return Ok(false);
        }
        self.load_overlay_from(&path)?;
        Ok(true)
    }

    fn persist_automatic_overlay(&self) -> Result<Option<PathBuf>, String> {
        if !self.path.is_file() {
            return Ok(None);
        }
        let path = self.automatic_overlay_path();
        if self.fields.is_empty() && !path.is_file() {
            return Ok(None);
        }
        let parent = path
            .parent()
            .ok_or_else(|| "Could not determine the overlay storage directory".to_string())?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Could not create overlay storage directory {}: {error}",
                parent.display()
            )
        })?;
        self.save_overlay_to(&path)?;
        Ok(Some(path))
    }

    fn save_automatic_overlay_after_change(&mut self) {
        match self.persist_automatic_overlay() {
            Ok(Some(path)) => {
                self.status = format!("{}; overlay saved to {}", self.status, path.display());
            }
            Ok(None) => {}
            Err(error) => self.status = format!("{}; {error}", self.status),
        }
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
    pub open_file_dialog: Option<OpenFileDialog>,
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
            open_file_dialog: None,
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
            let mut snapshots = Vec::new();
            for document in &mut self.documents {
                document.drain_search_messages();
                document.drain_entropy_messages();
                snapshots.extend(document.drain_python_messages());
            }
            self.update_entropy_status();
            for snapshot in snapshots {
                if let Some(document) = self.documents.get_mut(snapshot.index) {
                    document.apply_python_snapshot(&snapshot);
                }
            }
            terminal.draw(|frame| ui::render_workspace(frame, self))?;
            if !event::poll(Duration::from_millis(40))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('z')
                    {
                        self.suspend(terminal)?;
                        continue;
                    }
                    if self.handle_workspace_key(key)? {
                        self.persist_all_automatic_overlays();
                        for document in &mut self.documents {
                            document.cancel_search();
                        }
                        return Ok(());
                    }
                }
                Event::Mouse(mouse) => self.handle_workspace_mouse(mouse),
                Event::Paste(text) if self.handle_workspace_paste(&text)? => {
                    self.persist_all_automatic_overlays();
                    for document in &mut self.documents {
                        document.cancel_search();
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    /// Bracketed paste delivers the whole clipboard as one event. In Byte Edit
    /// Mode we decode it and apply it as a single batched edit rather than
    /// simulating a keystroke per character, since that path is quadratic for
    /// large pastes. Everywhere else (search boxes, field names, path
    /// dialogs, …) we fall back to feeding each character through the normal
    /// key dispatch, which is the same effective behavior a terminal without
    /// bracketed paste support already produces for those small inputs.
    fn handle_workspace_paste(&mut self, text: &str) -> io::Result<bool> {
        if !self.documents.is_empty() && self.open_file_dialog.is_none() {
            let active = self.active();
            let byte_edit_active = active.edit_mode
                && active.focus == Focus::Viewer
                && matches!(active.mode, Mode::Normal | Mode::Python(_));
            if byte_edit_active {
                self.active_mut().paste_hex_bytes(text);
                return Ok(false);
            }
        }
        for character in text.chars() {
            if character == '\n' || character == '\r' {
                continue;
            }
            let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            if self.handle_workspace_key(key)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn handle_workspace_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if self.open_file_dialog.is_some() {
            self.handle_open_file_dialog_key(key);
            return Ok(false);
        }
        if self.documents.is_empty() {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('n') {
                self.open_file_dialog = Some(OpenFileDialog::Choice { active: 0 });
                return Ok(false);
            }
            if key.code == KeyCode::Char('q') {
                return Ok(true);
            }
            self.status = "No binary open — Ctrl+N opens a file; q quits".into();
            return Ok(false);
        }
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
                    self.open_file_dialog = Some(OpenFileDialog::Choice { active: 0 });
                    return Ok(false);
                }
                KeyCode::Char('w' | 'W')
                    if !self.active().edit_mode && matches!(self.active().mode, Mode::Normal) =>
                {
                    self.close_active_document();
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
                self.request_entropy_for_visible_documents();
                self.status = "Calculating entropy in the background…".into();
            } else {
                self.status = "Entropy panel hidden".into();
            }
            return Ok(false);
        }
        if key.code == KeyCode::Esc
            && self.show_entropy
            && matches!(self.active().mode, Mode::Normal)
        {
            self.show_entropy = false;
            self.status = "Entropy panel hidden".into();
            return Ok(false);
        }
        if key.code == KeyCode::Char('p')
            && !self.active().edit_mode
            && matches!(self.active().mode, Mode::Normal)
        {
            self.open_python_pane();
            return Ok(false);
        }
        self.active_mut().handle_key(key)
    }

    fn open_python_pane(&mut self) {
        let active = self.active;
        if self.documents[active].selection.is_none() {
            self.status = "Select at least one byte before opening Python".into();
            return;
        }
        let documents = self
            .documents
            .iter()
            .enumerate()
            .map(|(index, document)| {
                let selection = document.selection.unwrap_or_else(|| Selection::new(0));
                let selections = document
                    .selected_ranges()
                    .into_iter()
                    .map(|range| (range.start(), range.end()))
                    .collect();
                PythonDocument {
                    index,
                    bytes: document.bytes.as_ref().clone(),
                    selection_start: selection
                        .start()
                        .min(document.bytes.len().saturating_sub(1)),
                    selection_end: selection.end().min(document.bytes.len().saturating_sub(1)),
                    selections,
                }
            })
            .collect();
        match PythonSession::start(documents, active) {
            Ok(session) => self.documents[active].open_python_pane_with_session(session),
            Err(error) => self.status = error,
        }
    }

    fn request_entropy_for_visible_documents(&mut self) {
        if self.side_by_side {
            for document in &mut self.documents {
                document.request_entropy();
            }
        } else {
            self.active_mut().request_entropy();
        }
    }

    fn update_entropy_status(&mut self) {
        if !self.show_entropy {
            return;
        }
        let running = self
            .documents
            .iter()
            .filter(|document| document.entropy_running())
            .collect::<Vec<_>>();
        if running.is_empty() {
            if self
                .documents
                .iter()
                .any(|document| document.entropy.is_some())
            {
                self.status = if self.side_by_side && self.diff_mode {
                    "Entropy profiles ready; showing absolute entropy differences".into()
                } else {
                    "Entropy calculation complete".into()
                };
            }
            return;
        }
        let scanned = running
            .iter()
            .map(|document| document.entropy_scanned)
            .sum::<usize>();
        let total = running
            .iter()
            .map(|document| document.entropy_total)
            .sum::<usize>();
        let percent = scanned
            .saturating_mul(100)
            .checked_div(total)
            .unwrap_or(100);
        self.status = format!(
            "Calculating entropy for {}/{} binary files: {percent}%",
            running.len(),
            self.documents.len()
        );
    }

    pub(crate) fn handle_workspace_mouse(&mut self, mouse: MouseEvent) {
        if let Some(OpenFileDialog::ManualPath {
            input,
            suggestions,
            suggestion_scroll,
            ..
        }) = &mut self.open_file_dialog
        {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => input.clear_selection(),
                MouseEventKind::ScrollUp => {
                    *suggestion_scroll = suggestion_scroll.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    let max_scroll = suggestions.len().saturating_sub(PATH_SUGGESTION_PAGE_SIZE);
                    *suggestion_scroll = suggestion_scroll.saturating_add(3).min(max_scroll);
                }
                _ => {}
            }
            return;
        }
        if self.open_file_dialog.is_some() {
            return;
        }
        if self.documents.is_empty() {
            return;
        }
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
            self.request_entropy_for_visible_documents();
        }
        self.status = format!("Active binary: {}", self.active().path.display());
    }

    fn handle_open_file_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = &mut self.open_file_dialog else {
            return;
        };
        match dialog {
            OpenFileDialog::Choice { active } => match key.code {
                KeyCode::Esc => self.open_file_dialog = None,
                KeyCode::Tab | KeyCode::Up | KeyCode::Down => *active = (*active + 1) % 2,
                KeyCode::Enter if *active == 0 => self.open_binary_picker(),
                KeyCode::Enter => {
                    self.open_file_dialog = Some(OpenFileDialog::ManualPath {
                        input: TextInput::default(),
                        suggestions: Vec::new(),
                        active_suggestion: None,
                        suggestion_scroll: 0,
                    });
                }
                _ => {}
            },
            OpenFileDialog::ManualPath {
                input,
                suggestions,
                active_suggestion,
                suggestion_scroll,
            } => match key.code {
                KeyCode::Esc => self.open_file_dialog = None,
                KeyCode::Tab => Self::complete_manual_path(
                    input,
                    suggestions,
                    active_suggestion,
                    suggestion_scroll,
                ),
                KeyCode::Down if !suggestions.is_empty() => Self::move_suggestion(
                    input,
                    suggestions,
                    active_suggestion,
                    suggestion_scroll,
                    1,
                ),
                KeyCode::Up if !suggestions.is_empty() => Self::move_suggestion(
                    input,
                    suggestions,
                    active_suggestion,
                    suggestion_scroll,
                    -1,
                ),
                KeyCode::PageDown if !suggestions.is_empty() => Self::move_suggestion(
                    input,
                    suggestions,
                    active_suggestion,
                    suggestion_scroll,
                    PATH_SUGGESTION_PAGE_SIZE as isize,
                ),
                KeyCode::PageUp if !suggestions.is_empty() => Self::move_suggestion(
                    input,
                    suggestions,
                    active_suggestion,
                    suggestion_scroll,
                    -(PATH_SUGGESTION_PAGE_SIZE as isize),
                ),
                KeyCode::Home if !suggestions.is_empty() => Self::select_suggestion(
                    input,
                    suggestions,
                    active_suggestion,
                    suggestion_scroll,
                    0,
                ),
                KeyCode::End if !suggestions.is_empty() => Self::select_suggestion(
                    input,
                    suggestions,
                    active_suggestion,
                    suggestion_scroll,
                    suggestions.len() - 1,
                ),
                KeyCode::Enter => {
                    if let Some(index) = *active_suggestion
                        && suggestions[index].is_dir()
                    {
                        input.set_value(completion_display_path(&suggestions[index]));
                        suggestions.clear();
                        *active_suggestion = None;
                        *suggestion_scroll = 0;
                        return;
                    }
                    let path = PathBuf::from(input.value.trim());
                    if path.as_os_str().is_empty() {
                        self.status = "Enter a full or relative file path".into();
                    } else if let Err(error) = self.open_binary_path(path) {
                        self.status = error;
                    } else {
                        self.open_file_dialog = None;
                    }
                }
                _ => {
                    input.handle_key(key);
                    suggestions.clear();
                    *active_suggestion = None;
                    *suggestion_scroll = 0;
                }
            },
        }
    }

    fn open_binary_picker(&mut self) {
        match pick_binary_file() {
            Ok(Some(path)) => match self.open_binary_path(path) {
                Ok(()) => self.open_file_dialog = None,
                Err(error) => self.status = error,
            },
            Ok(None) => self.status = "Open cancelled".into(),
            Err(error) => {
                self.status = format!("{error} Type a path manually instead.");
                self.open_file_dialog = Some(OpenFileDialog::ManualPath {
                    input: TextInput::default(),
                    suggestions: Vec::new(),
                    active_suggestion: None,
                    suggestion_scroll: 0,
                });
            }
        }
    }

    fn complete_manual_path(
        input: &mut TextInput,
        suggestions: &mut Vec<PathBuf>,
        active_suggestion: &mut Option<usize>,
        suggestion_scroll: &mut usize,
    ) {
        if !suggestions.is_empty() {
            let next = active_suggestion
                .map(|index| (index + 1) % suggestions.len())
                .unwrap_or(0);
            Self::select_suggestion(
                input,
                suggestions,
                active_suggestion,
                suggestion_scroll,
                next,
            );
            return;
        }
        let candidates = path_completion_candidates(&input.value);
        if candidates.is_empty() {
            *active_suggestion = None;
            *suggestion_scroll = 0;
            return;
        }
        if candidates.len() == 1 {
            let path = completion_display_path(&candidates[0]);
            input.set_value(path);
            suggestions.clear();
            *active_suggestion = None;
            *suggestion_scroll = 0;
            return;
        }
        *suggestions = candidates;
        *active_suggestion = None;
        *suggestion_scroll = 0;
    }

    fn move_suggestion(
        input: &mut TextInput,
        suggestions: &[PathBuf],
        active_suggestion: &mut Option<usize>,
        suggestion_scroll: &mut usize,
        delta: isize,
    ) {
        let current = active_suggestion.unwrap_or(if delta.is_negative() {
            suggestions.len() - 1
        } else {
            0
        });
        let next = current
            .saturating_add_signed(delta)
            .min(suggestions.len() - 1);
        Self::select_suggestion(
            input,
            suggestions,
            active_suggestion,
            suggestion_scroll,
            next,
        );
    }

    fn select_suggestion(
        input: &mut TextInput,
        suggestions: &[PathBuf],
        active_suggestion: &mut Option<usize>,
        suggestion_scroll: &mut usize,
        index: usize,
    ) {
        *active_suggestion = Some(index);
        input.set_value(completion_display_path(&suggestions[index]));
        if index < *suggestion_scroll {
            *suggestion_scroll = index;
        } else if index >= suggestion_scroll.saturating_add(PATH_SUGGESTION_PAGE_SIZE) {
            *suggestion_scroll = index + 1 - PATH_SUGGESTION_PAGE_SIZE;
        }
    }

    fn close_active_document(&mut self) {
        if self.documents.is_empty() {
            return;
        }
        if self
            .documents
            .iter()
            .any(|document| matches!(document.mode, Mode::Python(_)))
        {
            self.status = "Close the Python console before closing binaries".into();
            return;
        }
        let app = self.active();
        if !app.modified_offsets.is_empty() && !app.quit_armed {
            self.active_mut().quit_armed = true;
            self.status = "Unsaved byte changes: press Ctrl+W again to close without saving".into();
            return;
        }
        self.active_mut().invalidate_entropy();
        if let Err(error) = self.active().persist_automatic_overlay() {
            self.status = error;
            return;
        }
        let path = self.documents.remove(self.active).path;
        if self.documents.is_empty() {
            self.active = 0;
            self.side_by_side = false;
            self.diff_mode = false;
            self.comparison_panes.clear();
            self.status = format!("Closed {}; Ctrl+N opens a file", path.display());
            return;
        }
        self.active = self.active.min(self.documents.len() - 1);
        if self.documents.len() < 2 {
            self.side_by_side = false;
            self.diff_mode = false;
            self.comparison_panes.clear();
        }
        self.status = format!("Closed {}", path.display());
    }

    fn persist_all_automatic_overlays(&mut self) {
        for document in &self.documents {
            if let Err(error) = document.persist_automatic_overlay() {
                self.status = error;
                break;
            }
        }
    }

    fn open_binary_path(&mut self, path: PathBuf) -> Result<(), String> {
        let bytes = fs::read(&path)
            .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
        self.documents.push(App::new(path.clone(), bytes));
        self.active = self.documents.len() - 1;
        if self.show_entropy {
            self.request_entropy_for_visible_documents();
        }
        self.status = format!("Opened {}", path.display());
        Ok(())
    }

    #[cfg(unix)]
    fn suspend(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        execute!(io::stdout(), crossterm::event::DisableMouseCapture)?;
        ratatui::restore();
        // SAFETY: SIGTSTP is raised for the current process after terminal state is restored.
        unsafe {
            libc::raise(libc::SIGTSTP);
        }
        *terminal = ratatui::init();
        execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
        self.status = "Resumed from shell job control".into();
        Ok(())
    }

    #[cfg(not(unix))]
    fn suspend(&mut self, _terminal: &mut DefaultTerminal) -> io::Result<()> {
        self.status =
            "Ctrl+Z suspension requires Unix job control; it is unavailable on Windows".into();
        Ok(())
    }
}

impl App {
    pub fn row_count(&self) -> usize {
        self.display_rows.len()
    }

    pub fn max_scroll(&self) -> usize {
        self.row_count().saturating_sub(self.visible_rows)
    }

    pub fn field_max_scroll(&self) -> usize {
        self.fields.len().saturating_sub(self.visible_fields)
    }

    pub fn selected_ranges(&self) -> Vec<Selection> {
        let mut ranges = self.additional_selections.clone();
        if let Some(selection) = self.selection {
            ranges.push(selection);
        }
        ranges.sort_unstable_by_key(|range| range.start());
        let mut merged = Vec::<Selection>::new();
        for range in ranges {
            if let Some(previous) = merged.last_mut()
                && range.start() <= previous.end().saturating_add(1)
            {
                previous.cursor = previous.end().max(range.end());
            } else {
                merged.push(Selection {
                    anchor: range.start(),
                    cursor: range.end(),
                });
            }
        }
        merged
    }

    pub fn selected_bytes(&self) -> Vec<u8> {
        let Some(last) = self.bytes.len().checked_sub(1) else {
            return Vec::new();
        };
        self.selected_ranges()
            .into_iter()
            .flat_map(|range| {
                let start = range.start().min(last);
                let end = range.end().min(last);
                self.bytes[start..=end].iter().copied()
            })
            .collect()
    }

    pub fn is_selected(&self, offset: usize) -> bool {
        self.selection
            .is_some_and(|selection| selection.contains(offset))
            || self
                .additional_selections
                .iter()
                .any(|selection| selection.contains(offset))
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
        let row = self.display_row_for_offset(offset);
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
            ByteColorMode::LowNibble => {
                if byte & 0x0F < 8 {
                    self.theme.hex_primary
                } else {
                    self.theme.hex_secondary
                }
            }
            ByteColorMode::ZeroBytes => {
                if byte == 0 {
                    self.theme.hex_secondary
                } else {
                    self.theme.hex_primary
                }
            }
            ByteColorMode::Printable => {
                if byte.is_ascii_graphic() || byte == b' ' {
                    self.theme.hex_secondary
                } else {
                    self.theme.hex_primary
                }
            }
            ByteColorMode::ValueBands => match byte >> 6 {
                0 => self.theme.offset,
                1 => self.theme.hex_secondary,
                2 => self.theme.hex_primary,
                _ => self.theme.modified,
            },
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
            Mode::ConfirmReset(_) => {
                self.handle_reset_confirmation_key(key);
                Ok(false)
            }
            Mode::Python(_) => {
                self.handle_python_key(key);
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
        if self.vim_g_pending {
            self.vim_g_pending = false;
            if key.code == KeyCode::Char('g') {
                self.select_offset(0, false);
                return Ok(false);
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') => {
                    self.mode = Mode::Search(TextInput::with_value(self.search.query.clone()));
                }
                KeyCode::Char('g') => self.mode = Mode::Jump(TextInput::default()),
                KeyCode::Char('o') => self.open_path_dialog(PathAction::SaveOverlay),
                KeyCode::Char('l') => self.open_path_dialog(PathAction::LoadOverlay),
                KeyCode::Char('u') => self.undo_overwrite(),
                KeyCode::Char('r') => self.redo_overwrite(),
                KeyCode::Char('s') => self.open_path_dialog(PathAction::SaveBinary),
                KeyCode::Char('c' | 'C') => self.copy_selection_as_hex(),
                KeyCode::Char('v' | 'V') => self.paste_from_clipboard(),
                KeyCode::Up => self.previous_search_result(),
                KeyCode::Down => self.next_search_result(),
                _ => {}
            }
            return Ok(false);
        }
        #[cfg(target_os = "macos")]
        if key.modifiers.contains(KeyModifiers::SUPER) {
            match key.code {
                KeyCode::Char('c' | 'C') => {
                    self.copy_selection_as_hex();
                    return Ok(false);
                }
                KeyCode::Char('v' | 'V') => {
                    self.paste_from_clipboard();
                    return Ok(false);
                }
                _ => {}
            }
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
                self.edit_kind = EditKind::Overwrite;
                self.edit_high_nibble = true;
                self.insert_at_end = false;
                self.status =
                    "Overwrite Mode: type two hex digits per byte; Esc returns to View Mode".into();
            }
            KeyCode::Char('t') => self.mode = Mode::Theme(ThemeEditor::default()),
            KeyCode::Char('s') => self.mode = Mode::Settings(SettingsEditor::default()),
            KeyCode::Char('o') => {
                self.settings.show_overlays = !self.settings.show_overlays;
                self.status = if self.settings.show_overlays {
                    "Field overlays enabled"
                } else {
                    "Field overlays hidden"
                }
                .into();
            }
            KeyCode::Char('n') => self.next_search_result(),
            KeyCode::Char('N') => self.previous_search_result(),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Viewer => Focus::Fields,
                    Focus::Fields | Focus::Python => Focus::Viewer,
                };
            }
            KeyCode::Char('a') if self.selection.is_some() => {
                let mut editor = FieldEditor::new();
                editor.ranges = self.selected_ranges();
                self.mode = Mode::Field(editor);
            }
            KeyCode::Char('g') => self.vim_g_pending = true,
            KeyCode::Char('G') if !self.bytes.is_empty() => {
                self.select_offset(self.bytes.len() - 1, false);
            }
            KeyCode::Enter if self.focus == Focus::Fields => self.edit_selected_field(),
            KeyCode::Char('d') | KeyCode::Delete if self.focus == Focus::Fields => {
                self.delete_selected_field()
            }
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
                KeyCode::Char('c' | 'C') => self.copy_selection_as_hex(),
                KeyCode::Char('v' | 'V') => self.paste_from_clipboard(),
                _ => {}
            }
            return Ok(false);
        }
        #[cfg(target_os = "macos")]
        if key.modifiers.contains(KeyModifiers::SUPER) {
            match key.code {
                KeyCode::Char('c' | 'C') => {
                    self.copy_selection_as_hex();
                    return Ok(false);
                }
                KeyCode::Char('v' | 'V') => {
                    self.paste_from_clipboard();
                    return Ok(false);
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                self.commit_pending_edit();
                self.edit_mode = false;
                self.edit_kind = EditKind::Overwrite;
                self.edit_high_nibble = true;
                self.insert_at_end = false;
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
            KeyCode::Insert | KeyCode::Char('i') => self.toggle_edit_kind(),
            KeyCode::Backspace | KeyCode::Delete => self.delete_selected_bytes(),
            KeyCode::Char(character) if character.is_ascii_hexdigit() => {
                let nibble = character.to_digit(16).expect("checked hex digit") as u8;
                self.edit_nibble(nibble);
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
                        self.additional_selections.clear();
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
                    if let Some(input) = editor.active_text_mut() {
                        input.selected = !input.value.is_empty();
                    }
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Mode::Field(editor) = &mut self.mode {
                    editor.active = editor.active.checked_sub(1).unwrap_or(4);
                    if let Some(input) = editor.active_text_mut() {
                        input.selected = !input.value.is_empty();
                    }
                }
            }
            KeyCode::Left => {
                if let Mode::Field(editor) = &mut self.mode
                    && editor.active == 4
                {
                    editor.color = editor.color.previous();
                } else if let Mode::Field(editor) = &mut self.mode {
                    editor.handle_text_key(key);
                }
            }
            KeyCode::Right => {
                if let Mode::Field(editor) = &mut self.mode
                    && editor.active == 4
                {
                    editor.color = editor.color.next();
                } else if let Mode::Field(editor) = &mut self.mode {
                    editor.handle_text_key(key);
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
            KeyCode::Tab => {
                if let Mode::Path(dialog) = &mut self.mode {
                    Workspace::complete_manual_path(
                        &mut dialog.input,
                        &mut dialog.suggestions,
                        &mut dialog.active_suggestion,
                        &mut dialog.suggestion_scroll,
                    );
                }
            }
            KeyCode::Down if matches!(&self.mode, Mode::Path(dialog) if !dialog.suggestions.is_empty()) => {
                if let Mode::Path(dialog) = &mut self.mode {
                    Workspace::move_suggestion(
                        &mut dialog.input,
                        &dialog.suggestions,
                        &mut dialog.active_suggestion,
                        &mut dialog.suggestion_scroll,
                        1,
                    );
                }
            }
            KeyCode::Up if matches!(&self.mode, Mode::Path(dialog) if !dialog.suggestions.is_empty()) => {
                if let Mode::Path(dialog) = &mut self.mode {
                    Workspace::move_suggestion(
                        &mut dialog.input,
                        &dialog.suggestions,
                        &mut dialog.active_suggestion,
                        &mut dialog.suggestion_scroll,
                        -1,
                    );
                }
            }
            KeyCode::PageDown if matches!(&self.mode, Mode::Path(dialog) if !dialog.suggestions.is_empty()) => {
                if let Mode::Path(dialog) = &mut self.mode {
                    Workspace::move_suggestion(
                        &mut dialog.input,
                        &dialog.suggestions,
                        &mut dialog.active_suggestion,
                        &mut dialog.suggestion_scroll,
                        PATH_SUGGESTION_PAGE_SIZE as isize,
                    );
                }
            }
            KeyCode::PageUp if matches!(&self.mode, Mode::Path(dialog) if !dialog.suggestions.is_empty()) => {
                if let Mode::Path(dialog) = &mut self.mode {
                    Workspace::move_suggestion(
                        &mut dialog.input,
                        &dialog.suggestions,
                        &mut dialog.active_suggestion,
                        &mut dialog.suggestion_scroll,
                        -(PATH_SUGGESTION_PAGE_SIZE as isize),
                    );
                }
            }
            KeyCode::Home if matches!(&self.mode, Mode::Path(dialog) if !dialog.suggestions.is_empty()) => {
                if let Mode::Path(dialog) = &mut self.mode {
                    Workspace::select_suggestion(
                        &mut dialog.input,
                        &dialog.suggestions,
                        &mut dialog.active_suggestion,
                        &mut dialog.suggestion_scroll,
                        0,
                    );
                }
            }
            KeyCode::End if matches!(&self.mode, Mode::Path(dialog) if !dialog.suggestions.is_empty()) => {
                if let Mode::Path(dialog) = &mut self.mode {
                    let last = dialog.suggestions.len() - 1;
                    Workspace::select_suggestion(
                        &mut dialog.input,
                        &dialog.suggestions,
                        &mut dialog.active_suggestion,
                        &mut dialog.suggestion_scroll,
                        last,
                    );
                }
            }
            KeyCode::Enter => {
                let selected_directory = match &self.mode {
                    Mode::Path(dialog) => dialog
                        .active_suggestion
                        .and_then(|index| dialog.suggestions.get(index))
                        .filter(|path| path.is_dir())
                        .cloned(),
                    _ => None,
                };
                if let Some(path) = selected_directory {
                    if let Mode::Path(dialog) = &mut self.mode {
                        dialog.input.set_value(completion_display_path(&path));
                        dialog.suggestions.clear();
                        dialog.active_suggestion = None;
                        dialog.suggestion_scroll = 0;
                    }
                    return;
                }
                let (action, value) = match &self.mode {
                    Mode::Path(dialog) => (dialog.action, dialog.input.value.clone()),
                    _ => return,
                };
                self.perform_path_action(action, PathBuf::from(value.trim()));
            }
            _ => {
                if let Mode::Path(dialog) = &mut self.mode {
                    dialog.input.handle_key(key);
                    dialog.suggestions.clear();
                    dialog.active_suggestion = None;
                    dialog.suggestion_scroll = 0;
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
                KeyCode::Char('r') => self.mode = Mode::ConfirmReset(ResetTarget::Theme),
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
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.mode = Mode::ConfirmReset(ResetTarget::Settings);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Tab | KeyCode::Down => {
                if let Mode::Settings(editor) = &mut self.mode {
                    editor.active = (editor.active + 1) % 7;
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Mode::Settings(editor) = &mut self.mode {
                    editor.active = editor.active.checked_sub(1).unwrap_or(6);
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                self.toggle_setting(active);
            }
            _ => {}
        }
    }

    fn handle_reset_confirmation_key(&mut self, key: KeyEvent) {
        let target = match self.mode {
            Mode::ConfirmReset(target) => target,
            _ => return,
        };
        match key.code {
            KeyCode::Char('y' | 'Y') => match target {
                ResetTarget::Theme => {
                    self.theme = Theme::default();
                    self.status = "Theme reset to defaults".into();
                    self.mode = Mode::Theme(ThemeEditor::default());
                }
                ResetTarget::Settings => {
                    self.settings = ViewerSettings::default();
                    self.rebuild_display_rows();
                    if let Some(selection) = self.selection {
                        self.ensure_visible(selection.cursor);
                    }
                    self.status = "Viewer settings reset to defaults".into();
                    self.mode = Mode::Settings(SettingsEditor::default());
                }
            },
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.status = "Reset cancelled".into();
                self.mode = match target {
                    ResetTarget::Theme => Mode::Theme(ThemeEditor::default()),
                    ResetTarget::Settings => Mode::Settings(SettingsEditor::default()),
                };
            }
            _ => {
                self.status = "Type y to reset or n to cancel".into();
            }
        }
    }

    fn open_python_pane_with_session(&mut self, session: PythonSession) {
        self.mode = Mode::Python(PythonPane {
                    input: TextInput::default(),
                    repl_lines: Vec::new(),
                    output: vec![
                        "Python 3 analysis console".into(),
                        "Active: buffer, selected, selection_start/end | all documents: buffer_N, selected_N, buffers".into(),
                        "Preloaded: struct, binascii, hashlib, base64, zlib, math, re, pathlib"
                            .into(),
                        "Use rexedit_help() for the complete namespace; :apply writes all same-length buffers.".into(),
                    ],
                    session,
                    pending: 0,
                    scroll: 0,
                    visible_output_lines: 1,
                    history: self.python_history.clone(),
                    history_index: None,
                    history_draft: String::new(),
                });
        self.focus = Focus::Python;
        self.status = "Python pane opened".into();
    }

    #[cfg(test)]
    fn open_python_pane(&mut self) {
        let selection = self.selection.unwrap_or_else(|| Selection::new(0));
        let document = PythonDocument {
            index: 0,
            bytes: self.bytes.as_ref().clone(),
            selection_start: selection.start().min(self.bytes.len().saturating_sub(1)),
            selection_end: selection.end().min(self.bytes.len().saturating_sub(1)),
            selections: self
                .selected_ranges()
                .into_iter()
                .map(|range| (range.start(), range.end()))
                .collect(),
        };
        if let Ok(session) = PythonSession::start(vec![document], 0) {
            self.open_python_pane_with_session(session);
        }
    }

    fn handle_python_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            if self.focus == Focus::Viewer && self.edit_mode {
                self.commit_pending_edit();
                self.edit_mode = false;
                self.edit_kind = EditKind::Overwrite;
                self.edit_high_nibble = true;
                self.insert_at_end = false;
                self.status = "Python mode: hex View Mode".into();
                return;
            }
            if self.focus == Focus::Python
                && let Mode::Python(pane) = &mut self.mode
                && (!pane.repl_lines.is_empty() || !pane.input.value.is_empty())
            {
                pane.repl_lines.clear();
                pane.input = TextInput::default();
                self.status = "Discarded the unfinished Python block".into();
                return;
            }
            if let Mode::Python(pane) = &self.mode {
                self.python_history = pane.history.clone();
            }
            self.mode = Mode::Normal;
            self.focus = Focus::Viewer;
            self.status = "Python pane closed".into();
            return;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.cycle_python_focus(key.code == KeyCode::Tab);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            let result = match &self.mode {
                Mode::Python(pane) if pane.pending > 0 => pane.session.interrupt(),
                Mode::Python(_) => {
                    self.status = "No Python command is currently running".into();
                    return;
                }
                _ => return,
            };
            self.status = match result {
                Ok(()) => "Interrupt sent to Python".into(),
                Err(error) => error,
            };
            return;
        }
        if self.focus != Focus::Python {
            self.handle_python_content_key(key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
            if let Mode::Python(pane) = &mut self.mode {
                pane.output.clear();
                pane.scroll = 0;
            }
            return;
        }
        match key.code {
            KeyCode::Up => {
                if let Mode::Python(pane) = &mut self.mode {
                    navigate_python_history(pane, true);
                }
                return;
            }
            KeyCode::Down => {
                if let Mode::Python(pane) = &mut self.mode {
                    navigate_python_history(pane, false);
                }
                return;
            }
            KeyCode::PageUp => {
                if let Mode::Python(pane) = &mut self.mode {
                    pane.scroll = pane.scroll.saturating_add(10);
                    pane.clamp_scroll();
                }
                return;
            }
            KeyCode::PageDown => {
                if let Mode::Python(pane) = &mut self.mode {
                    pane.scroll = pane.scroll.saturating_sub(10);
                }
                return;
            }
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Python(pane) = &mut self.mode {
                    pane.scroll = pane.max_scroll();
                }
                return;
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Mode::Python(pane) = &mut self.mode {
                    pane.scroll = 0;
                }
                return;
            }
            _ => {}
        }
        if key.code == KeyCode::Enter {
            let mut command = match &mut self.mode {
                Mode::Python(pane) => pane.input.take_value(),
                _ => return,
            };
            let result = if let Mode::Python(pane) = &mut self.mode {
                if !pane.repl_lines.is_empty() {
                    if command.trim().is_empty() {
                        command = std::mem::take(&mut pane.repl_lines).join("\n");
                    } else {
                        let indentation = python_continuation_indentation(&command);
                        pane.repl_lines.push(command);
                        pane.input.set_value(indentation);
                        pane.scroll = 0;
                        return;
                    }
                } else if command.trim_end().ends_with(':') {
                    let indentation = python_continuation_indentation(&command);
                    pane.repl_lines.push(command);
                    pane.input.set_value(indentation);
                    pane.scroll = 0;
                    return;
                } else if command.trim().is_empty() {
                    pane.output.push(">>>".into());
                    pane.scroll = 0;
                    return;
                }
                pane.output
                    .extend(command.lines().enumerate().map(|(index, line)| {
                        format!("{} {line}", if index == 0 { ">>>" } else { "..." })
                    }));
                pane.scroll = 0;
                if pane.history.last() != Some(&command) {
                    pane.history.push(command.clone());
                }
                self.python_history = pane.history.clone();
                pane.history_index = None;
                pane.history_draft.clear();
                let result = if command.trim() == ":apply" {
                    pane.session.apply()
                } else {
                    pane.session.execute(command)
                };
                if result.is_ok() {
                    pane.pending += 1;
                }
                result
            } else {
                return;
            };
            if let Err(error) = result {
                self.status = error;
            }
            return;
        }
        if let Mode::Python(pane) = &mut self.mode {
            pane.input.handle_key(key);
            pane.history_index = None;
            pane.history_draft.clear();
        }
    }

    fn cycle_python_focus(&mut self, forward: bool) {
        self.focus = match (self.focus, self.settings.show_sidebar, forward) {
            (Focus::Viewer, true, true) => Focus::Fields,
            (Focus::Viewer, false, true) => Focus::Python,
            (Focus::Fields, _, true) => Focus::Python,
            (Focus::Python, _, true) => Focus::Viewer,
            (Focus::Viewer, _, false) => Focus::Python,
            (Focus::Fields, _, false) => Focus::Viewer,
            (Focus::Python, true, false) => Focus::Fields,
            (Focus::Python, false, false) => Focus::Viewer,
        };
        self.status = match self.focus {
            Focus::Viewer => "Python mode: hex viewer focused".into(),
            Focus::Fields => "Python mode: fields pane focused".into(),
            Focus::Python => "Python mode: console focused".into(),
        };
    }

    fn handle_python_content_key(&mut self, key: KeyEvent) {
        if self.focus == Focus::Viewer && self.edit_mode {
            self.handle_python_overwrite_key(key);
            return;
        }
        match (self.focus, key.code) {
            (Focus::Viewer, KeyCode::Char('i')) => {
                self.edit_mode = true;
                self.edit_kind = EditKind::Overwrite;
                self.edit_high_nibble = true;
                self.insert_at_end = false;
                self.status = "Python mode: hex Overwrite Mode (Esc returns to View Mode)".into();
            }
            (Focus::Viewer, KeyCode::Left) => {
                self.move_cursor(-1, key.modifiers.contains(KeyModifiers::SHIFT));
            }
            (Focus::Viewer, KeyCode::Right) => {
                self.move_cursor(1, key.modifiers.contains(KeyModifiers::SHIFT));
            }
            (Focus::Viewer, KeyCode::Up) => self.move_cursor(
                -(self.settings.bytes_per_row as isize),
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            (Focus::Viewer, KeyCode::Down) => self.move_cursor(
                self.settings.bytes_per_row as isize,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            (Focus::Viewer, KeyCode::PageUp) => self.move_cursor(
                -(self
                    .visible_rows
                    .saturating_mul(self.settings.bytes_per_row) as isize),
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            (Focus::Viewer, KeyCode::PageDown) => self.move_cursor(
                self.visible_rows
                    .saturating_mul(self.settings.bytes_per_row) as isize,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            (Focus::Viewer, KeyCode::Home) => {
                self.select_offset(0, key.modifiers.contains(KeyModifiers::SHIFT));
            }
            (Focus::Viewer, KeyCode::End) if !self.bytes.is_empty() => {
                self.select_offset(
                    self.bytes.len() - 1,
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
            }
            (Focus::Fields, KeyCode::Up | KeyCode::Char('[')) => self.select_previous_field(),
            (Focus::Fields, KeyCode::Down | KeyCode::Char(']')) => self.select_next_field(),
            (Focus::Fields, KeyCode::Char('d') | KeyCode::Delete) => self.delete_selected_field(),
            _ => {}
        }
    }

    fn handle_python_overwrite_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('u') => self.undo_overwrite(),
                KeyCode::Char('r') => self.redo_overwrite(),
                KeyCode::Char('s') => {
                    self.status = "Close the Python pane before saving the binary".into();
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Insert | KeyCode::Char('i') => self.toggle_edit_kind(),
            KeyCode::Backspace | KeyCode::Delete => self.delete_selected_bytes(),
            KeyCode::Char(character) if character.is_ascii_hexdigit() => {
                self.edit_nibble(character.to_digit(16).expect("hex digit") as u8)
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
    }

    fn drain_python_messages(&mut self) -> Vec<PythonSnapshot> {
        let responses = match &mut self.mode {
            Mode::Python(pane) => pane.session.responses.try_iter().collect::<Vec<_>>(),
            _ => return Vec::new(),
        };
        let mut snapshots = Vec::new();
        for response in responses {
            let snapshot = match &mut self.mode {
                Mode::Python(pane) => {
                    pane.pending = pane.pending.saturating_sub(1);
                    if !response.output.is_empty() {
                        pane.output.extend(wrap_python_output(&response.output));
                    }
                    if let Some(error) = response.error {
                        pane.output.extend(wrap_python_output(&error));
                    }
                    pane.scroll = 0;
                    response.applied.then(|| pane.session.snapshots.clone())
                }
                _ => None,
            };
            if let Some(applied) = snapshot {
                snapshots.extend(applied);
            }
        }
        snapshots
    }

    fn apply_python_snapshot(&mut self, snapshot: &PythonSnapshot) {
        let python_bytes = match fs::read(&snapshot.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status = format!("Could not read Python buffer: {error}");
                return;
            }
        };
        if python_bytes.len() != self.bytes.len() || snapshot.baseline.len() != self.bytes.len() {
            self.status = format!(
                "Python buffer length changed from {} to {}; only same-length edits can be applied",
                self.bytes.len(),
                python_bytes.len()
            );
            return;
        }
        let mut merged = self.bytes.as_ref().clone();
        let mut applied = 0;
        let mut conflicts = 0;
        for (offset, ((baseline, python), editor)) in snapshot
            .baseline
            .iter()
            .zip(&python_bytes)
            .zip(self.bytes.iter())
            .enumerate()
        {
            let python_changed = python != baseline;
            let editor_changed = editor != baseline;
            if python_changed && editor_changed && python != editor {
                conflicts += 1;
            } else if python_changed && python != editor {
                merged[offset] = *python;
                applied += 1;
            }
        }
        if applied == 0 && conflicts == 0 {
            self.status = "Python apply found no buffer changes".into();
            return;
        }
        self.cancel_search();
        self.search.results.clear();
        self.bytes = Arc::new(merged);
        self.modified_offsets = self
            .bytes
            .iter()
            .zip(self.saved_bytes.iter())
            .enumerate()
            .filter_map(|(offset, (current, saved))| (current != saved).then_some(offset))
            .collect();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.pending_edit = None;
        self.invalidate_entropy();
        self.rebuild_display_rows();
        self.status = match (applied, conflicts) {
            (0, conflicts) => format!(
                "Python apply kept {} direct hex edit conflict(s); no changes applied",
                conflicts
            ),
            (applied, 0) => format!("Applied {applied} Python byte change(s); Ctrl+S saves"),
            (applied, conflicts) => format!(
                "Applied {applied} Python byte change(s); kept {conflicts} direct hex edit conflict(s)"
            ),
        };
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
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            match &mut self.mode {
                Mode::Search(input) | Mode::Jump(input) => input.clear_selection(),
                Mode::Field(editor) => {
                    if let Some(input) = editor.active_text_mut() {
                        input.clear_selection();
                    }
                }
                Mode::Path(dialog) => dialog.input.clear_selection(),
                _ => {}
            }
        }
        if let Mode::Help(help) = &mut self.mode {
            match mouse.kind {
                MouseEventKind::ScrollUp => help.scroll = help.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => help.scroll = help.scroll.saturating_add(3),
                _ => {}
            }
            return;
        }
        if let Mode::Path(dialog) = &mut self.mode {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    dialog.suggestion_scroll = dialog.suggestion_scroll.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    let max_scroll = dialog
                        .suggestions
                        .len()
                        .saturating_sub(PATH_SUGGESTION_PAGE_SIZE);
                    dialog.suggestion_scroll =
                        dialog.suggestion_scroll.saturating_add(3).min(max_scroll);
                }
                _ => {}
            }
            return;
        }
        if self.handle_scrollbar_mouse(mouse) {
            return;
        }
        if matches!(self.mode, Mode::Python(_))
            && self.python_area.contains((mouse.column, mouse.row).into())
        {
            self.focus = Focus::Python;
            let Mode::Python(pane) = &mut self.mode else {
                return;
            };
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    pane.scroll = pane.scroll.saturating_add(3);
                    pane.clamp_scroll();
                }
                MouseEventKind::ScrollDown => {
                    pane.scroll = pane.scroll.saturating_sub(3);
                }
                _ => {}
            }
            return;
        }
        if matches!(self.mode, Mode::Python(_)) {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if self.viewer_area.contains((mouse.column, mouse.row).into()) {
                    self.focus = Focus::Viewer;
                } else if self.fields_area.contains((mouse.column, mouse.row).into()) {
                    self.focus = Focus::Fields;
                }
            }
            self.handle_content_mouse(mouse);
            return;
        }
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        self.handle_content_mouse(mouse);
    }

    fn handle_content_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp
                if self.fields_area.contains((mouse.column, mouse.row).into()) =>
            {
                self.fields_scroll = self.fields_scroll.saturating_sub(3);
                self.focus = Focus::Fields;
            }
            MouseEventKind::ScrollDown
                if self.fields_area.contains((mouse.column, mouse.row).into()) =>
            {
                self.fields_scroll = self
                    .fields_scroll
                    .saturating_add(3)
                    .min(self.field_max_scroll());
                self.focus = Focus::Fields;
            }
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(3).min(self.max_scroll());
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(offset) = self.byte_at(mouse.column, mouse.row) {
                    if self.edit_mode {
                        self.commit_pending_edit();
                    }
                    let additive = mouse.modifiers.contains(KeyModifiers::CONTROL);
                    if additive {
                        if let Some(selection) = self.selection.take() {
                            self.additional_selections.push(selection);
                        }
                    } else {
                        self.additional_selections.clear();
                    }
                    self.selection = Some(Selection::new(offset));
                    self.focus = Focus::Viewer;
                    self.mouse_dragging = true;
                    self.edit_high_nibble = true;
                    self.insert_at_end = false;
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
            MouseEventKind::Up(MouseButton::Left) => {
                self.mouse_dragging = false;
            }
            _ => {}
        }
    }

    fn handle_scrollbar_mouse(&mut self, mouse: MouseEvent) -> bool {
        let drag_target = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if is_scrollbar_column(self.python_area, mouse.column)
                    && self.python_area.contains((mouse.column, mouse.row).into())
                    && matches!(self.mode, Mode::Python(_))
                {
                    Some(ScrollbarDrag::Python)
                } else if is_scrollbar_column(self.fields_area, mouse.column)
                    && self.fields_area.contains((mouse.column, mouse.row).into())
                {
                    Some(ScrollbarDrag::Fields)
                } else if is_scrollbar_column(self.viewer_area, mouse.column)
                    && self.viewer_area.contains((mouse.column, mouse.row).into())
                {
                    Some(ScrollbarDrag::Viewer)
                } else {
                    None
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.scrollbar_dragging,
            MouseEventKind::Up(MouseButton::Left) => {
                let was_dragging = self.scrollbar_dragging.take().is_some();
                return was_dragging;
            }
            _ => None,
        };
        let Some(target) = drag_target else {
            return false;
        };
        self.scrollbar_dragging = Some(target);
        match target {
            ScrollbarDrag::Viewer => {
                self.focus = Focus::Viewer;
                self.scroll =
                    scrollbar_position_from_row(self.viewer_area, mouse.row, self.max_scroll());
            }
            ScrollbarDrag::Fields => {
                self.focus = Focus::Fields;
                self.fields_scroll = scrollbar_position_from_row(
                    self.fields_area,
                    mouse.row,
                    self.field_max_scroll(),
                );
            }
            ScrollbarDrag::Python => {
                self.focus = Focus::Python;
                let Mode::Python(pane) = &mut self.mode else {
                    return false;
                };
                let top_position =
                    scrollbar_position_from_row(self.python_area, mouse.row, pane.max_scroll());
                pane.scroll = pane.max_scroll().saturating_sub(top_position);
            }
        }
        true
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
        let display_row = *self.display_rows.get(data_row)?;
        if let DisplayRow::Repeated { start, .. } = display_row {
            return Some(start);
        }
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
        let offset = display_row.start().checked_add(byte_column)?;
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
        let local_row = usize::from(row.checked_sub(inner_top)?);
        if local_row < self.visible_fields {
            let index = self.fields_scroll.saturating_add(local_row);
            (index < self.fields.len()).then_some(index)
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
            self.additional_selections.clear();
        }
        self.insert_at_end = false;
        self.ensure_visible(offset);
    }

    fn copy_selection_as_hex(&mut self) {
        let selected = self.selected_bytes();
        let hex = hex_string(&selected);
        if hex.is_empty() {
            self.status = "No bytes selected to copy".into();
            return;
        }
        match copy_to_clipboard(&hex) {
            Ok(()) => self.status = format!("Copied {} bytes as continuous hex", hex.len() / 2),
            Err(error) => self.status = format!("Could not copy selection: {error}"),
        }
    }

    /// Reads hex directly from the system clipboard and applies it through the
    /// same batched path as a bracketed paste. Terminal-relayed paste is
    /// unreliable on some platforms (notably Windows, where bracketed paste
    /// support in crossterm falls back to a flood of individual keystrokes),
    /// so this keybind is the dependable route to a fast paste everywhere.
    fn paste_from_clipboard(&mut self) {
        if !self.edit_mode {
            self.status = "Enter Overwrite or Insert Mode (i) before pasting bytes".into();
            return;
        }
        match read_clipboard() {
            Ok(text) => self.paste_hex_bytes(&text),
            Err(error) => self.status = format!("Could not paste from clipboard: {error}"),
        }
    }

    fn toggle_edit_kind(&mut self) {
        self.commit_pending_edit();
        self.edit_kind = self.edit_kind.toggle();
        self.edit_high_nibble = true;
        self.insert_at_end = false;
        self.status = match self.edit_kind {
            EditKind::Overwrite => "Overwrite Mode: type two hex digits per byte".into(),
            EditKind::Insert => {
                "Insert Mode: type two hex digits to insert; Backspace/Delete removes selection"
                    .into()
            }
        };
    }

    fn edit_nibble(&mut self, nibble: u8) {
        match self.edit_kind {
            EditKind::Overwrite => self.overwrite_nibble(nibble),
            EditKind::Insert => self.insert_nibble(nibble),
        }
    }

    fn overwrite_nibble(&mut self, nibble: u8) {
        let Some(offset) = self.selection.map(|selection| selection.cursor) else {
            return;
        };
        if offset >= self.bytes.len() {
            return;
        }
        self.cancel_search();
        self.search.results.clear();
        let bytes = Arc::make_mut(&mut self.bytes);
        let original = bytes[offset];
        if self.edit_high_nibble {
            self.pending_edit = Some(PendingEdit::Overwrite {
                offset,
                before: original,
            });
        }
        bytes[offset] = if self.edit_high_nibble {
            (nibble << 4) | (original & 0x0F)
        } else {
            (original & 0xF0) | nibble
        };
        self.invalidate_entropy();
        self.rebuild_display_rows();
        self.refresh_modified_offsets();
        if self.edit_high_nibble {
            self.edit_high_nibble = false;
        } else {
            self.commit_pending_edit();
            let next = (offset + 1).min(self.bytes.len() - 1);
            self.selection = Some(Selection::new(next));
            self.additional_selections.clear();
            self.ensure_visible(next);
        }
        self.status = format!("Modified byte at 0x{offset:X}; Ctrl+S saves");
    }

    fn insert_nibble(&mut self, nibble: u8) {
        let offset = if self.insert_at_end {
            self.bytes.len()
        } else {
            self.selection
                .map(|selection| selection.cursor)
                .unwrap_or(self.bytes.len())
        };
        if self.edit_high_nibble {
            self.pending_edit = Some(PendingEdit::Insert {
                offset,
                high_nibble: nibble,
            });
            self.edit_high_nibble = false;
            self.status = format!("Insert at 0x{offset:X}: enter the second hex digit");
            return;
        }
        let Some(PendingEdit::Insert {
            offset,
            high_nibble,
        }) = self.pending_edit.take()
        else {
            self.edit_high_nibble = true;
            return;
        };
        let byte = (high_nibble << 4) | nibble;
        self.insert_bytes_at(offset, &[byte]);
        self.undo_stack.push(EditAction::Insert { offset, byte });
        self.redo_stack.clear();
        self.edit_high_nibble = true;
        self.additional_selections.clear();
        let next = (offset + 1).min(self.bytes.len().saturating_sub(1));
        self.selection = Some(Selection::new(next));
        self.insert_at_end = offset + 1 == self.bytes.len();
        self.ensure_visible(next);
        self.status = format!("Inserted {byte:02X} at 0x{offset:X}; Ctrl+S saves");
    }

    fn delete_selected_bytes(&mut self) {
        self.commit_pending_edit();
        let Some(selection) = self.selection else {
            self.status = "No bytes selected to delete".into();
            return;
        };
        if self.bytes.is_empty() {
            self.status = "No bytes selected to delete".into();
            return;
        }
        let start = selection.start().min(self.bytes.len() - 1);
        let end = selection.end().min(self.bytes.len() - 1);
        let removed = self.delete_bytes_at(start, end - start + 1);
        if removed.is_empty() {
            self.status = "No bytes selected to delete".into();
            return;
        }
        self.undo_stack.push(EditAction::Delete {
            offset: start,
            bytes: removed.clone(),
        });
        self.redo_stack.clear();
        self.additional_selections.clear();
        self.insert_at_end = false;
        if self.bytes.is_empty() {
            self.selection = None;
        } else {
            let next = start.min(self.bytes.len() - 1);
            self.selection = Some(Selection::new(next));
            self.ensure_visible(next);
        }
        self.status = format!("Deleted {} byte(s); Ctrl+S saves", removed.len());
    }

    /// Decodes pasted hexadecimal text and applies it in a single batched
    /// edit, instead of routing every nibble through the normal one-byte-at-a-time
    /// key handling. A per-character path re-shifts and re-scans the whole
    /// buffer once per byte, which is quadratic for large pastes.
    fn paste_hex_bytes(&mut self, text: &str) {
        self.commit_pending_edit();
        let mut bytes = Vec::new();
        let mut pending_high_nibble = None;
        for character in text.chars() {
            if !character.is_ascii_hexdigit() {
                continue;
            }
            let nibble = character.to_digit(16).expect("checked hex digit") as u8;
            match pending_high_nibble.take() {
                Some(high) => bytes.push((high << 4) | nibble),
                None => pending_high_nibble = Some(nibble),
            }
        }
        if bytes.is_empty() {
            self.status = "Clipboard did not contain a full byte of hex digits".into();
            return;
        }
        match self.edit_kind {
            EditKind::Insert => self.paste_insert_bytes(&bytes),
            EditKind::Overwrite => self.paste_overwrite_bytes(&bytes),
        }
        if pending_high_nibble.is_some() {
            self.status = format!("{} (trailing hex digit ignored)", self.status);
        }
    }

    fn paste_insert_bytes(&mut self, bytes: &[u8]) {
        let offset = if self.insert_at_end {
            self.bytes.len()
        } else {
            self.selection
                .map(|selection| selection.cursor)
                .unwrap_or(self.bytes.len())
        };
        self.insert_bytes_at(offset, bytes);
        self.undo_stack.push(EditAction::InsertMany {
            offset,
            bytes: bytes.to_vec(),
        });
        self.redo_stack.clear();
        self.edit_high_nibble = true;
        self.additional_selections.clear();
        let next = (offset + bytes.len()).min(self.bytes.len().saturating_sub(1));
        self.selection = Some(Selection::new(next));
        self.insert_at_end = offset + bytes.len() == self.bytes.len();
        self.ensure_visible(next);
        self.status = format!(
            "Inserted {} byte(s) at 0x{offset:X}; Ctrl+S saves",
            bytes.len()
        );
    }

    fn paste_overwrite_bytes(&mut self, bytes: &[u8]) {
        let Some(offset) = self.selection.map(|selection| selection.cursor) else {
            self.status = "No bytes selected to overwrite".into();
            return;
        };
        if offset >= self.bytes.len() {
            self.status = "No bytes selected to overwrite".into();
            return;
        }
        self.cancel_search();
        self.search.results.clear();
        let available = self.bytes.len() - offset;
        let truncated = bytes.len() > available;
        let bytes = &bytes[..bytes.len().min(available)];
        let buffer = Arc::make_mut(&mut self.bytes);
        let before = buffer[offset..offset + bytes.len()].to_vec();
        buffer[offset..offset + bytes.len()].copy_from_slice(bytes);
        self.invalidate_entropy();
        self.rebuild_display_rows();
        self.refresh_modified_offsets();
        self.undo_stack.push(EditAction::OverwriteMany {
            offset,
            before,
            after: bytes.to_vec(),
        });
        self.redo_stack.clear();
        self.edit_high_nibble = true;
        self.additional_selections.clear();
        let next = (offset + bytes.len()).min(self.bytes.len() - 1);
        self.selection = Some(Selection::new(next));
        self.ensure_visible(next);
        self.status = if truncated {
            format!(
                "Overwrote {} byte(s) at 0x{offset:X} (truncated at end of file); Ctrl+S saves",
                bytes.len()
            )
        } else {
            format!(
                "Overwrote {} byte(s) at 0x{offset:X}; Ctrl+S saves",
                bytes.len()
            )
        };
    }

    fn insert_bytes_at(&mut self, offset: usize, bytes_to_insert: &[u8]) {
        if bytes_to_insert.is_empty() {
            return;
        }
        let offset = offset.min(self.bytes.len());
        self.cancel_search();
        self.search.results.clear();
        Arc::make_mut(&mut self.bytes).splice(offset..offset, bytes_to_insert.iter().copied());
        self.shift_fields_for_insert(offset, bytes_to_insert.len());
        self.invalidate_entropy();
        self.rebuild_display_rows();
        self.refresh_modified_offsets();
    }

    fn delete_bytes_at(&mut self, offset: usize, length: usize) -> Vec<u8> {
        if length == 0 || offset >= self.bytes.len() {
            return Vec::new();
        }
        let end = offset.saturating_add(length).min(self.bytes.len());
        self.cancel_search();
        self.search.results.clear();
        let removed = Arc::make_mut(&mut self.bytes)
            .drain(offset..end)
            .collect::<Vec<_>>();
        self.shift_fields_for_delete(offset, removed.len());
        self.invalidate_entropy();
        self.rebuild_display_rows();
        self.refresh_modified_offsets();
        removed
    }

    fn shift_fields_for_insert(&mut self, offset: usize, length: usize) {
        for field in &mut self.fields {
            if field.start >= offset {
                field.start = field.start.saturating_add(length);
                field.end = field.end.saturating_add(length);
            } else if field.end >= offset {
                field.end = field.end.saturating_add(length);
            }
        }
    }

    fn shift_fields_for_delete(&mut self, offset: usize, length: usize) {
        if length == 0 {
            return;
        }
        let end = offset.saturating_add(length - 1);
        self.fields.retain_mut(|field| {
            if field.end < offset {
                return true;
            }
            if field.start > end {
                field.start = field.start.saturating_sub(length);
                field.end = field.end.saturating_sub(length);
                return true;
            }
            if field.start < offset {
                if field.end > end {
                    field.end = field.end.saturating_sub(length);
                } else {
                    field.end = offset.saturating_sub(1);
                }
                return field.start <= field.end;
            }
            if field.end > end {
                field.start = offset;
                field.end = field.end.saturating_sub(length);
                return true;
            }
            false
        });
        self.selected_field = self.selected_field.min(self.fields.len().saturating_sub(1));
        self.fields_scroll = self.fields_scroll.min(self.field_max_scroll());
    }

    fn commit_pending_edit(&mut self) {
        let Some(pending) = self.pending_edit.take() else {
            return;
        };
        if let PendingEdit::Overwrite { offset, before } = pending {
            let after = self.bytes[offset];
            if before != after {
                self.undo_stack.push(EditAction::Overwrite {
                    offset,
                    before,
                    after,
                });
                self.redo_stack.clear();
            }
        }
        self.edit_high_nibble = true;
    }

    fn undo_overwrite(&mut self) {
        self.commit_pending_edit();
        let Some(action) = self.undo_stack.pop() else {
            self.status = "Nothing to undo".into();
            return;
        };
        let (offset, description) = match &action {
            EditAction::Overwrite { offset, before, .. } => {
                self.cancel_search();
                self.search.results.clear();
                Arc::make_mut(&mut self.bytes)[*offset] = *before;
                self.invalidate_entropy();
                self.rebuild_display_rows();
                self.refresh_modified_offsets();
                (*offset, "overwrite")
            }
            EditAction::OverwriteMany { offset, before, .. } => {
                self.cancel_search();
                self.search.results.clear();
                Arc::make_mut(&mut self.bytes)[*offset..*offset + before.len()]
                    .copy_from_slice(before);
                self.invalidate_entropy();
                self.rebuild_display_rows();
                self.refresh_modified_offsets();
                (*offset, "overwrite")
            }
            EditAction::Insert { offset, .. } => {
                self.delete_bytes_at(*offset, 1);
                (*offset, "insertion")
            }
            EditAction::InsertMany { offset, bytes } => {
                self.delete_bytes_at(*offset, bytes.len());
                (*offset, "insertion")
            }
            EditAction::Delete { offset, bytes } => {
                self.insert_bytes_at(*offset, bytes);
                (*offset, "deletion")
            }
        };
        self.redo_stack.push(action);
        self.additional_selections.clear();
        self.insert_at_end = false;
        if self.bytes.is_empty() {
            self.selection = None;
        } else {
            let selection = offset.min(self.bytes.len() - 1);
            self.selection = Some(Selection::new(selection));
            self.ensure_visible(selection);
        }
        self.status = format!("Undid {description} at 0x{offset:X}");
    }

    fn redo_overwrite(&mut self) {
        self.commit_pending_edit();
        let Some(action) = self.redo_stack.pop() else {
            self.status = "Nothing to redo".into();
            return;
        };
        let (offset, description) = match &action {
            EditAction::Overwrite { offset, after, .. } => {
                self.cancel_search();
                self.search.results.clear();
                Arc::make_mut(&mut self.bytes)[*offset] = *after;
                self.invalidate_entropy();
                self.rebuild_display_rows();
                self.refresh_modified_offsets();
                (*offset, "overwrite")
            }
            EditAction::OverwriteMany { offset, after, .. } => {
                self.cancel_search();
                self.search.results.clear();
                Arc::make_mut(&mut self.bytes)[*offset..*offset + after.len()]
                    .copy_from_slice(after);
                self.invalidate_entropy();
                self.rebuild_display_rows();
                self.refresh_modified_offsets();
                (*offset, "overwrite")
            }
            EditAction::Insert { offset, byte } => {
                self.insert_bytes_at(*offset, &[*byte]);
                (*offset, "insertion")
            }
            EditAction::InsertMany { offset, bytes } => {
                self.insert_bytes_at(*offset, bytes);
                (*offset, "insertion")
            }
            EditAction::Delete { offset, bytes } => {
                self.delete_bytes_at(*offset, bytes.len());
                (*offset, "deletion")
            }
        };
        self.undo_stack.push(action);
        self.additional_selections.clear();
        self.insert_at_end = false;
        if self.bytes.is_empty() {
            self.selection = None;
        } else {
            let selection = offset.min(self.bytes.len() - 1);
            self.selection = Some(Selection::new(selection));
            self.ensure_visible(selection);
        }
        self.status = format!("Redid {description} at 0x{offset:X}");
    }

    fn refresh_modified_offsets(&mut self) {
        self.modified_offsets = (0..self.bytes.len().max(self.saved_bytes.len()))
            .filter(|offset| self.bytes.get(*offset) != self.saved_bytes.get(*offset))
            .collect();
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
                self.rebuild_display_rows();
                if let Some(selection) = self.selection {
                    self.ensure_visible(selection.cursor);
                }
            }
            2 => self.settings.uppercase_hex = !self.settings.uppercase_hex,
            3 => self.settings.show_offsets = !self.settings.show_offsets,
            4 => self.settings.show_sidebar = !self.settings.show_sidebar,
            5 => {
                self.settings.compress_repeated_rows = !self.settings.compress_repeated_rows;
                self.rebuild_display_rows();
                if let Some(selection) = self.selection {
                    self.ensure_visible(selection.cursor);
                }
            }
            6 => self.settings.show_overlays = !self.settings.show_overlays,
            _ => {}
        }
    }

    pub fn display_row_for_offset(&self, offset: usize) -> usize {
        let index = self
            .display_rows
            .partition_point(|row| row.end(self.settings.bytes_per_row, self.bytes.len()) < offset);
        index.min(self.display_rows.len().saturating_sub(1))
    }

    fn rebuild_display_rows(&mut self) {
        self.display_rows.clear();
        let bytes_per_row = self.settings.bytes_per_row.max(1);
        let physical_rows = self.bytes.len().div_ceil(bytes_per_row);
        let mut row = 0;
        while row < physical_rows {
            let offset = row * bytes_per_row;
            if self.settings.compress_repeated_rows && offset + bytes_per_row <= self.bytes.len() {
                let byte = self.bytes[offset];
                let uniform = self.bytes[offset..offset + bytes_per_row]
                    .iter()
                    .all(|candidate| *candidate == byte);
                if uniform {
                    let mut run_end = row + 1;
                    while run_end < physical_rows {
                        let next = run_end * bytes_per_row;
                        if next + bytes_per_row > self.bytes.len()
                            || !self.bytes[next..next + bytes_per_row]
                                .iter()
                                .all(|candidate| *candidate == byte)
                        {
                            break;
                        }
                        run_end += 1;
                    }
                    let run_rows = run_end - row;
                    if run_rows >= 3 {
                        self.display_rows.push(DisplayRow::Repeated {
                            start: offset,
                            end: run_end * bytes_per_row - 1,
                            byte,
                            physical_rows: run_rows,
                        });
                        row = run_end;
                        continue;
                    }
                }
            }
            self.display_rows.push(DisplayRow::Bytes { offset });
            row += 1;
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
        let use_selected_ranges = editor.editing.is_none()
            && editor.start.value.trim().is_empty()
            && editor.end.value.trim().is_empty()
            && editor.ranges.len() > 1;
        let start = match editor.start.value.trim() {
            "" => self.selection.map_or(0, Selection::start),
            input => match parse_offset(input) {
                Ok(value) => value,
                Err(error) => {
                    self.status = format!("Invalid start: {error}");
                    return;
                }
            },
        };
        let end = match editor.end.value.trim() {
            "" => self.selection.map_or(0, Selection::end),
            input => match parse_offset(input) {
                Ok(value) => value,
                Err(error) => {
                    self.status = format!("Invalid end: {error}");
                    return;
                }
            },
        };
        if self.bytes.is_empty() || start > end || end >= self.bytes.len() {
            self.status = "Field range must be ordered and inside the file".into();
            return;
        }
        let name = if editor.name.value.trim().is_empty() {
            format!("field_{}", self.fields.len() + 1)
        } else {
            editor.name.value.trim().to_owned()
        };
        if let Some(index) = editor.editing {
            self.fields[index] = Field {
                name,
                description: editor.description.value.trim().to_owned(),
                start,
                end,
                color: editor.color,
            };
            self.selected_field = index;
            self.status = "Field updated".into();
        } else if use_selected_ranges {
            let ranges = editor.ranges.clone();
            let description = editor.description.value.trim().to_owned();
            let color = editor.color;
            let added = ranges.len();
            for (index, range) in ranges.into_iter().enumerate() {
                self.fields.push(Field {
                    name: if index == 0 {
                        name.clone()
                    } else {
                        format!("{name} [{}]", index + 1)
                    },
                    description: description.clone(),
                    start: range.start(),
                    end: range.end(),
                    color,
                });
            }
            self.selected_field = self.fields.len().saturating_sub(1);
            self.status = format!("Added {added} fields from the separate selections");
        } else {
            self.fields.push(Field {
                name,
                description: editor.description.value.trim().to_owned(),
                start,
                end,
                color: editor.color,
            });
            self.selected_field = self.fields.len() - 1;
            self.status = "Field added".into();
        }
        self.mode = Mode::Normal;
        self.activate_selected_field();
        self.save_automatic_overlay_after_change();
    }

    fn delete_selected_field(&mut self) {
        if self.fields.is_empty() {
            return;
        }
        let removed = self.fields.remove(self.selected_field);
        self.selected_field = self.selected_field.min(self.fields.len().saturating_sub(1));
        self.ensure_selected_field_visible();
        self.status = format!("Deleted field '{}'", removed.name);
        self.save_automatic_overlay_after_change();
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
            self.additional_selections.clear();
            self.additional_selections.clear();
            self.ensure_visible(start);
            self.ensure_selected_field_visible();
        }
    }

    fn ensure_selected_field_visible(&mut self) {
        if self.selected_field < self.fields_scroll {
            self.fields_scroll = self.selected_field;
        } else if self.selected_field >= self.fields_scroll.saturating_add(self.visible_fields) {
            self.fields_scroll = self
                .selected_field
                .saturating_add(1)
                .saturating_sub(self.visible_fields);
        }
        self.fields_scroll = self.fields_scroll.min(self.field_max_scroll());
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
            PathAction::SaveOverlay | PathAction::LoadOverlay => self.automatic_overlay_path(),
            PathAction::SaveBinary => self.path.clone(),
            PathAction::SaveTheme | PathAction::LoadTheme => suggested_named_path(&format!(
                "{}.rexedit-theme.json",
                safe_name(&self.theme.name)
            )),
        };
        self.mode = Mode::Path(PathDialog {
            action,
            input: TextInput::with_value(suggested.display().to_string()),
            suggestions: Vec::new(),
            active_suggestion: None,
            suggestion_scroll: 0,
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
        let automatic_dir = overlay_storage_dir();
        if path.starts_with(&automatic_dir) {
            fs::create_dir_all(&automatic_dir).map_err(|error| {
                format!(
                    "Could not create overlay storage directory {}: {error}",
                    automatic_dir.display()
                )
            })?;
        }
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
        match self.persist_automatic_overlay() {
            Ok(Some(overlay)) => Ok(format!(
                "Saved binary to {}; overlay saved to {}",
                path.display(),
                overlay.display()
            )),
            Ok(None) => Ok(format!("Saved binary to {}", path.display())),
            Err(error) => Ok(format!("Saved binary to {}; {error}", path.display())),
        }
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

fn overlay_storage_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(directory) = env::var_os("APPDATA") {
        return PathBuf::from(directory).join("rexedit").join("overlays");
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = user_home_dir() {
        return home
            .join("Library")
            .join("Application Support")
            .join("rexedit")
            .join("overlays");
    }

    if let Some(directory) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(directory).join("rexedit").join("overlays");
    }
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("rexedit")
        .join("overlays")
}

fn user_home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn content_identity(bytes: &[u8]) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut first = FNV_OFFSET;
    let mut second = FNV_OFFSET ^ 0x9e37_79b9_7f4a_7c15;
    for byte in bytes {
        first ^= u64::from(*byte);
        first = first.wrapping_mul(FNV_PRIME);
        second ^= u64::from(*byte).wrapping_add(0x9d);
        second = second.wrapping_mul(FNV_PRIME);
    }
    format!("{:016x}{:016x}{:016x}", bytes.len(), first, second)
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

fn path_completion_candidates(input: &str) -> Vec<PathBuf> {
    let typed = Path::new(input);
    let has_trailing_separator =
        input.ends_with('/') || input.ends_with('\\') || input.ends_with(std::path::MAIN_SEPARATOR);
    let (directory, prefix) = if input.is_empty() {
        (PathBuf::from("."), "")
    } else if has_trailing_separator {
        (typed.to_owned(), "")
    } else {
        let prefix = typed
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let directory = typed
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_owned);
        (directory, prefix)
    };
    let mut candidates = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.starts_with(prefix).then(|| {
                (
                    entry.path(),
                    entry.file_type().is_ok_and(|kind| kind.is_dir()),
                )
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_path, left_is_dir), (right_path, right_is_dir)| {
        right_is_dir
            .cmp(left_is_dir)
            .then_with(|| left_path.file_name().cmp(&right_path.file_name()))
    });
    candidates.into_iter().map(|(path, _)| path).collect()
}

fn completion_display_path(path: &Path) -> String {
    let mut display = path.display().to_string();
    if path.is_dir() && !display.ends_with(std::path::MAIN_SEPARATOR) {
        display.push(std::path::MAIN_SEPARATOR);
    }
    display
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[cfg(windows)]
fn copy_to_clipboard(content: &str) -> Result<(), String> {
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-STA",
            "-Command",
            "Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the clipboard command: {error}"))?;
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| "clipboard command did not accept input".to_string())?;
    stdin
        .write_all(content.as_bytes())
        .map_err(|error| format!("could not write clipboard data: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("clipboard command failed: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if error.is_empty() {
            "clipboard command did not complete successfully".into()
        } else {
            error
        })
    }
}

#[cfg(not(windows))]
fn copy_to_clipboard(content: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let commands = vec![("pbcopy", Vec::new())];
    #[cfg(not(target_os = "macos"))]
    let commands = vec![
        ("wl-copy", Vec::new()),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
    ];

    let mut errors = Vec::new();
    for (program, args) in commands {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match write_clipboard_command(&mut command, content) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{program}: {error}")),
        }
    }

    execute!(io::stdout(), CopyToClipboard::to_clipboard_from(content)).map_err(|error| {
        let attempts = errors.join("; ");
        if attempts.is_empty() {
            error.to_string()
        } else {
            format!("{attempts}; terminal clipboard fallback failed: {error}")
        }
    })
}

#[cfg(not(windows))]
fn write_clipboard_command(command: &mut Command, content: &str) -> Result<(), String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "did not accept input".to_string())?;
    stdin
        .write_all(content.as_bytes())
        .map_err(|error| format!("could not write data: {error}"))?;
    drop(stdin);

    // X11 clipboard owners such as xclip intentionally stay alive until another
    // application takes ownership. Waiting for them here blocks the event loop
    // forever after Ctrl+C. Give a command a short window to report an immediate
    // failure, then let a small reaper thread wait for long-lived owners.
    let deadline = Instant::now() + Duration::from_millis(150);
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("could not check clipboard command: {error}"))?
        {
            Some(status) if status.success() => return Ok(()),
            Some(_) => return Err("did not complete successfully".into()),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
        }
    }
}

#[cfg(windows)]
fn read_clipboard() -> Result<String, String> {
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-STA",
            "-Command",
            "[Console]::Out.Write((Get-Clipboard -Raw))",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000);
    let output = command
        .output()
        .map_err(|error| format!("could not start the clipboard command: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if error.is_empty() {
            "clipboard command did not complete successfully".into()
        } else {
            error
        })
    }
}

#[cfg(not(windows))]
fn read_clipboard() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    let commands = vec![("pbpaste", Vec::new())];
    #[cfg(not(target_os = "macos"))]
    let commands = vec![
        ("wl-paste", vec!["--no-newline"]),
        ("xclip", vec!["-selection", "clipboard", "-o"]),
        ("xsel", vec!["--clipboard", "--output"]),
    ];

    let mut errors = Vec::new();
    for (program, args) in commands {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            Ok(output) => {
                let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                errors.push(format!(
                    "{program}: {}",
                    if error.is_empty() {
                        "did not complete successfully".to_string()
                    } else {
                        error
                    }
                ));
            }
            Err(error) => errors.push(format!("{program}: could not start ({error})")),
        }
    }
    Err(if errors.is_empty() {
        "no clipboard command available".into()
    } else {
        errors.join("; ")
    })
}

fn wrap_python_output(output: &str) -> Vec<String> {
    output
        .split('\n')
        .flat_map(|line| {
            let chunks = line
                .chars()
                .collect::<Vec<_>>()
                .chunks(PYTHON_CONSOLE_LINE_WIDTH)
                .map(|chunk| chunk.iter().collect::<String>())
                .collect::<Vec<_>>();
            if chunks.is_empty() {
                vec![String::new()]
            } else {
                chunks
            }
        })
        .collect()
}

fn navigate_python_history(pane: &mut PythonPane, older: bool) {
    if pane.history.is_empty() {
        return;
    }
    if older {
        let index = match pane.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                pane.history_draft = python_input_source(pane);
                pane.history.len() - 1
            }
        };
        pane.history_index = Some(index);
        let source = pane.history[index].clone();
        set_python_input_source(pane, &source);
    } else {
        let Some(index) = pane.history_index else {
            return;
        };
        if index + 1 < pane.history.len() {
            let next = index + 1;
            pane.history_index = Some(next);
            let source = pane.history[next].clone();
            set_python_input_source(pane, &source);
        } else {
            pane.history_index = None;
            let draft = std::mem::take(&mut pane.history_draft);
            set_python_input_source(pane, &draft);
        }
    }
}

fn python_input_source(pane: &PythonPane) -> String {
    pane.repl_lines
        .iter()
        .chain(std::iter::once(&pane.input.value))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn set_python_input_source(pane: &mut PythonPane, source: &str) {
    let mut lines = source.split('\n').map(str::to_owned).collect::<Vec<_>>();
    let input = lines.pop().unwrap_or_default();
    pane.repl_lines = lines;
    pane.input.set_value(input);
}

fn python_continuation_indentation(line: &str) -> String {
    let existing = line
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .collect::<String>();
    format!("{existing}    ")
}

fn is_scrollbar_column(area: Rect, column: u16) -> bool {
    area.width >= 2 && area.right().checked_sub(1) == Some(column)
}

fn scrollbar_position_from_row(area: Rect, row: u16, max_position: usize) -> usize {
    if max_position == 0 || area.height < 3 {
        return 0;
    }
    let top = area.y.saturating_add(1);
    let bottom = area.bottom().saturating_sub(2);
    let row = row.clamp(top, bottom);
    let offset = usize::from(row.saturating_sub(top));
    let track = usize::from(bottom.saturating_sub(top)).max(1);
    offset.saturating_mul(max_position) / track
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
    fn copies_selected_bytes_as_continuous_hex() {
        assert_eq!(hex_string(&[0xDE, 0xAD, 0x00, 0xEF]), "DEAD00EF");
    }

    #[test]
    fn python_output_is_chunked_into_scrollable_console_rows() {
        let output = "x".repeat(PYTHON_CONSOLE_LINE_WIDTH * 2 + 1);
        let lines = wrap_python_output(&output);
        assert_eq!(lines.len(), 3);
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() <= PYTHON_CONSOLE_LINE_WIDTH)
        );
    }

    #[test]
    fn python_apply_merges_non_conflicting_changes_and_keeps_hex_edit_conflicts() {
        let path = temporary_file("python-merge.bin");
        fs::write(&path, [3, 4]).unwrap();
        let mut app = App::new("sample.bin".into(), vec![1, 2]);
        app.bytes = Arc::new(vec![9, 2]);
        app.apply_python_snapshot(&PythonSnapshot {
            index: 0,
            path: path.clone(),
            baseline: vec![1, 2],
        });

        assert_eq!(app.bytes.as_slice(), [9, 4]);
        assert!(app.status.contains("conflict"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn new_field_editor_starts_blank_and_uses_the_selection_range() {
        let editor = FieldEditor::new();
        assert!(editor.name.value.is_empty());
        assert!(editor.description.value.is_empty());
        assert!(editor.start.value.is_empty());
        assert!(editor.end.value.is_empty());

        let mut app = App::new("sample.bin".into(), vec![0; 16]);
        app.selection = Some(Selection {
            anchor: 3,
            cursor: 8,
        });
        app.mode = Mode::Field(editor);
        app.commit_field_editor();

        assert_eq!(app.fields[0].name, "field_1");
        assert_eq!((app.fields[0].start, app.fields[0].end), (3, 8));
    }

    #[test]
    fn field_editor_accepts_ctrl_h_as_backspace() {
        let mut editor = FieldEditor::new();
        editor.name = TextInput::with_value("field".into());
        editor.name.clear_selection();
        editor.handle_text_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(editor.name.value, "fiel");
    }

    #[test]
    fn selected_text_input_is_replaced_by_the_first_keystroke() {
        let mut input = TextInput::with_value("existing".into());
        input.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(input.value, "n");

        input.set_value("existing".into());
        input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(input.value, "existin!g");
    }

    #[test]
    fn tab_path_completion_lists_then_cycles_matching_paths() {
        let directory = temporary_file("completion");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("alpha.bin"), []).unwrap();
        fs::write(directory.join("alpine.bin"), []).unwrap();
        let mut input = TextInput::with_value(format!(
            "{}{}al",
            directory.display(),
            std::path::MAIN_SEPARATOR
        ));
        input.clear_selection();
        let mut suggestions = Vec::new();
        let mut active = None;
        let mut scroll = 0;

        Workspace::complete_manual_path(&mut input, &mut suggestions, &mut active, &mut scroll);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(active, None);
        Workspace::complete_manual_path(&mut input, &mut suggestions, &mut active, &mut scroll);
        assert!(input.value.ends_with("alpha.bin"));
        Workspace::complete_manual_path(&mut input, &mut suggestions, &mut active, &mut scroll);
        assert!(input.value.ends_with("alpine.bin"));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn suggestion_navigation_scrolls_past_the_first_page() {
        let suggestions = (0..PATH_SUGGESTION_PAGE_SIZE + 3)
            .map(|index| PathBuf::from(format!("file-{index}")))
            .collect::<Vec<_>>();
        let mut input = TextInput::default();
        let mut active = None;
        let mut scroll = 0;

        Workspace::move_suggestion(
            &mut input,
            &suggestions,
            &mut active,
            &mut scroll,
            PATH_SUGGESTION_PAGE_SIZE as isize,
        );

        assert_eq!(active, Some(PATH_SUGGESTION_PAGE_SIZE));
        assert_eq!(scroll, 1);
        assert!(input.value.ends_with("file-12"));
    }

    #[test]
    fn closing_a_file_preserves_unsaved_changes_until_confirmed() {
        let mut workspace = Workspace::new(vec![App::new("sample.bin".into(), vec![0])]);
        workspace.active_mut().modified_offsets.insert(0);
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(workspace.documents.len(), 1);
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(workspace.documents.is_empty());
    }

    #[test]
    fn vim_navigation_jumps_to_file_boundaries() {
        let mut app = App::new("sample.bin".into(), vec![0; 32]);
        app.select_offset(12, false);
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.selection.unwrap().cursor, 0);

        app.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(app.selection.unwrap().cursor, 31);
    }

    #[test]
    fn open_binary_dialog_accepts_a_manually_typed_path() {
        let path = temporary_file("manual-open.bin");
        fs::write(&path, [0xCA, 0xFE]).unwrap();
        let mut workspace = Workspace::new(Vec::new());
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))
            .unwrap();
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        let Some(OpenFileDialog::ManualPath { input, .. }) = &mut workspace.open_file_dialog else {
            panic!("manual path input should be open");
        };
        input.value = path.display().to_string();
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(workspace.documents.len(), 1);
        assert_eq!(workspace.active().bytes.as_slice(), [0xCA, 0xFE]);
        assert!(workspace.open_file_dialog.is_none());
        fs::remove_file(path).unwrap();
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
    fn automatic_overlay_path_uses_the_user_data_directory_and_content_identity() {
        let app = App::new("sample.bin".into(), vec![0xCA, 0xFE]);
        assert_eq!(
            app.automatic_overlay_path().parent(),
            Some(overlay_storage_dir().as_path())
        );
        assert_eq!(
            app.automatic_overlay_path()
                .file_name()
                .unwrap()
                .to_string_lossy(),
            format!("{}.json", content_identity(&[0xCA, 0xFE]))
        );
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
    fn insert_mode_inserts_bytes_and_undo_redo_restore_file_length() {
        let mut app = App::new("sample.bin".into(), vec![0xAA, 0xBB]);
        app.edit_kind = EditKind::Insert;
        app.insert_nibble(0x1);
        app.insert_nibble(0x2);

        assert_eq!(app.bytes.as_slice(), [0x12, 0xAA, 0xBB]);
        assert!(app.modified_offsets.contains(&2));

        app.undo_overwrite();
        assert_eq!(app.bytes.as_slice(), [0xAA, 0xBB]);

        app.redo_overwrite();
        assert_eq!(app.bytes.as_slice(), [0x12, 0xAA, 0xBB]);
    }

    #[test]
    fn paste_from_clipboard_requires_byte_edit_mode() {
        let mut app = App::new("sample.bin".into(), vec![0xAA, 0xBB]);
        app.edit_mode = false;

        app.paste_from_clipboard();

        assert_eq!(app.bytes.as_slice(), [0xAA, 0xBB]);
        assert!(app.status.contains("Enter Overwrite or Insert Mode"));
    }

    #[test]
    fn pasted_hex_is_inserted_as_a_single_batched_edit() {
        let mut app = App::new("sample.bin".into(), vec![0xAA, 0xBB]);
        app.edit_mode = true;
        app.edit_kind = EditKind::Insert;
        app.selection = Some(Selection::new(1));

        app.paste_hex_bytes("DE AD be ef");

        assert_eq!(app.bytes.as_slice(), [0xAA, 0xDE, 0xAD, 0xBE, 0xEF, 0xBB]);
        assert_eq!(app.undo_stack.len(), 1);

        app.undo_overwrite();
        assert_eq!(app.bytes.as_slice(), [0xAA, 0xBB]);

        app.redo_overwrite();
        assert_eq!(app.bytes.as_slice(), [0xAA, 0xDE, 0xAD, 0xBE, 0xEF, 0xBB]);
    }

    #[test]
    fn pasted_hex_with_a_trailing_nibble_drops_the_incomplete_byte() {
        let mut app = App::new("sample.bin".into(), vec![]);
        app.edit_mode = true;
        app.edit_kind = EditKind::Insert;

        app.paste_hex_bytes("DEAD B");

        assert_eq!(app.bytes.as_slice(), [0xDE, 0xAD]);
        assert!(app.status.contains("trailing hex digit ignored"));
    }

    #[test]
    fn pasted_hex_overwrites_in_place_and_truncates_at_end_of_file() {
        let mut app = App::new("sample.bin".into(), vec![0, 0, 0, 0]);
        app.edit_mode = true;
        app.edit_kind = EditKind::Overwrite;
        app.selection = Some(Selection::new(2));

        app.paste_hex_bytes("DEADBEEF");

        assert_eq!(app.bytes.as_slice(), [0, 0, 0xDE, 0xAD]);
        assert!(app.status.contains("truncated"));

        app.undo_overwrite();
        assert_eq!(app.bytes.as_slice(), [0, 0, 0, 0]);

        app.redo_overwrite();
        assert_eq!(app.bytes.as_slice(), [0, 0, 0xDE, 0xAD]);
    }

    #[test]
    fn pasting_a_large_hex_block_into_a_large_file_stays_fast() {
        // Regression guard for the quadratic path this replaced: feeding each
        // pasted nibble through the one-byte-at-a-time key handler re-shifts
        // and re-scans the whole buffer per byte, which is O(file size *
        // pasted size). With a 2 MiB file and a 100,000-byte paste that would
        // take minutes; the batched path should finish in well under a second.
        let mut app = App::new("sample.bin".into(), vec![0u8; 2_000_000]);
        app.edit_mode = true;
        app.edit_kind = EditKind::Insert;
        app.selection = Some(Selection::new(1_000_000));
        let hex: String = (0..100_000).map(|_| "AB").collect();

        let start = std::time::Instant::now();
        app.paste_hex_bytes(&hex);
        let elapsed = start.elapsed();

        assert_eq!(app.bytes.len(), 2_100_000);
        assert!(
            elapsed < Duration::from_secs(2),
            "batched paste took too long: {elapsed:?}"
        );
    }

    #[test]
    fn workspace_paste_batches_hex_in_byte_edit_mode_but_falls_back_elsewhere() {
        let mut workspace = Workspace::new(vec![App::new("sample.bin".into(), vec![0xAA, 0xBB])]);
        workspace.active_mut().edit_mode = true;
        workspace.active_mut().edit_kind = EditKind::Insert;
        workspace.active_mut().selection = Some(Selection::new(0));

        workspace.handle_workspace_paste("CC").unwrap();
        assert_eq!(workspace.active().bytes.as_slice(), [0xCC, 0xAA, 0xBB]);
        assert_eq!(workspace.active().undo_stack.len(), 1);

        workspace.active_mut().edit_mode = false;
        workspace.active_mut().mode = Mode::Search(TextInput::default());
        workspace.handle_workspace_paste("needle").unwrap();
        let Mode::Search(input) = &workspace.active().mode else {
            panic!("search mode should remain open");
        };
        assert_eq!(input.value, "needle");
    }

    #[test]
    fn deletion_updates_file_length_and_field_offsets() {
        let mut app = App::new("sample.bin".into(), vec![0, 1, 2, 3, 4]);
        app.fields = vec![
            Field {
                name: "before".into(),
                description: String::new(),
                start: 0,
                end: 0,
                color: FieldColor::Cyan,
            },
            Field {
                name: "after".into(),
                description: String::new(),
                start: 3,
                end: 4,
                color: FieldColor::Cyan,
            },
        ];
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 2,
        });
        app.delete_selected_bytes();

        assert_eq!(app.bytes.as_slice(), [0, 3, 4]);
        assert_eq!((app.fields[1].start, app.fields[1].end), (1, 2));

        app.undo_overwrite();
        assert_eq!(app.bytes.as_slice(), [0, 1, 2, 3, 4]);
        assert_eq!((app.fields[1].start, app.fields[1].end), (3, 4));
    }

    #[test]
    fn save_dialog_supports_directory_completion() {
        let directory = temporary_file("save-completion");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("output.bin"), []).unwrap();
        fs::write(directory.join("outline.bin"), []).unwrap();
        let mut app = App::new("sample.bin".into(), vec![0]);
        app.open_path_dialog(PathAction::SaveBinary);
        let Mode::Path(dialog) = &mut app.mode else {
            panic!("save path dialog should be open");
        };
        dialog.input.set_value(format!(
            "{}{}out",
            directory.display(),
            std::path::MAIN_SEPARATOR
        ));
        app.handle_path_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        let Mode::Path(dialog) = &app.mode else {
            panic!("save path dialog should stay open");
        };
        assert_eq!(dialog.suggestions.len(), 2);
        assert!(dialog.suggestions.contains(&directory.join("output.bin")));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn view_mode_supports_undo_redo_and_save() {
        let mut app = App::new("sample.bin".into(), vec![0xAB]);
        app.overwrite_nibble(0x1);
        app.overwrite_nibble(0x2);

        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.bytes[0], 0xAB);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.bytes[0], 0x12);

        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(matches!(
            app.mode,
            Mode::Path(PathDialog {
                action: PathAction::SaveBinary,
                ..
            })
        ));
    }

    #[test]
    fn overwrite_mode_still_opens_the_save_dialog() {
        let mut app = App::new("sample.bin".into(), vec![0xAB]);
        app.edit_mode = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(matches!(
            app.mode,
            Mode::Path(PathDialog {
                action: PathAction::SaveBinary,
                ..
            })
        ));
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
    fn repeated_uniform_rows_are_compressed_and_mapped_to_offsets() {
        let mut bytes = vec![0; 16 * 6];
        bytes.extend([1; 16]);
        let mut app = App::new("sample.bin".into(), bytes);
        app.toggle_setting(5);

        assert_eq!(app.row_count(), 2);
        assert!(matches!(
            app.display_rows[0],
            DisplayRow::Repeated {
                start: 0,
                end: 95,
                byte: 0,
                physical_rows: 6
            }
        ));
        assert_eq!(app.display_row_for_offset(80), 0);
        assert_eq!(app.display_row_for_offset(96), 1);
    }

    #[test]
    fn settings_reset_requires_confirmation() {
        let mut app = App::new("sample.bin".into(), vec![0; 64]);
        app.settings.show_ascii = false;
        app.mode = Mode::Settings(SettingsEditor::default());
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(matches!(
            app.mode,
            Mode::ConfirmReset(ResetTarget::Settings)
        ));

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))
            .unwrap();
        assert!(!app.settings.show_ascii);

        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.settings.show_ascii);
    }

    #[test]
    fn theme_reset_requires_confirmation() {
        let mut app = App::new("sample.bin".into(), vec![0; 16]);
        app.theme.byte_mode = ByteColorMode::ValueBands;
        app.mode = Mode::Theme(ThemeEditor::default());
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.theme, Theme::default());
        assert!(matches!(app.mode, Mode::Theme(_)));
    }

    #[test]
    fn entropy_distinguishes_uniform_and_varied_data() {
        let uniform = entropy::calculate(&vec![0; 1024]);
        let varied = entropy::calculate(&(0..=255).cycle().take(1024).collect::<Vec<_>>());
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
    fn side_by_side_python_console_receives_a_visible_pane() {
        let mut workspace = Workspace::new(vec![
            App::new("one.bin".into(), vec![1, 2]),
            App::new("two.bin".into(), vec![3, 4]),
        ]);
        workspace.side_by_side = true;
        workspace.open_python_pane();
        if !matches!(workspace.active().mode, Mode::Python(_)) {
            return;
        }

        let backend = ratatui::backend::TestBackend::new(160, 48);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| ui::render_workspace(frame, &mut workspace))
            .unwrap();

        assert!(workspace.active().python_area.height > 3);
    }

    #[test]
    fn workspace_shortcuts_close_side_by_side_files_and_hide_entropy() {
        let mut workspace = Workspace::new(vec![
            App::new("one.bin".into(), vec![1]),
            App::new("two.bin".into(), vec![2]),
        ]);
        workspace.side_by_side = true;
        workspace.show_entropy = true;
        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(!workspace.show_entropy);

        workspace
            .handle_workspace_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(workspace.documents.len(), 1);
        assert!(!workspace.side_by_side);
    }

    #[test]
    fn empty_workspace_can_wait_for_a_file_or_quit() {
        let mut workspace = Workspace::new(Vec::new());
        assert!(
            workspace
                .handle_workspace_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
                .unwrap()
        );

        assert!(
            !workspace
                .handle_workspace_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
                .unwrap()
        );
        assert!(workspace.status.contains("Ctrl+N"));
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
    fn separate_selections_are_merged_in_file_order_for_copying() {
        let mut app = App::new("sample.bin".into(), (0..10).collect());
        app.selection = Some(Selection {
            anchor: 7,
            cursor: 8,
        });
        app.additional_selections = vec![Selection {
            anchor: 2,
            cursor: 3,
        }];

        assert_eq!(
            app.selected_ranges(),
            [
                Selection {
                    anchor: 2,
                    cursor: 3
                },
                Selection {
                    anchor: 7,
                    cursor: 8
                }
            ]
        );
        assert_eq!(app.selected_bytes(), [2, 3, 7, 8]);
    }

    #[test]
    fn control_mouse_drag_adds_a_separate_selection() {
        let mut app = App::new("sample.bin".into(), (0..8).collect());
        app.viewer_area = Rect::new(0, 0, 80, 5);
        app.handle_content_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 14,
            row: 1,
            modifiers: KeyModifiers::CONTROL,
        });
        app.handle_content_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 20,
            row: 1,
            modifiers: KeyModifiers::CONTROL,
        });

        assert_eq!(app.additional_selections, [Selection::new(0)]);
        assert_eq!(app.selection.unwrap().start(), 1);
        assert_eq!(app.selection.unwrap().end(), 3);
    }

    #[test]
    fn blank_field_ranges_create_one_field_per_separate_selection() {
        let mut app = App::new("sample.bin".into(), vec![0; 10]);
        app.selection = Some(Selection {
            anchor: 1,
            cursor: 2,
        });
        app.additional_selections = vec![Selection {
            anchor: 6,
            cursor: 7,
        }];
        let mut editor = FieldEditor::new();
        editor.ranges = app.selected_ranges();
        editor.name.set_value("header".into());
        app.mode = Mode::Field(editor);

        app.commit_field_editor();

        assert_eq!(app.fields.len(), 2);
        assert_eq!(app.fields[0].name, "header");
        assert_eq!(app.fields[1].name, "header [2]");
        assert_eq!((app.fields[0].start, app.fields[0].end), (1, 2));
        assert_eq!((app.fields[1].start, app.fields[1].end), (6, 7));
    }

    #[test]
    fn field_rows_follow_the_fields_scroll_offset() {
        let mut app = App::new("sample.bin".into(), vec![0; 32]);
        app.fields = (0..8)
            .map(|index| Field {
                name: format!("field_{index}"),
                description: String::new(),
                start: index,
                end: index,
                color: FieldColor::Cyan,
            })
            .collect();
        app.fields_area = Rect::new(0, 0, 42, 8);
        app.visible_fields = 3;
        app.fields_scroll = 4;

        assert_eq!(app.field_at(1, 1), Some(4));
        assert_eq!(app.field_at(1, 4), None);
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
    fn scrollbar_rows_map_to_the_full_scroll_range() {
        let area = Rect::new(10, 5, 40, 12);
        assert_eq!(scrollbar_position_from_row(area, 6, 100), 0);
        assert_eq!(scrollbar_position_from_row(area, 15, 100), 100);
        assert_eq!(scrollbar_position_from_row(area, 10, 100), 44);
    }

    #[test]
    fn python_history_restores_the_unfinished_draft() {
        let mut app = App::new("sample.bin".into(), vec![0]);
        app.open_python_pane();
        let Mode::Python(pane) = &mut app.mode else {
            return;
        };
        pane.history = vec!["first".into(), "second".into()];
        pane.input.value = "unfinished".into();

        navigate_python_history(pane, true);
        assert_eq!(pane.input.value, "second");
        navigate_python_history(pane, true);
        assert_eq!(pane.input.value, "first");
        navigate_python_history(pane, false);
        assert_eq!(pane.input.value, "second");
        navigate_python_history(pane, false);
        assert_eq!(pane.input.value, "unfinished");
    }

    #[test]
    fn python_history_restores_multiline_blocks_with_indentation() {
        let mut app = App::new("sample.bin".into(), vec![0]);
        app.open_python_pane();
        let Mode::Python(pane) = &mut app.mode else {
            return;
        };
        pane.history = vec!["if ready:\n    process()".into()];

        navigate_python_history(pane, true);

        assert_eq!(pane.repl_lines, ["if ready:"]);
        assert_eq!(pane.input.value, "    process()");
    }

    #[test]
    fn python_repl_auto_indents_after_a_block_header() {
        let mut app = App::new("sample.bin".into(), vec![0]);
        app.open_python_pane();
        let Mode::Python(pane) = &mut app.mode else {
            return;
        };
        pane.input.set_value("if enabled:".into());

        app.handle_python_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Mode::Python(pane) = &app.mode else {
            unreachable!();
        };
        assert_eq!(pane.repl_lines, ["if enabled:"]);
        assert_eq!(pane.input.value, "    ");
    }

    #[test]
    fn blank_python_input_adds_another_prompt_line() {
        let mut app = App::new("sample.bin".into(), vec![0]);
        app.open_python_pane();
        if !matches!(app.mode, Mode::Python(_)) {
            return;
        }
        let previous_length = match &app.mode {
            Mode::Python(pane) => pane.output.len(),
            _ => unreachable!(),
        };

        app.handle_python_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Mode::Python(pane) = &app.mode else {
            unreachable!();
        };
        assert_eq!(pane.output.len(), previous_length + 1);
        assert_eq!(pane.output.last().map(String::as_str), Some(">>>"));
    }

    #[test]
    fn python_history_survives_closing_and_reopening_the_pane() {
        let mut app = App::new("sample.bin".into(), vec![0]);
        app.open_python_pane();
        let Mode::Python(pane) = &mut app.mode else {
            panic!("Python should open");
        };
        pane.history = vec!["first".into(), "second".into()];

        app.handle_python_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.python_history, ["first", "second"]);

        app.open_python_pane();
        let Mode::Python(pane) = &app.mode else {
            panic!("Python should reopen");
        };
        assert_eq!(pane.history, ["first", "second"]);
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
    fn help_window_scrolls_with_the_mouse_wheel() {
        let mut app = App::new("sample.bin".into(), vec![0; 16]);
        app.mode = Mode::Help(HelpViewer::default());
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        let Mode::Help(help) = &app.mode else {
            panic!("help should remain open");
        };
        assert_eq!(help.scroll, 3);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        let Mode::Help(help) = &app.mode else {
            panic!("help should remain open");
        };
        assert_eq!(help.scroll, 0);
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
