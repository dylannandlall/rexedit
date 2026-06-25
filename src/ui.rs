use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Sparkline, Wrap},
};

use crate::app::{
    App, DisplayRow, FieldEditor, Focus, HelpViewer, Mode, PathAction, PathDialog, PythonPane,
    ResetTarget, SettingsEditor, ThemeEditor, Workspace,
};

pub fn render_workspace(frame: &mut Frame, workspace: &mut Workspace) {
    let [tabs, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(frame.area());
    render_tabs(frame, workspace, tabs);

    if workspace.documents.is_empty() {
        workspace.comparison_panes.clear();
        render_empty_workspace(frame, workspace, content);
        return;
    }

    let content = if workspace.show_entropy {
        let [main, entropy] =
            Layout::vertical([Constraint::Percentage(72), Constraint::Percentage(28)])
                .areas(content);
        render_entropy(frame, workspace.active(), entropy);
        main
    } else {
        content
    };

    if workspace.side_by_side && workspace.documents.len() > 1 {
        render_comparison(frame, workspace, content);
        render_mode_modal(frame, &workspace.documents[workspace.active]);
    } else {
        workspace.comparison_panes.clear();
        let active = workspace.active;
        render_in(frame, &mut workspace.documents[active], content);
    }
}

fn render_empty_workspace(frame: &mut Frame, workspace: &Workspace, area: Rect) {
    let area = centered_rect(area, 72, 11);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::styled(
                "No binary is open",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(""),
            Line::from("Press Ctrl+N to choose a binary with the system file picker."),
            Line::from(""),
            Line::styled("q quits", Style::default().fg(Color::DarkGray)),
            Line::from(""),
            Line::styled(&workspace.status, Style::default().fg(Color::Yellow)),
        ])
        .block(
            Block::default()
                .title(" rexedit ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn render_in(frame: &mut Frame, app: &mut App, area: Rect) {
    let [body, status] = Layout::vertical([Constraint::Min(8), Constraint::Length(3)]).areas(area);
    let (body, python_area) = if matches!(app.mode, Mode::Python(_)) {
        let [main, python] =
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);
        (main, Some(python))
    } else {
        (body, None)
    };
    let (viewer, fields, inspector) = if app.settings.show_sidebar {
        let [viewer, sidebar] =
            Layout::horizontal([Constraint::Min(78), Constraint::Length(42)]).areas(body);
        let [fields, inspector] =
            Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
                .areas(sidebar);
        (viewer, fields, inspector)
    } else {
        (body, Rect::default(), Rect::default())
    };

    app.viewer_area = viewer;
    app.fields_area = fields;
    app.python_area = python_area.unwrap_or_default();
    app.visible_rows = viewer.height.saturating_sub(2) as usize;
    app.scroll = app.scroll.min(app.max_scroll());
    if let Mode::Python(pane) = &mut app.mode {
        pane.visible_output_lines = app.python_area.height.saturating_sub(4) as usize;
        pane.clamp_scroll();
    }

    render_viewer(frame, app, viewer, None, None);
    if app.settings.show_sidebar {
        render_fields(frame, app, fields);
        render_inspector(frame, app, inspector);
    }
    if let (Some(area), Mode::Python(pane)) = (python_area, &app.mode) {
        render_python_pane(frame, pane, area, app.focus == Focus::Python);
    }
    render_status(frame, app, status);

    render_mode_modal(frame, app);
}

fn render_mode_modal(frame: &mut Frame, app: &App) {
    match &app.mode {
        Mode::Search(input) => render_input_modal(
            frame,
            " Search bytes ",
            &input.value,
            "Enter starts a background search. Browse normally; n/N visits matches.",
        ),
        Mode::Jump(input) => render_input_modal(
            frame,
            " Jump to offset ",
            &input.value,
            "Enter decimal or 0x-prefixed hexadecimal offset",
        ),
        Mode::Field(editor) => render_field_modal(frame, editor),
        Mode::Path(dialog) => render_path_modal(frame, dialog),
        Mode::Theme(editor) => render_theme_modal(frame, app, editor),
        Mode::Settings(editor) => render_settings_modal(frame, app, editor),
        Mode::ConfirmReset(target) => render_reset_confirmation(frame, *target),
        Mode::Python(_) => {}
        Mode::Help(help) => render_help_modal(frame, help),
        Mode::Normal => {}
    }
}

fn render_tabs(frame: &mut Frame, workspace: &mut Workspace, area: Rect) {
    workspace.tab_row = area.y;
    workspace.tab_hitboxes.clear();
    let mut tab_x = area.x + " binaries ".len() as u16;
    let mut spans = vec![Span::styled(
        " binaries ",
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    for (index, document) in workspace.documents.iter().enumerate() {
        let name = document
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("binary");
        let marker = if !document.modified_offsets.is_empty() {
            "*"
        } else {
            ""
        };
        let style = if index == workspace.active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let label = format!(" {name}{marker} ");
        let end = tab_x.saturating_add(label.chars().count() as u16);
        workspace.tab_hitboxes.push((tab_x, end));
        tab_x = end;
        spans.push(Span::styled(label, style));
    }
    let flags = format!(
        "  {}{}",
        if workspace.side_by_side {
            "SIDE-BY-SIDE "
        } else {
            ""
        },
        if workspace.diff_mode { "DIFF" } else { "" }
    );
    spans.push(Span::styled(flags, Style::default().fg(Color::Yellow)));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_comparison(frame: &mut Frame, workspace: &mut Workspace, area: Rect) {
    let [body, status] = Layout::vertical([Constraint::Min(5), Constraint::Length(2)]).areas(area);
    let constraints =
        vec![Constraint::Ratio(1, workspace.documents.len() as u32); workspace.documents.len()];
    let panes = Layout::horizontal(constraints).split(body);
    workspace.comparison_panes = panes.to_vec();
    let active = workspace.active;
    let comparison = workspace.comparison_index();
    let active_bytes = workspace.documents[active].bytes.clone();
    let active_scroll = workspace.documents[active].scroll;
    let active_row_width = workspace.documents[active].settings.bytes_per_row;
    let active_top_offset = workspace.documents[active]
        .display_rows
        .get(active_scroll)
        .map_or(0, |row| row.start());
    let comparison_bytes = comparison.map(|index| workspace.documents[index].bytes.clone());

    for (index, pane) in panes.iter().enumerate() {
        let diff_reference = if workspace.diff_mode {
            if index == active {
                comparison_bytes.as_deref().map(Vec::as_slice)
            } else {
                Some(active_bytes.as_slice())
            }
        } else {
            None
        };
        let document = &mut workspace.documents[index];
        document.set_bytes_per_row(active_row_width);
        document.viewer_area = *pane;
        document.visible_rows = pane.height.saturating_sub(2) as usize;
        document.scroll = document
            .display_row_for_offset(active_top_offset)
            .min(document.max_scroll());
        let name = document
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("binary")
            .to_owned();
        render_viewer(frame, document, *pane, Some(&name), diff_reference);
    }
    let help = format!(
        "Ctrl+B then Left/Right switch, S comparison | Ctrl+N open | Ctrl+D diff | Ctrl+F search | e entropy | ? keybinds | {}",
        workspace.status
    );
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        status,
    );
}

fn render_entropy(frame: &mut Frame, app: &App, area: Rect) {
    let data = app
        .entropy
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|value| (value * 100.0) as u64)
        .collect::<Vec<_>>();
    let average = if data.is_empty() {
        0.0
    } else {
        data.iter().sum::<u64>() as f64 / data.len() as f64 / 100.0
    };
    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .max(800)
            .style(Style::default().fg(Color::Magenta))
            .block(
                Block::default()
                    .title(format!(
                        " Entropy - {} (average {:.3} bits/byte) ",
                        app.path.display(),
                        average
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Magenta)),
            ),
        area,
    );
}

fn render_viewer(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    label: Option<&str>,
    diff_reference: Option<&[u8]>,
) {
    let end_row = app
        .scroll
        .saturating_add(app.visible_rows)
        .min(app.row_count());
    let mut lines = Vec::with_capacity(end_row.saturating_sub(app.scroll));

    for row in app.scroll..end_row {
        let bytes_per_row = app.settings.bytes_per_row;
        let display_row = app.display_rows[row];
        if let DisplayRow::Repeated {
            start,
            end,
            byte,
            physical_rows,
        } = display_row
        {
            let offset = app.settings.show_offsets.then(|| {
                if app.settings.uppercase_hex {
                    format!("{start:08X}  ")
                } else {
                    format!("{start:08x}  ")
                }
            });
            let end = if app.settings.uppercase_hex {
                format!("{end:08X}")
            } else {
                format!("{end:08x}")
            };
            let byte = if app.settings.uppercase_hex {
                format!("{byte:02X}")
            } else {
                format!("{byte:02x}")
            };
            let mut spans = Vec::new();
            if let Some(offset) = offset {
                spans.push(Span::styled(
                    offset,
                    Style::default().fg(app.theme.offset.color()),
                ));
            }
            let selected_offset =
                app.selection
                    .map(|selection| selection.cursor)
                    .filter(|cursor| {
                        (start..=display_row.end(bytes_per_row, app.bytes.len())).contains(cursor)
                    });
            let summary = if let Some(cursor) = selected_offset {
                format!(
                    "… {physical_rows} identical rows of {byte} compressed (through {end}; cursor {cursor:08X}) …"
                )
            } else {
                format!("… {physical_rows} identical rows of {byte} compressed (through {end}) …")
            };
            let mut style = Style::default()
                .fg(app.theme.hex_secondary.color())
                .add_modifier(Modifier::ITALIC);
            if selected_offset.is_some() {
                style = style
                    .bg(app.theme.selection_background.color())
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(summary, style));
            lines.push(Line::from(spans));
            continue;
        }
        let offset = display_row.start();
        let chunk = &app.bytes[offset..(offset + bytes_per_row).min(app.bytes.len())];
        let mut spans = Vec::new();
        if app.settings.show_offsets {
            let text = if app.settings.uppercase_hex {
                format!("{offset:08X}  ")
            } else {
                format!("{offset:08x}  ")
            };
            spans.push(Span::styled(
                text,
                Style::default().fg(app.theme.offset.color()),
            ));
        }

        for index in 0..bytes_per_row {
            if index > 0 && index.is_multiple_of(8) {
                spans.push(Span::raw(" "));
            }
            if let Some(byte) = chunk.get(index) {
                let absolute = offset + index;
                let text = if app.settings.uppercase_hex {
                    format!("{byte:02X} ")
                } else {
                    format!("{byte:02x} ")
                };
                spans.push(Span::styled(
                    text,
                    byte_style_with_diff(app, absolute, *byte, false, diff_reference),
                ));
            } else {
                spans.push(Span::raw("   "));
            }
        }

        if app.settings.show_ascii {
            spans.push(Span::styled(
                " |",
                Style::default().fg(app.theme.offset.color()),
            ));
            for (index, byte) in chunk.iter().enumerate() {
                let character = if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '.'
                };
                spans.push(Span::styled(
                    character.to_string(),
                    byte_style_with_diff(app, offset + index, *byte, true, diff_reference),
                ));
            }
        }
        lines.push(Line::from(spans));
    }

    let mode_name = if app.edit_mode {
        "Overwrite Mode"
    } else {
        "View Mode"
    };
    let title = label.map_or_else(
        || format!(" Hex Viewer - {mode_name} "),
        |label| format!(" {label} - {mode_name} "),
    );
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(Span::styled(title, viewer_title_style(app)))
                .borders(Borders::ALL)
                .border_style(viewer_border_style(app)),
        ),
        area,
    );
}

