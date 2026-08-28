use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Sparkline, Wrap,
    },
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::{
    App, DisplayRow, FieldEditor, Focus, HelpViewer, Mode, OpenFileDialog,
    PATH_SUGGESTION_PAGE_SIZE, PathAction, PathDialog, PythonPane, ResetTarget, SettingsEditor,
    ThemeEditor, Workspace,
};

// A 16-byte row with offsets and ASCII needs 77 inner columns. Leave room for
// both viewer borders so the sidebar border never overwrites the final ASCII
// character when the terminal is narrowed.
const VIEWER_MIN_WIDTH: u16 = 79;
const SIDEBAR_WIDTH: u16 = 41;

pub fn render_workspace(frame: &mut Frame, workspace: &mut Workspace) {
    let [tabs, content] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(frame.area());
    render_tabs(frame, workspace, tabs);

    if workspace.documents.is_empty() {
        workspace.comparison_panes.clear();
        render_empty_workspace(frame, workspace, content);
        if let Some(dialog) = &workspace.open_file_dialog {
            render_open_file_modal(frame, dialog);
        }
        return;
    }

    let content = if workspace.show_entropy {
        let [main, entropy] =
            Layout::vertical([Constraint::Percentage(72), Constraint::Percentage(28)])
                .areas(content);
        render_workspace_entropy(frame, workspace, entropy);
        main
    } else {
        content
    };

    if workspace.side_by_side && workspace.documents.len() > 1 {
        let active = workspace.active;
        let (comparison, python_area) =
            if matches!(workspace.documents[active].mode, Mode::Python(_)) {
                let [comparison, python] =
                    Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                        .areas(content);
                (comparison, Some(python))
            } else {
                (content, None)
            };
        let show_sidebar = workspace.documents[active].settings.show_sidebar;
        let (comparison, sidebar) = if show_sidebar {
            let (comparison, sidebar) = split_viewer_and_sidebar(comparison);
            (comparison, Some(sidebar))
        } else {
            (comparison, None)
        };
        render_comparison(frame, workspace, comparison);
        if let Some(sidebar) = sidebar {
            let [fields, inspector] =
                Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .areas(sidebar);
            let app = &mut workspace.documents[active];
            app.fields_area = fields;
            app.visible_fields = fields.height.saturating_sub(5).max(1) as usize;
            app.fields_scroll = app.fields_scroll.min(app.field_max_scroll());
            render_fields(frame, app, fields);
            render_inspector(frame, app, inspector);
        } else {
            workspace.documents[active].fields_area = Rect::default();
        }
        if let Some(area) = python_area {
            let app = &mut workspace.documents[active];
            app.python_area = area;
            if let Mode::Python(pane) = &mut app.mode {
                pane.visible_output_lines = area.height.saturating_sub(4) as usize;
                pane.clamp_scroll();
                render_python_pane(frame, pane, area, app.focus == Focus::Python);
            }
        }
        render_mode_modal(frame, &workspace.documents[workspace.active]);
    } else {
        workspace.comparison_panes.clear();
        let active = workspace.active;
        render_in(frame, &mut workspace.documents[active], content);
    }
    if let Some(dialog) = &workspace.open_file_dialog {
        render_open_file_modal(frame, dialog);
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
            Line::from("Press Ctrl+N to choose a system picker or type a binary path."),
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
        let (viewer, sidebar) = split_viewer_and_sidebar(body);
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
    app.visible_fields = fields.height.saturating_sub(5).max(1) as usize;
    app.fields_scroll = app.fields_scroll.min(app.field_max_scroll());
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

fn split_viewer_and_sidebar(area: Rect) -> (Rect, Rect) {
    let [viewer, sidebar] = Layout::horizontal([
        Constraint::Min(VIEWER_MIN_WIDTH),
        Constraint::Length(SIDEBAR_WIDTH),
    ])
    .areas(area);
    // Keep the sidebar's lower edge exactly level with the viewer. This also
    // protects against future layout changes that reserve space below only one
    // of the two panes.
    let sidebar = Rect::new(sidebar.x, viewer.y, sidebar.width, viewer.height);
    (viewer, sidebar)
}

fn render_mode_modal(frame: &mut Frame, app: &App) {
    match &app.mode {
        Mode::Search(input) => render_input_modal(
            frame,
            " Search bytes ",
            input,
            "Hex (DE AD/0xDEAD), dec:, bin:, or re: | Enter search | n/N matches",
        ),
        Mode::Jump(input) => render_input_modal(
            frame,
            " Jump to offset ",
            input,
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
        document.viewer_area = *pane;
        document.visible_rows = pane.height.saturating_sub(2) as usize;
        document.scroll = document.scroll.min(document.max_scroll());
        let name = document
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("binary")
            .to_owned();
        render_viewer(frame, document, *pane, Some(&name), diff_reference);
    }
    let help = format!(
        "Ctrl+B then Left/Right switch, S comparison | Ctrl+N open | Ctrl+W close | Ctrl+D diff | e/Esc entropy | ? keybinds | {}",
        workspace.status
    );
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        status,
    );
}

fn render_workspace_entropy(frame: &mut Frame, workspace: &Workspace, area: Rect) {
    if workspace.side_by_side && workspace.documents.len() > 1 {
        if workspace.diff_mode {
            let active = workspace.active;
            let comparison = workspace.comparison_index().unwrap_or(active);
            let [active_area, diff_area] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(area);
            render_entropy(frame, &workspace.documents[active], active_area);
            render_entropy_difference(
                frame,
                &workspace.documents[active],
                &workspace.documents[comparison],
                diff_area,
            );
        } else {
            let constraints = vec![
                Constraint::Ratio(1, workspace.documents.len() as u32);
                workspace.documents.len()
            ];
            let areas = Layout::horizontal(constraints).split(area);
            for (document, area) in workspace.documents.iter().zip(areas.iter()) {
                render_entropy(frame, document, *area);
            }
        }
    } else {
        render_entropy(frame, workspace.active(), area);
    }
}

fn render_entropy(frame: &mut Frame, app: &App, area: Rect) {
    if app.entropy.is_none() {
        let percent = app
            .entropy_scanned
            .saturating_mul(100)
            .checked_div(app.entropy_total)
            .unwrap_or(0);
        frame.render_widget(
            Paragraph::new(format!(" Calculating entropy… {percent}% ")).block(
                Block::default()
                    .title(format!(" Entropy - {} ", app.path.display()))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
            area,
        );
        return;
    }
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

fn render_entropy_difference(frame: &mut Frame, active: &App, comparison: &App, area: Rect) {
    let (Some(active_profile), Some(comparison_profile)) =
        (active.entropy.as_deref(), comparison.entropy.as_deref())
    else {
        frame.render_widget(
            Paragraph::new(" Calculating both entropy profiles before showing the difference… ")
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title(" Entropy difference ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                ),
            area,
        );
        return;
    };
    let points = active_profile.len().max(comparison_profile.len()).max(1);
    let data = (0..points)
        .map(|index| {
            let active_index =
                index * active_profile.len().saturating_sub(1) / points.saturating_sub(1).max(1);
            let comparison_index = index * comparison_profile.len().saturating_sub(1)
                / points.saturating_sub(1).max(1);
            let active_value = active_profile.get(active_index).copied().unwrap_or(0.0);
            let comparison_value = comparison_profile
                .get(comparison_index)
                .copied()
                .unwrap_or(0.0);
            ((active_value - comparison_value).abs() * 100.0) as u64
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .max(800)
            .style(Style::default().fg(Color::LightRed))
            .block(
                Block::default()
                    .title(format!(
                        " Entropy difference - {} vs {} ",
                        active.path.display(),
                        comparison.path.display()
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::LightRed)),
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
        format!("{} Mode", app.edit_kind.name())
    } else {
        "View Mode".into()
    };
    let selection = selection_location_summary(app);
    let title = label.map_or_else(
        || format!(" Hex Viewer - {mode_name} | {selection} "),
        |label| format!(" {label} - {mode_name} | {selection} "),
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
    render_vertical_scrollbar(
        frame,
        area,
        app.row_count(),
        app.visible_rows,
        app.scroll,
        app.theme.hex_secondary.color(),
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

    if app.settings.show_overlays
        && let Some(field) = app.fields.iter().find(|field| field.contains(offset))
    {
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
    if app.is_selected(offset) {
        style = style
            .bg(app.theme.selection_background.color())
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
    }
    style
}

fn render_fields(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    let list_height = app.visible_fields;
    for (index, field) in app
        .fields
        .iter()
        .enumerate()
        .skip(app.fields_scroll)
        .take(list_height)
    {
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
                    " Fields [wheel scroll, a add, Enter edit, d/Del delete] ",
                    title_style(app, active),
                ))
                .borders(Borders::ALL)
                .border_style(border_style(app, active)),
        ),
        area,
    );
    render_vertical_scrollbar(
        frame,
        area,
        app.fields.len(),
        list_height,
        app.fields_scroll,
        if active {
            Color::Cyan
        } else {
            app.theme.border.color()
        },
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
    let decimal = preview
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
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
            format!(
                "0x{:X} to 0x{:X} ({} to {})",
                selection.start(),
                selection.end(),
                selection.start(),
                selection.end()
            ),
        ),
        kv("Length", format!("{} bytes", selection.len())),
        kv("Selected", format!("0x{hex} ({decimal})")),
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

fn selection_location_summary(app: &App) -> String {
    let Some(selection) = app.selection else {
        return "no byte selected".into();
    };
    if selection.start() == selection.end() {
        return format!("0x{:X} ({})", selection.cursor, selection.cursor);
    }
    format!(
        "0x{:X} to 0x{:X} ({} to {}, {} bytes)",
        selection.start(),
        selection.end(),
        selection.start(),
        selection.end(),
        selection.len()
    )
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
        format!(" | {} Mode", app.edit_kind.name())
    } else {
        " | View Mode".into()
    };
    let first = Line::from(vec![
        Span::styled(
            format!(" {} ", app.path.display()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "| {} bytes | {} fields | {}{dirty}{search}{mode}",
            app.bytes.len(),
            app.fields.len(),
            selection_location_summary(app),
        )),
    ]);
    let second = Line::styled(&app.status, Style::default().fg(Color::Cyan));
    let help = if app.edit_mode {
        "Ctrl+B then Left/Right binary | Insert/i switches overwrite/insert | Del removes selection | Ctrl+U/R undo/redo | Ctrl+S save | Esc View Mode | ? keybinds"
    } else {
        "Ctrl+B then Left/Right binary, S compare | Ctrl+U/R undo/redo | Ctrl+S save | Ctrl+F search | i edit | ? keybinds"
    };
    let third = Line::styled(help, Style::default().fg(Color::DarkGray));
    frame.render_widget(Paragraph::new(vec![first, second, third]), area);
}

fn render_input_modal(frame: &mut Frame, title: &str, input: &crate::app::TextInput, help: &str) {
    let area = centered_rect(frame.area(), 78, 7);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            input_line(input),
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
    render_path_completion_modal(
        frame,
        title,
        &dialog.input,
        &dialog.suggestions,
        dialog.active_suggestion,
        dialog.suggestion_scroll,
        help,
    );
}

fn render_open_file_modal(frame: &mut Frame, dialog: &OpenFileDialog) {
    match dialog {
        OpenFileDialog::Choice { active } => {
            let area = centered_rect(frame.area(), 62, 9);
            frame.render_widget(Clear, area);
            let options = ["Use system file picker", "Type a full or relative path"];
            let lines = options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    Line::styled(
                        format!(" {} {option}", if index == *active { ">" } else { " " }),
                        selected_row(index == *active),
                    )
                })
                .chain([
                    Line::from(""),
                    Line::styled(
                        "Up/Down or Tab select | Enter confirm | Esc cancel",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
                .collect::<Vec<_>>();
            frame.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .title(Span::styled(" Open binary ", modal_title_style()))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                ),
                area,
            );
        }
        OpenFileDialog::ManualPath {
            input,
            suggestions,
            active_suggestion,
            suggestion_scroll,
        } => render_manual_path_modal(
            frame,
            input,
            suggestions,
            *active_suggestion,
            *suggestion_scroll,
        ),
    }
}

fn render_manual_path_modal(
    frame: &mut Frame,
    input: &crate::app::TextInput,
    suggestions: &[std::path::PathBuf],
    active_suggestion: Option<usize>,
    suggestion_scroll: usize,
) {
    render_path_completion_modal(
        frame,
        " Open binary by path ",
        input,
        suggestions,
        active_suggestion,
        suggestion_scroll,
        "Type a full or relative binary path.",
    );
}

fn render_path_completion_modal(
    frame: &mut Frame,
    title: &str,
    input: &crate::app::TextInput,
    suggestions: &[std::path::PathBuf],
    active_suggestion: Option<usize>,
    suggestion_scroll: usize,
    help: &str,
) {
    let suggestion_lines = suggestions.len().min(PATH_SUGGESTION_PAGE_SIZE) as u16;
    let area = centered_rect(frame.area(), 78, 7 + suggestion_lines);
    frame.render_widget(Clear, area);
    let mut lines = vec![Line::from(""), input_line(input)];
    if !suggestions.is_empty() {
        lines.push(Line::from(""));
        lines.extend(
            suggestions
                .iter()
                .enumerate()
                .skip(suggestion_scroll)
                .take(PATH_SUGGESTION_PAGE_SIZE)
                .map(|(index, path)| {
                    let (kind, name) = if path.is_dir() {
                        (
                            "[DIR] ",
                            format!(
                                "{}/",
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("directory")
                            ),
                        )
                    } else {
                        (
                            "[FILE]",
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("file")
                                .to_owned(),
                        )
                    };
                    Line::styled(
                        format!(
                            " {} {kind} {name}",
                            if Some(index) == active_suggestion {
                                ">"
                            } else {
                                " "
                            },
                        ),
                        selected_row(Some(index) == active_suggestion),
                    )
                }),
        );
    }
    lines.extend([
        Line::from(""),
        Line::styled(
            format!(
                "{help} Tab/Up/Down select | PgUp/PgDn scroll | Enter confirm | mouse wheel scroll"
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(title, modal_title_style()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
    render_vertical_scrollbar(
        frame,
        area,
        suggestions.len(),
        PATH_SUGGESTION_PAGE_SIZE,
        suggestion_scroll,
        Color::Cyan,
    );
}

fn input_line(input: &crate::app::TextInput) -> Line<'static> {
    let style = if input.selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![Span::styled(" ", style)];
    spans.extend(editable_spans(input, style, true));
    Line::from(spans)
}

fn cursor_is_visible() -> bool {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .div_euclid(500)
        .is_multiple_of(2)
}

fn editable_spans(input: &crate::app::TextInput, style: Style, active: bool) -> Vec<Span<'static>> {
    caret_spans(
        &input.value,
        input.cursor_byte_index(),
        style,
        active && cursor_is_visible(),
    )
}

fn caret_spans(value: &str, cursor: usize, style: Style, show_caret: bool) -> Vec<Span<'static>> {
    let cursor = cursor.min(value.len());
    let mut spans = vec![Span::styled(value[..cursor].to_owned(), style)];
    if show_caret {
        spans.push(Span::styled(
            "▏",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    spans.push(Span::styled(value[cursor..].to_owned(), style));
    spans
}

fn render_field_modal(frame: &mut Frame, editor: &FieldEditor) {
    let area = centered_rect(frame.area(), 72, 13);
    frame.render_widget(Clear, area);
    let text_rows = [
        ("Name", &editor.name),
        ("Description", &editor.description),
        ("Start", &editor.start),
        ("End", &editor.end),
    ];
    let mut lines = text_rows
        .iter()
        .enumerate()
        .map(|(index, (label, input))| {
            let active = index == editor.active;
            let style = if active && input.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                selected_row(active)
            };
            let mut spans = vec![Span::styled(format!(" {label:<12} "), style)];
            spans.extend(editable_spans(input, style, active));
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    lines.push(Line::styled(
        format!(" {:<12} {}", "Color", editor.color.name()),
        selected_row(editor.active == 4),
    ));
    lines.extend([
        Line::from(""),
        Line::styled(
            "Tab/Up/Down field | Backspace/Ctrl+H erase | Enter save | Esc cancel",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
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
            let active = index == editor.active;
            let style = selected_row(active);
            if index == 0 {
                let mut spans = vec![Span::styled(format!(" {label:<17} "), style)];
                spans.extend(caret_spans(
                    value,
                    value.len(),
                    style,
                    active && cursor_is_visible(),
                ));
                Line::from(spans)
            } else {
                Line::styled(format!(" {label:<17} {value}"), style)
            }
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
    let area = centered_rect(frame.area(), 70, 15);
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
        (
            "Field overlays",
            enabled(app.settings.show_overlays).to_string(),
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
    let mut content = pane
        .output
        .iter()
        .map(|line| Line::raw(line.clone()))
        .collect::<Vec<_>>();
    if !pane.repl_lines.is_empty() {
        content.push(Line::from(""));
        content.extend(pane.repl_lines.iter().map(|line| {
            Line::from(vec![
                Span::styled("... ", Style::default().fg(Color::LightGreen)),
                Span::raw(line.clone()),
            ])
        }));
    }
    let prompt = if pane.repl_lines.is_empty() {
        ">>> "
    } else {
        "... "
    };
    let mut input_spans = vec![Span::styled(prompt, Style::default().fg(Color::LightGreen))];
    input_spans.extend(editable_spans(
        &pane.input,
        Style::default().fg(Color::White),
        active,
    ));
    if pane.pending > 0 {
        input_spans.push(Span::styled(
            format!("  [{} running]", pane.pending),
            Style::default().fg(Color::Yellow),
        ));
    }
    content.push(Line::from(input_spans));
    let (start, end) = python_output_range(content.len(), output_height, pane.scroll);
    let lines = content[start..end].to_vec();
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(Span::styled(
                    " Python console — ↑/↓ history | Ctrl+C interrupt | Tab pane | Enter run | Esc close ",
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
    render_vertical_scrollbar(
        frame,
        area,
        content.len(),
        pane.visible_output_lines,
        python_scrollbar_position(content.len(), pane.visible_output_lines, pane.scroll),
        if active {
            Color::LightGreen
        } else {
            Color::Green
        },
    );
}

fn python_output_range(length: usize, height: usize, scroll: usize) -> (usize, usize) {
    let max_scroll = length.saturating_sub(height);
    let end = length.saturating_sub(scroll.min(max_scroll));
    (end.saturating_sub(height), end)
}

fn python_scrollbar_position(length: usize, viewport_length: usize, scroll: usize) -> usize {
    length
        .saturating_sub(viewport_length)
        .saturating_sub(scroll)
}

fn render_vertical_scrollbar(
    frame: &mut Frame,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    position: usize,
    color: Color,
) {
    if content_length <= viewport_length || area.width < 2 || area.height < 3 {
        return;
    }
    let mut state = ScrollbarState::new(content_length)
        .position(scrollbar_state_position(
            content_length,
            viewport_length,
            position,
        ))
        .viewport_content_length(viewport_length);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"))
        .track_symbol(Some("│"))
        .thumb_symbol("█")
        .style(Style::default().fg(color));
    frame.render_stateful_widget(
        scrollbar,
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

fn scrollbar_state_position(
    content_length: usize,
    viewport_length: usize,
    position: usize,
) -> usize {
    let max_scroll = content_length.saturating_sub(viewport_length);
    position
        .min(max_scroll)
        .saturating_mul(content_length.saturating_sub(1))
        .checked_div(max_scroll)
        .unwrap_or_default()
}

fn render_help_modal(frame: &mut Frame, help: &HelpViewer) {
    let area = centered_rect(
        frame.area(),
        96,
        frame.area().height.saturating_sub(4).min(36),
    );
    frame.render_widget(Clear, area);
    let lines = keybinding_lines();
    let line_count = lines.len();
    let visible_height = area.height.saturating_sub(2) as usize;
    let max_scroll = line_count.saturating_sub(visible_height);
    let scroll = help.scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(
                Block::default()
                    .title(Span::styled(
                        " Help: keyboard and mouse reference — scroll with arrows, PgUp/PgDn, or wheel; Esc/?/q closes ",
                        modal_title_style(),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
    render_vertical_scrollbar(frame, area, line_count, visible_height, scroll, Color::Cyan);
}

fn keybinding_lines() -> Vec<Line<'static>> {
    let section = |title| {
        Line::from(vec![
            Span::styled(
                format!("  {title}  "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " ─────────────────────────",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };
    let binding = |keys: &'static str, action: &'static str| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!(" {keys} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  —  ", Style::default().fg(Color::DarkGray)),
            Span::styled(action, Style::default().fg(Color::White)),
        ])
    };

    vec![
        Line::from(vec![
            Span::styled(
                "  QUICK TIP  ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "highlighted keys and their action stay together on each row",
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(""),
        section("Workspace"),
        binding("Ctrl+B, then Right", "activate the next binary"),
        binding("Ctrl+B, then Left", "activate the previous binary"),
        binding("Ctrl+B, then S", "toggle side-by-side comparison"),
        binding("Ctrl+N", "choose system picker or type a binary path"),
        binding("Ctrl+W", "close the active binary (twice if unsaved)"),
        binding("Ctrl+D", "toggle byte diff mode"),
        binding("Ctrl+Z", "suspend on Unix; resume with shell fg"),
        binding(
            "e / Esc",
            "show / hide entropy; calculations run in the background",
        ),
        binding("mouse on tab/pane", "activate that binary"),
        Line::from(""),
        section("View Mode"),
        binding("arrows", "move the byte cursor"),
        binding("Shift+arrows", "extend the byte selection"),
        binding("Page Up / Page Down", "move by one visible page"),
        binding(
            "Home / End or gg / G",
            "jump to the start / end of the file",
        ),
        binding("mouse drag", "select a range of bytes"),
        binding(
            "Ctrl + mouse drag",
            "add a separate byte range (mouse-only; no shell keybinding conflict)",
        ),
        binding("mouse wheel", "scroll the hex viewer"),
        binding("i", "enter byte edit mode (Overwrite initially)"),
        binding("Ctrl+F", "open byte-pattern search"),
        binding("n / N", "next / previous search result"),
        binding("Ctrl+Down / Ctrl+Up", "next / previous search result"),
        binding("Ctrl+G", "jump to a decimal or hexadecimal offset"),
        binding(
            "Ctrl+C / Ctrl+Shift+C (Cmd+C on macOS)",
            "copy every selected range as continuous hexadecimal",
        ),
        binding("Ctrl+U / Ctrl+R", "undo / redo byte edits"),
        binding("Ctrl+S", "save the edited binary"),
        binding("a", "create a field from the current selection"),
        binding("Tab", "switch between viewer and fields pane"),
        binding("Enter", "edit the selected field"),
        binding("d / Delete", "delete the selected field"),
        binding("[ / ]", "select previous / next field"),
        binding(
            "Ctrl+O / Ctrl+L",
            "save / load a field overlay (auto-saved per file)",
        ),
        binding("o", "toggle field overlays"),
        binding("s", "open viewer settings"),
        binding("t", "open theme customization"),
        binding("p", "open the Python buffer console"),
        Line::from(""),
        section("Byte Edit Mode"),
        binding("0-9, A-F", "overwrite the selected byte, two nibbles"),
        binding("Insert / i", "toggle between Overwrite and Insert Mode"),
        binding(
            "0-9, A-F in Insert",
            "insert a byte at the cursor, two nibbles",
        ),
        binding(
            "Backspace / Delete",
            "delete the current selected byte range",
        ),
        binding(
            "Ctrl+C / Ctrl+Shift+C (Cmd+C on macOS)",
            "copy every selected range as continuous hexadecimal",
        ),
        binding(
            "Ctrl+V / Ctrl+Shift+V (Cmd+V on macOS)",
            "paste hexadecimal from the clipboard as one batched edit",
        ),
        binding(
            "arrows / Page Up/Down",
            "navigate without leaving byte edit mode",
        ),
        binding(
            "Ctrl+U / Ctrl+R",
            "undo / redo overwrite, insertion, or deletion",
        ),
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
        binding(
            "Tab / Up / Down",
            "complete and select filesystem paths in path dialogs",
        ),
        binding(
            "PgUp / PgDn / wheel",
            "scroll filesystem completion choices",
        ),
        binding("Left / Right / Space", "change the selected option"),
        binding("Ctrl+U", "clear a text input"),
        binding("Enter", "confirm or close"),
        binding("Escape", "cancel or close"),
        binding("Ctrl+S / Ctrl+L", "save / load inside the theme menu"),
        binding("Ctrl+R", "reset theme or settings after y/n confirmation"),
        Line::from(""),
        section("Python console"),
        binding(
            "Enter",
            "execute; blank input adds a prompt or finishes a multi-line block",
        ),
        binding("Up / Down", "previous / next Python command"),
        binding("Ctrl+C", "interrupt the running Python command"),
        binding("Tab / Shift+Tab", "cycle viewer, fields, and Python panes"),
        binding(":apply", "copy same-length buffer edits into rexedit"),
        binding("PgUp/PgDn / wheel", "scroll console output"),
        binding("Ctrl+Home / Ctrl+End", "oldest / newest console output"),
        binding("Ctrl+L", "clear console output"),
        binding(
            "viewer: i / Esc",
            "enter/leave hex edit mode without closing Python",
        ),
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
    fn sidebar_leaves_room_for_ascii_and_ends_with_the_viewer() {
        let (viewer, sidebar) = split_viewer_and_sidebar(Rect::new(0, 0, 120, 36));
        assert_eq!(viewer.width, VIEWER_MIN_WIDTH);
        assert_eq!(sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(viewer.bottom(), sidebar.bottom());
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
    fn python_scrollbar_position_uses_top_based_coordinates() {
        assert_eq!(python_scrollbar_position(100, 20, 0), 80);
        assert_eq!(python_scrollbar_position(100, 20, 80), 0);
    }

    #[test]
    fn scrollbar_thumb_reaches_the_end_of_scrolled_content() {
        assert_eq!(scrollbar_state_position(100, 20, 0), 0);
        assert_eq!(scrollbar_state_position(100, 20, 80), 99);
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
    fn inspector_includes_prefixed_hex_and_decimal_selection_details() {
        let mut app = App::new(PathBuf::from("sample.bin"), vec![0, 0xDE, 0xAD]);
        app.selection = Some(crate::model::Selection {
            anchor: 1,
            cursor: 2,
        });
        let rendered = inspector_lines(&app)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("0x1 to 0x2 (1 to 2)"));
        assert!(rendered.contains("0xDE AD (222, 173)"));
    }

    #[test]
    fn caret_is_inserted_at_the_current_text_position() {
        let rendered = caret_spans("abcd", 2, Style::default(), true)
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, "ab▏cd");
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
        assert!(rendered.contains("Inspector"));
    }

    #[test]
    fn side_by_side_keeps_each_document_scroll_position() {
        let backend = TestBackend::new(180, 42);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut workspace = Workspace::new(vec![
            App::new(PathBuf::from("one.bin"), vec![0; 4096]),
            App::new(PathBuf::from("two.bin"), vec![1; 4096]),
        ]);
        workspace.side_by_side = true;
        workspace.documents[0].scroll = 7;
        workspace.documents[1].scroll = 23;

        terminal
            .draw(|frame| render_workspace(frame, &mut workspace))
            .unwrap();

        assert_eq!(workspace.documents[0].scroll, 7);
        assert_eq!(workspace.documents[1].scroll, 23);
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
        let backend = TestBackend::new(120, 80);
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
        assert!(rendered.contains("Help:"));
        assert!(rendered.contains("Ctrl+F"));
        assert!(keybinding_lines().iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Byte Edit Mode"))
        }));
    }

    #[test]
    fn help_keeps_each_keybinding_next_to_its_action() {
        let line = keybinding_lines()
            .into_iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.as_ref() == " Ctrl+F ")
            })
            .expect("search shortcut should be listed");

        assert_eq!(line.spans.len(), 4);
        assert_eq!(line.spans[3].content.as_ref(), "open byte-pattern search");
    }
}
