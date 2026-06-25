use ratatui::style::Color;
use serde::{Deserialize, Serialize};

pub const DEFAULT_BYTES_PER_ROW: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub cursor: usize,
}

impl Selection {
    pub fn new(offset: usize) -> Self {
        Self {
            anchor: offset,
            cursor: offset,
        }
    }

    pub fn start(self) -> usize {
        self.anchor.min(self.cursor)
    }

    pub fn end(self) -> usize {
        self.anchor.max(self.cursor)
    }

    pub fn len(self) -> usize {
        self.end() - self.start() + 1
    }

    pub fn contains(self, offset: usize) -> bool {
        (self.start()..=self.end()).contains(&offset)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub description: String,
    pub start: usize,
    pub end: usize,
    pub color: FieldColor,
}

impl Field {
    pub fn contains(&self, offset: usize) -> bool {
        (self.start..=self.end).contains(&offset)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum FieldColor {
    #[default]
    Cyan,
    Green,
    Yellow,
    Magenta,
    Blue,
    Red,
}

impl FieldColor {
    pub const ALL: [Self; 6] = [
        Self::Cyan,
        Self::Green,
        Self::Yellow,
        Self::Magenta,
        Self::Blue,
        Self::Red,
    ];

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Cyan => "cyan",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Magenta => "magenta",
            Self::Blue => "blue",
            Self::Red => "red",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Self::Cyan => Color::Cyan,
            Self::Green => Color::Green,
            Self::Yellow => Color::Yellow,
            Self::Magenta => Color::Magenta,
            Self::Blue => Color::Blue,
            Self::Red => Color::Red,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Overlay {
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ByteColorMode {
    #[default]
    Plain,
    Alternating,
    ByteClass,
    HighNibble,
    LowNibble,
    ZeroBytes,
    Printable,
    ValueBands,
}

impl ByteColorMode {
    pub const ALL: [Self; 8] = [
        Self::Plain,
        Self::Alternating,
        Self::ByteClass,
        Self::HighNibble,
        Self::LowNibble,
        Self::ZeroBytes,
        Self::Printable,
        Self::ValueBands,
    ];

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[index.checked_sub(1).unwrap_or(Self::ALL.len() - 1)]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "plain white",
            Self::Alternating => "alternating bytes",
            Self::ByteClass => "byte classes",
            Self::HighNibble => "high-nibble bands",
            Self::LowNibble => "low-nibble bands",
            Self::ZeroBytes => "zero vs non-zero",
            Self::Printable => "printable vs binary",
            Self::ValueBands => "four value bands",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    DarkGray,
    Gray,
    #[default]
    White,
    Red,
    LightRed,
    Green,
    LightGreen,
    Yellow,
    LightYellow,
    Blue,
    LightBlue,
    Magenta,
    LightMagenta,
    Cyan,
    LightCyan,
}

impl NamedColor {
    pub const ALL: [Self; 16] = [
        Self::Black,
        Self::DarkGray,
        Self::Gray,
        Self::White,
        Self::Red,
        Self::LightRed,
        Self::Green,
        Self::LightGreen,
        Self::Yellow,
        Self::LightYellow,
        Self::Blue,
        Self::LightBlue,
        Self::Magenta,
        Self::LightMagenta,
        Self::Cyan,
        Self::LightCyan,
    ];

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[index.checked_sub(1).unwrap_or(Self::ALL.len() - 1)]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::DarkGray => "dark gray",
            Self::Gray => "gray",
            Self::White => "white",
            Self::Red => "red",
            Self::LightRed => "light red",
            Self::Green => "green",
            Self::LightGreen => "light green",
            Self::Yellow => "yellow",
            Self::LightYellow => "light yellow",
            Self::Blue => "blue",
            Self::LightBlue => "light blue",
            Self::Magenta => "magenta",
            Self::LightMagenta => "light magenta",
            Self::Cyan => "cyan",
            Self::LightCyan => "light cyan",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Self::Black => Color::Black,
            Self::DarkGray => Color::DarkGray,
            Self::Gray => Color::Gray,
            Self::White => Color::White,
            Self::Red => Color::Red,
            Self::LightRed => Color::LightRed,
            Self::Green => Color::Green,
            Self::LightGreen => Color::LightGreen,
            Self::Yellow => Color::Yellow,
            Self::LightYellow => Color::LightYellow,
            Self::Blue => Color::Blue,
            Self::LightBlue => Color::LightBlue,
            Self::Magenta => Color::Magenta,
            Self::LightMagenta => Color::LightMagenta,
            Self::Cyan => Color::Cyan,
            Self::LightCyan => Color::LightCyan,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Theme {
    pub name: String,
    pub byte_mode: ByteColorMode,
    pub hex_primary: NamedColor,
    pub hex_secondary: NamedColor,
    pub ascii: NamedColor,
    pub offset: NamedColor,
    pub border: NamedColor,
    pub selection_background: NamedColor,
    pub search_background: NamedColor,
    pub modified: NamedColor,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            byte_mode: ByteColorMode::Plain,
            hex_primary: NamedColor::White,
            hex_secondary: NamedColor::Cyan,
            ascii: NamedColor::Green,
            offset: NamedColor::DarkGray,
            border: NamedColor::DarkGray,
            selection_background: NamedColor::Blue,
            search_background: NamedColor::DarkGray,
            modified: NamedColor::LightRed,
        }
    }
}

impl SearchMatch {
    pub fn contains(&self, offset: usize) -> bool {
        (self.start..=self.end).contains(&offset)
    }
}