fn byte_style_with_diff(
    app: &App,
    offset: usize,
    byte: u8,
    ascii: bool,
    diff_reference: Option<&[u8]>,
) -> Style {
    let mut style = byte_style(app, offset, byte, ascii);
    if diff_reference.is_some_and(|reference| reference.get(offset) != Some(&byte)) {
        style = style
            .bg(Color::Red)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
    }
    style
}

fn byte_style(app: &App, offset: usize, byte: u8, ascii: bool) -> Style {
    let mut style = Style::default().fg(app.theme_color_for_byte(offset, byte, ascii).color());

    if let Some(field) = app.fields.iter().find(|field| field.contains(offset)) {
        style = style
            .fg(field.color.color())
            .add_modifier(Modifier::UNDERLINED);
    }
    if app.modified_offsets.contains(&offset) {
        style = style
            .fg(app.theme.modified.color())
            .add_modifier(Modifier::BOLD);
    }
    if app.is_search_match(offset) {
        style = style
            .bg(app.theme.search_background.color())
            .add_modifier(Modifier::BOLD);
    }
    if app
        .selection
        .is_some_and(|selection| selection.contains(offset))
    {
        style = style
            .bg(app.theme.selection_background.color())
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
    }
    style
}

fn render_fields(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    let list_height = area.height.saturating_sub(6) as usize;
    for (index, field) in app.fields.iter().take(list_height.max(1)).enumerate() {
        let selected = index == app.selected_field;
        let marker = if selected { ">" } else { " " };
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(field.color.color())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(field.color.color())
        };
        lines.push(Line::from(Span::styled(
            format!(
                "{marker} {:<16} {:08X}-{:08X}",
                truncate(&field.name, 16),
                field.start,
                field.end
            ),
            style,
        )));
    }
    if app.fields.is_empty() {
        lines.push(Line::styled(
            "No fields. Select bytes and press a.",
            Style::default().fg(Color::DarkGray),
        ));
    } else if let Some(field) = app.fields.get(app.selected_field) {
        lines.extend([
            Line::from(""),
            Line::from(vec![
                Span::styled("Color: ", Style::default().fg(Color::DarkGray)),
                Span::styled(field.color.name(), Style::default().fg(field.color.color())),
            ]),
            Line::from(vec![
                Span::styled("Description: ", Style::default().fg(Color::DarkGray)),
                Span::raw(truncate(&field.description, 25)),
            ]),
        ]);
    }

    let active = app.focus == Focus::Fields;
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(
                    " Fields [a add, Enter edit, d delete] ",
                    title_style(app, active),
                ))
                .borders(Borders::ALL)
                .border_style(border_style(app, active)),
        ),
        area,
    );
}

fn render_inspector(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(
        Paragraph::new(inspector_lines(app))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(Span::styled(" Inspector ", title_style(app, false)))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.border.color())),
            ),
        area,
    );
}

fn inspector_lines(app: &App) -> Vec<Line<'static>> {
    let Some(selection) = app.selection else {
        return vec![Line::from("Empty file")];
    };
    let bytes = app.current_bytes();
    let preview = bytes.iter().take(16).copied().collect::<Vec<_>>();
    let hex = preview
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let ascii: String = preview
        .iter()
        .map(|byte| {
            if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            }
        })
        .collect();
    let binary = preview
        .iter()
        .take(4)
        .map(|byte| format!("{byte:08b}"))
        .collect::<Vec<_>>()
        .join(" ");

    let mut lines = vec![
        kv(
            "Range",
            format!("0x{:X}..=0x{:X}", selection.start(), selection.end()),
        ),
        kv("Length", format!("{} bytes", selection.len())),
        kv("Hex", hex),
        kv("ASCII", ascii),
        kv("Binary", binary),
    ];
    if let Some(byte) = bytes.first() {
        lines.push(kv("u8 / i8", format!("{byte} / {}", *byte as i8)));
    }
    add_integer_lines(&mut lines, bytes);
    add_float_lines(&mut lines, bytes);
    if !bytes.is_empty() {
        lines.push(kv("UTF-8", utf8_preview(bytes)));
    }
    lines
}

fn add_integer_lines(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.len() >= 2 {
        let value = [bytes[0], bytes[1]];
        lines.push(kv(
            "u16 LE/BE",
            format!(
                "{} / {}",
                u16::from_le_bytes(value),
                u16::from_be_bytes(value)
            ),
        ));
        lines.push(kv(
            "i16 LE/BE",
            format!(
                "{} / {}",
                i16::from_le_bytes(value),
                i16::from_be_bytes(value)
            ),
        ));
    }
    if bytes.len() >= 4 {
        let value: [u8; 4] = bytes[..4].try_into().expect("checked length");
        lines.push(kv(
            "u32 LE/BE",
            format!(
                "{} / {}",
                u32::from_le_bytes(value),
                u32::from_be_bytes(value)
            ),
        ));
        lines.push(kv(
            "i32 LE/BE",
            format!(
                "{} / {}",
                i32::from_le_bytes(value),
                i32::from_be_bytes(value)
            ),
        ));
    }
    if bytes.len() >= 8 {
        let value: [u8; 8] = bytes[..8].try_into().expect("checked length");
        lines.push(kv(
            "u64 LE/BE",
            format!(
                "{} / {}",
                u64::from_le_bytes(value),
                u64::from_be_bytes(value)
            ),
        ));
        lines.push(kv(
            "i64 LE/BE",
            format!(
                "{} / {}",
                i64::from_le_bytes(value),
                i64::from_be_bytes(value)
            ),
        ));
    }
}

fn add_float_lines(lines: &mut Vec<Line<'static>>, bytes: &[u8]) {
    if bytes.len() >= 4 {
        let value: [u8; 4] = bytes[..4].try_into().expect("checked length");
        lines.push(kv(
            "f32 LE/BE",
            format!(
                "{:.6} / {:.6}",
                f32::from_le_bytes(value),
                f32::from_be_bytes(value)
            ),
        ));
    }
    if bytes.len() >= 8 {
        let value: [u8; 8] = bytes[..8].try_into().expect("checked length");
        lines.push(kv(
            "f64 LE/BE",
            format!(
                "{:.6} / {:.6}",
                f64::from_le_bytes(value),
                f64::from_be_bytes(value)
            ),
        ));
    }
}

fn kv(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<11}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value),
    ])
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let dirty = if app.modified_offsets.is_empty() {
        String::new()
    } else {
        format!(" | {} modified", app.modified_offsets.len())
    };
    let search = if app.search.running {
        let percent = app
            .search
            .scanned
            .saturating_mul(100)
            .checked_div(app.search.total)
            .unwrap_or(100);
        format!(
            " | searching {percent}% ({} matches)",
            app.search.results.len()
        )
    } else if app.search.results.is_empty() {
        String::new()
    } else if !app.search.has_active_result {
        format!(" | {} matches (n/N to navigate)", app.search.results.len())
    } else {
        format!(
            " | match {}/{}",
            app.search.current + 1,
            app.search.results.len()
        )
    };
    let mode = if app.edit_mode {
        " | Overwrite Mode"
    } else {
        " | View Mode"
    };
    let first = Line::from(vec![
        Span::styled(
            format!(" {} ", app.path.display()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "| {} bytes | {} fields{dirty}{search}{mode}",
            app.bytes.len(),
            app.fields.len(),
        )),
    ]);
    let second = Line::styled(&app.status, Style::default().fg(Color::Cyan));
    let help = if app.edit_mode {
        "Ctrl+B then Left/Right binary | hex overwrite | Ctrl+U/R undo/redo | Ctrl+S save | Esc View Mode | ? keybinds"
    } else {
        "Ctrl+B then Left/Right binary, S compare | Ctrl+N open | Ctrl+F search | Ctrl+G jump | i edit | ? keybinds"
    };
    let third = Line::styled(help, Style::default().fg(Color::DarkGray));
    frame.render_widget(Paragraph::new(vec![first, second, third]), area);
}

fn render_input_modal(frame: &mut Frame, title: &str, value: &str, help: &str) {
    let area = centered_rect(frame.area(), 78, 7);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            input_line(value),
            Line::from(""),
            Line::styled(help, Style::default().fg(Color::DarkGray)),
        ])
        .block(
            Block::default()
                .title(Span::styled(title, modal_title_style()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn render_path_modal(frame: &mut Frame, dialog: &PathDialog) {
    let (title, help) = match dialog.action {
        PathAction::SaveOverlay => (
            " Save overlay ",
            "Choose a writable JSON file path. Parent directories must already exist.",
        ),
        PathAction::LoadOverlay => (
            " Load overlay ",
            "Enter the path to an existing overlay JSON file.",
        ),
        PathAction::SaveBinary => (
            " Save binary ",
            "Save the in-memory byte changes here. This may overwrite an existing file.",
        ),
        PathAction::SaveTheme => (" Save theme ", "Save this custom theme as JSON."),
        PathAction::LoadTheme => (" Load theme ", "Enter the path to a theme JSON file."),
    };
    render_input_modal(frame, title, &dialog.input.value, help);
}

fn input_line(value: &str) -> Line<'static> {
    Line::styled(
        format!(" {value}"),
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

fn render_field_modal(frame: &mut Frame, editor: &FieldEditor) {
    let area = centered_rect(frame.area(), 72, 13);
    frame.render_widget(Clear, area);
    let rows = [
        ("Name", editor.name.as_str()),
        ("Description", editor.description.as_str()),
        ("Start", editor.start.as_str()),
        ("End", editor.end.as_str()),
        ("Color", editor.color.name()),
    ];
    let lines = rows
        .iter()
        .enumerate()
        .map(|(index, (label, value))| {
            let style = selected_row(index == editor.active);
            Line::styled(format!(" {label:<12} {value}"), style)
        })
        .chain([
            Line::from(""),
            Line::styled(
                "Tab/Up/Down field | Left/Right color | Enter save | Esc cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ])
        .collect::<Vec<_>>();
    let title = if editor.editing.is_some() {
        " Edit field "
    } else {
        " Add field "
    };
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(title, modal_title_style()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn render_theme_modal(frame: &mut Frame, app: &App, editor: &ThemeEditor) {
    let area = centered_rect(frame.area(), 70, 17);
    frame.render_widget(Clear, area);
    let rows = [
        ("Name", app.theme.name.as_str()),
        ("Byte pattern", app.theme.byte_mode.name()),
        ("Hex primary", app.theme.hex_primary.name()),
        ("Hex secondary", app.theme.hex_secondary.name()),
        ("ASCII", app.theme.ascii.name()),
        ("Offsets", app.theme.offset.name()),
        ("Borders", app.theme.border.name()),
        ("Selection bg", app.theme.selection_background.name()),
        ("Search bg", app.theme.search_background.name()),
        ("Modified bytes", app.theme.modified.name()),
    ];
    let mut lines = rows
        .iter()
        .enumerate()
        .map(|(index, (label, value))| {
            Line::styled(
                format!(" {label:<17} {value}"),
                selected_row(index == editor.active),
            )
        })
        .collect::<Vec<_>>();
    lines.extend([
        Line::from(""),
        Line::styled(
            "Arrows change | type name | Ctrl+S/L save/load | Ctrl+R reset | Enter/Esc close",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(" Theme customization ", modal_title_style()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn render_settings_modal(frame: &mut Frame, app: &App, editor: &SettingsEditor) {
    let area = centered_rect(frame.area(), 70, 14);
    frame.render_widget(Clear, area);
    let enabled = |value| if value { "enabled" } else { "disabled" };
    let rows = [
        ("ASCII column", enabled(app.settings.show_ascii).to_string()),
        ("Bytes per row", app.settings.bytes_per_row.to_string()),
        (
            "Uppercase hex",
            enabled(app.settings.uppercase_hex).to_string(),
        ),
        (
            "Offset column",
            enabled(app.settings.show_offsets).to_string(),
        ),
        ("Side panes", enabled(app.settings.show_sidebar).to_string()),
        (
            "Compress repeated rows",
            enabled(app.settings.compress_repeated_rows).to_string(),
        ),
    ];
    let mut lines = rows
        .iter()
        .enumerate()
        .map(|(index, (label, value))| {
            Line::styled(
                format!(" {label:<20} {value}"),
                selected_row(index == editor.active),
            )
        })
        .collect::<Vec<_>>();
    lines.extend([
        Line::from(""),
        Line::styled(
            "Up/Down select | Left/Right/Space change | Ctrl+R reset | Enter/Esc close",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(" View Mode Settings ", modal_title_style()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

fn render_reset_confirmation(frame: &mut Frame, target: ResetTarget) {
    let name = match target {
        ResetTarget::Theme => "theme",
        ResetTarget::Settings => "viewer settings",
    };
    let area = centered_rect(frame.area(), 62, 7);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::styled(
                format!(" Reset {name} to defaults?"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::from(""),
            Line::styled(
                " Type y to confirm or n to cancel.",
                Style::default().fg(Color::Yellow),
            ),
        ])
        .block(
            Block::default()
                .title(Span::styled(" Confirm reset ", modal_title_style()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        area,
    );
}

fn render_python_pane(frame: &mut Frame, pane: &PythonPane, area: Rect, active: bool) {
    let output_height = area.height.saturating_sub(4) as usize;
    let (start, end) = python_output_range(pane.output.len(), output_height, pane.scroll);
    let mut lines = pane.output[start..end]
        .iter()
        .map(|line| Line::raw(line.clone()))
        .collect::<Vec<_>>();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(">>> ", Style::default().fg(Color::LightGreen)),
        Span::raw(&pane.input.value),
        if pane.pending > 0 {
            Span::styled(
                format!("  [{} running]", pane.pending),
                Style::default().fg(Color::Yellow),
            )
        } else {
            Span::raw("")
        },
    ]));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(Span::styled(
                    " Python console — Tab changes pane | PgUp/PgDn scroll | Enter run | Esc close ",
                    if active {
                        Style::default()
                            .fg(Color::LightGreen)
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                    } else {
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD)
                    },
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if active {
                    Color::LightGreen
                } else {
                    Color::Green
                })),
        ),
        area,
    );
}

fn python_output_range(length: usize, height: usize, scroll: usize) -> (usize, usize) {
    let max_scroll = length.saturating_sub(height);
    let end = length.saturating_sub(scroll.min(max_scroll));
    (end.saturating_sub(height), end)
}

fn render_help_modal(frame: &mut Frame, help: &HelpViewer) {
    let area = centered_rect(
        frame.area(),
        92,
        frame.area().height.saturating_sub(4).min(34),
    );
    frame.render_widget(Clear, area);
    let lines = keybinding_lines();
    let visible_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible_height);
    let scroll = help.scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(
                Block::default()
                    .title(Span::styled(
                        " Keybindings - Up/Down or PgUp/PgDn scroll; Esc/?/q closes ",
                        modal_title_style(),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
        area,
    );
}

fn keybinding_lines() -> Vec<Line<'static>> {
    let section = |title| {
        Line::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    };
    let binding = |keys: &'static str, action: &'static str| {
        Line::from(vec![
            Span::styled(format!("  {keys:<24}"), Style::default().fg(Color::Cyan)),
            Span::raw(action),
        ])
    };

    vec![
        section("Workspace"),
        binding("Ctrl+B, then Right", "activate the next binary"),
        binding("Ctrl+B, then Left", "activate the previous binary"),
        binding("Ctrl+B, then S", "toggle side-by-side comparison"),
        binding("Ctrl+N", "open another binary with the system picker"),
        binding("Ctrl+D", "toggle byte diff mode"),
        binding("Ctrl+Z", "suspend on Unix; resume with shell fg"),
        binding("e", "toggle the active binary's entropy graph"),
        binding("mouse on tab/pane", "activate that binary"),
        Line::from(""),
        section("View Mode"),
        binding("arrows", "move the byte cursor"),
        binding("Shift+arrows", "extend the byte selection"),
        binding("Page Up / Page Down", "move by one visible page"),
        binding("Home / End", "jump to the start / end of the file"),
        binding("mouse drag", "select a range of bytes"),
        binding("mouse wheel", "scroll the hex viewer"),
        binding("i", "enter Overwrite Mode"),
        binding("Ctrl+F", "open byte-pattern search"),
        binding("n / N", "next / previous search result"),
        binding("Ctrl+Down / Ctrl+Up", "next / previous search result"),
        binding("Ctrl+G", "jump to a decimal or hexadecimal offset"),
        binding("a", "create a field from the current selection"),
        binding("Tab", "switch between viewer and fields pane"),
        binding("Enter", "edit the selected field"),
        binding("d", "delete the selected field"),
        binding("[ / ]", "select previous / next field"),
        binding("Ctrl+O / Ctrl+L", "save / load a field overlay"),
        binding("s", "open viewer settings"),
        binding("t", "open theme customization"),
        binding("p", "open the Python buffer console"),
        Line::from(""),
        section("Overwrite Mode"),
        binding("0-9, A-F", "overwrite the selected byte, two nibbles"),
        binding(
            "arrows / Page Up/Down",
            "navigate without leaving overwrite mode",
        ),
        binding("Ctrl+U", "undo the previous byte overwrite"),
        binding("Ctrl+R", "redo the previous byte overwrite"),
        binding("Ctrl+S", "save the edited binary"),
        binding("Escape", "return to View Mode"),
        Line::from(""),
        section("Search syntax"),
        binding("DE AD BE EF", "spaced hexadecimal bytes"),
        binding("0xDEADBEEF", "compact hexadecimal bytes"),
        binding("hex: DE ?? BE", "hexadecimal with wildcard bytes"),
        binding("dec: 65535", "unsigned decimal value"),
        binding("bin: 01001101", "binary bytes"),
        binding("re:\\x4D\\x5A.", "byte regular expression"),
        Line::from(""),
        section("Dialogs, themes, and settings"),
        binding("Tab / Up / Down", "move between dialog options"),
        binding("Left / Right / Space", "change the selected option"),
        binding("Ctrl+U", "clear a text input"),
        binding("Enter", "confirm or close"),
        binding("Escape", "cancel or close"),
        binding("Ctrl+S / Ctrl+L", "save / load inside the theme menu"),
        binding("Ctrl+R", "reset theme or settings after y/n confirmation"),
        Line::from(""),
        section("Python console"),
        binding("Enter", "execute an expression or statement"),
        binding("Tab / Shift+Tab", "cycle viewer, fields, and Python panes"),
        binding(":apply", "copy same-length buffer edits into rexedit"),
        binding("PgUp/PgDn / wheel", "scroll console output"),
        binding("Ctrl+Home / Ctrl+End", "oldest / newest console output"),
        binding("Ctrl+L", "clear console output"),
        binding("Escape", "close the Python pane"),
        Line::from(""),
        section("General"),
        binding("?", "open this keybinding reference"),
        binding("q", "quit; press twice when edits are unsaved"),
    ]
}

fn selected_row(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn modal_title_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

fn title_style(app: &App, active: bool) -> Style {
    let style = Style::default()
        .fg(app.theme.hex_secondary.color())
        .add_modifier(Modifier::BOLD);
    if active {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        style
    }
}

fn viewer_title_style(app: &App) -> Style {
    if app.edit_mode {
        Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        title_style(app, app.focus == Focus::Viewer)
    }
}

fn viewer_border_style(app: &App) -> Style {
    if app.edit_mode {
        Style::default().fg(Color::LightRed)
    } else {
        border_style(app, app.focus == Focus::Viewer)
    }
}

fn border_style(app: &App, active: bool) -> Style {
    Style::default().fg(if active {
        app.theme.hex_secondary.color()
    } else {
        app.theme.border.color()
    })
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut output: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars && max_chars > 0 {
        output.pop();
        output.push('…');
    }
    output
}

fn utf8_preview(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 4096;
    const MAX_CHARS: usize = 256;

    let inspected = &bytes[..bytes.len().min(MAX_BYTES)];
    let decoded = String::from_utf8_lossy(inspected);
    let mut preview: String = decoded.chars().take(MAX_CHARS).collect();
    if decoded.chars().count() > MAX_CHARS {
        preview.pop();
        preview.push('…');
    }
    if bytes.len() > MAX_BYTES {
        preview.push_str(&format!(" … (+{} bytes)", bytes.len() - MAX_BYTES));
    }
    preview
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn truncates_long_labels() {
        assert_eq!(truncate("abcdefghijkl", 6), "abcde…");
        assert_eq!(truncate("abc", 6), "abc");
    }

    #[test]
    fn utf8_preview_tracks_long_and_invalid_selections() {
        let mut bytes = vec![b'a'; 5000];
        bytes[10] = 0xFF;
        let preview = utf8_preview(&bytes);
        assert!(preview.contains('�'));
        assert!(preview.contains("+904 bytes"));
    }

    #[test]
    fn python_output_range_scrolls_back_through_history() {
        assert_eq!(python_output_range(100, 20, 0), (80, 100));
        assert_eq!(python_output_range(100, 20, 10), (70, 90));
        assert_eq!(python_output_range(8, 20, usize::MAX), (0, 8));
        assert_eq!(python_output_range(100, 20, usize::MAX), (0, 20));
    }

    #[test]
    fn renders_the_complete_workspace() {
        let backend = TestBackend::new(130, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(PathBuf::from("sample.bin"), (0..=127).collect());
        terminal
            .draw(|frame| render_in(frame, &mut app, frame.area()))
            .unwrap();
        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(rendered.contains("Hex Viewer - View Mode"));
        assert!(rendered.contains("Inspector"));
        assert!(rendered.contains("Fields"));
    }

    #[test]
    fn renders_empty_workspace_file_opening_prompt() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut workspace = Workspace::new(Vec::new());
        terminal
            .draw(|frame| render_workspace(frame, &mut workspace))
            .unwrap();
        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(rendered.contains("No binary is open"));
        assert!(rendered.contains("Ctrl+N"));
    }

    #[test]
    fn renders_side_by_side_diff_and_entropy() {
        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut workspace = Workspace::new(vec![
            App::new(PathBuf::from("one.bin"), vec![0; 512]),
            App::new(PathBuf::from("two.bin"), vec![1; 512]),
        ]);
        workspace.side_by_side = true;
        workspace.diff_mode = true;
        workspace.show_entropy = true;
        workspace.active_mut().entropy_profile();
        terminal
            .draw(|frame| render_workspace(frame, &mut workspace))
            .unwrap();

        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(rendered.contains("one.bin"));
        assert!(rendered.contains("two.bin"));
        assert!(rendered.contains("DIFF"));
        assert!(rendered.contains("Entropy"));
    }

    #[test]
    fn dragging_a_large_selection_in_a_comparison_pane_does_not_panic() {
        let backend = TestBackend::new(180, 42);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut workspace = Workspace::new(vec![
            App::new(PathBuf::from("one.bin"), vec![0; 4096]),
            App::new(PathBuf::from("two.bin"), vec![1; 4096]),
        ]);
        workspace.side_by_side = true;
        terminal
            .draw(|frame| render_workspace(frame, &mut workspace))
            .unwrap();

        let pane = workspace.comparison_panes[1];
        workspace.handle_workspace_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: pane.x + 12,
            row: pane.y + 1,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        workspace.handle_workspace_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: pane.x + 50,
            row: pane.bottom().saturating_sub(2),
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        workspace.handle_workspace_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: pane.x + 50,
            row: pane.bottom().saturating_sub(2),
            modifiers: crossterm::event::KeyModifiers::NONE,
        });

        workspace.side_by_side = false;
        terminal
            .draw(|frame| render_workspace(frame, &mut workspace))
            .unwrap();
        assert_eq!(workspace.active, 1);
        assert!(workspace.active().selection.unwrap().len() > 1);
    }

    #[test]
    fn renders_complete_keybinding_help() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut workspace =
            Workspace::new(vec![App::new(PathBuf::from("sample.bin"), vec![0; 64])]);
        workspace.active_mut().mode = Mode::Help(HelpViewer::default());
        terminal
            .draw(|frame| render_workspace(frame, &mut workspace))
            .unwrap();
        let rendered =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(rendered.contains("Keybindings"));
        assert!(rendered.contains("Ctrl+F"));
        assert!(rendered.contains("Overwrite Mode"));
    }
}
