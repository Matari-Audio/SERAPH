//! Grok Build presentation primitives.
//!
//! Modified from xAI Grok Build's Apache-2.0 `groknight.rs`, `glyphs.rs`, and
//! `views/dock.rs` at commit bb7f39d5858cbf5e00de639367f59debbdcb0138.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg_base: Color,
    pub bg_highlight: Color,
    pub text_primary: Color,
    pub gray_dim: Color,
    pub gray: Color,
    pub gray_bright: Color,
    pub accent_running: Color,
    pub accent_error: Color,
    pub accent_success: Color,
    pub warning: Color,
    pub prompt_border_active: Color,
    pub accent_model: Color,
    pub command_blue: Color,
}

pub const GROKNIGHT: Theme = Theme {
    bg_base: rgb(20, 20, 20),
    bg_highlight: rgb(36, 36, 36),
    text_primary: rgb(225, 225, 225),
    gray_dim: rgb(88, 88, 88),
    gray: rgb(108, 108, 108),
    gray_bright: rgb(120, 120, 120),
    accent_running: rgb(187, 154, 247),
    accent_error: rgb(247, 118, 142),
    accent_success: rgb(158, 206, 106),
    warning: rgb(224, 175, 104),
    prompt_border_active: rgb(80, 80, 88),
    accent_model: rgb(26, 188, 156),
    command_blue: rgb(122, 162, 247),
};

pub const BRAILLE_SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub const fn prompt_arrow() -> &'static str {
    "❯ "
}

pub const MAX_SECTION_ROWS: usize = 2;

pub struct DockRow {
    pub icon: &'static str,
    pub color: Color,
    pub kind: String,
    pub description: String,
    pub activity: Option<String>,
    pub meta: String,
    pub killable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Subagents,
    Tasks,
    Watchers,
    Queued,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Self::Subagents => "Subagents",
            Self::Tasks => "Tasks",
            Self::Watchers => "Watchers",
            Self::Queued => "Queued",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockItem {
    Header(Section),
    Row(Section, usize),
}

#[derive(Default, Clone, Copy)]
pub struct DockCounts {
    pub subagents: usize,
    pub tasks: usize,
    pub watchers: usize,
    pub queued: usize,
    pub subagents_expanded: bool,
    pub tasks_expanded: bool,
    pub watchers_expanded: bool,
}

impl DockCounts {
    fn row_sections(self) -> [(Section, usize, bool); 3] {
        [
            (Section::Subagents, self.subagents, self.subagents_expanded),
            (Section::Tasks, self.tasks, self.tasks_expanded),
            (Section::Watchers, self.watchers, self.watchers_expanded),
        ]
    }
}

#[derive(Default)]
pub struct DockData {
    pub subagents: Vec<DockRow>,
    pub tasks: Vec<DockRow>,
    pub tasks_total: usize,
    pub watchers: Vec<DockRow>,
    pub queued: usize,
    pub subagents_expanded: bool,
    pub tasks_expanded: bool,
    pub watchers_expanded: bool,
    pub focused: bool,
    pub cursor: usize,
    pub queue_body_rows: u16,
}

impl DockData {
    pub fn counts(&self) -> DockCounts {
        DockCounts {
            subagents: self.subagents.len(),
            tasks: self.tasks_total,
            watchers: self.watchers.len(),
            queued: self.queued,
            subagents_expanded: self.subagents_expanded,
            tasks_expanded: self.tasks_expanded,
            watchers_expanded: self.watchers_expanded,
        }
    }

    fn rows(&self, section: Section) -> &[DockRow] {
        match section {
            Section::Subagents => &self.subagents,
            Section::Tasks => &self.tasks,
            Section::Watchers => &self.watchers,
            Section::Queued => &[],
        }
    }
}

enum Visual {
    Header(Section),
    Row(Section, usize),
    More(usize),
}

fn visual_rows(counts: &DockCounts) -> Vec<Visual> {
    let mut rows = Vec::new();
    for (section, len, expanded) in counts.row_sections() {
        if len == 0 {
            continue;
        }
        rows.push(Visual::Header(section));
        if expanded {
            rows.extend((0..len.min(MAX_SECTION_ROWS)).map(|index| Visual::Row(section, index)));
            if len > MAX_SECTION_ROWS {
                rows.push(Visual::More(len - MAX_SECTION_ROWS));
            }
        }
    }
    if counts.queued > 0 {
        rows.push(Visual::Header(Section::Queued));
    }
    rows
}

fn as_item(visual: &Visual) -> Option<DockItem> {
    match *visual {
        Visual::Header(section) => Some(DockItem::Header(section)),
        Visual::Row(section, index) => Some(DockItem::Row(section, index)),
        Visual::More(_) => None,
    }
}

pub fn visible_items(data: &DockData) -> Vec<DockItem> {
    visual_rows(&data.counts())
        .iter()
        .filter_map(as_item)
        .collect()
}

pub fn desired_height(data: &DockData) -> u16 {
    let rows = visual_rows(&data.counts()).len() as u16;
    rows + if data.queued > 0 {
        data.queue_body_rows
    } else {
        0
    }
}

pub fn render_dock(buf: &mut Buffer, area: Rect, theme: &Theme, data: &DockData) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let counts = data.counts();
    let mut item_index = 0;
    for (index, visual) in visual_rows(&counts).into_iter().enumerate() {
        let y = area.y.saturating_add(index as u16);
        if y >= area.bottom() {
            return;
        }
        match visual {
            Visual::Header(section) => {
                let (count, expanded) = match section {
                    Section::Subagents => (counts.subagents, counts.subagents_expanded),
                    Section::Tasks => (counts.tasks, counts.tasks_expanded),
                    Section::Watchers => (counts.watchers, counts.watchers_expanded),
                    Section::Queued => (counts.queued, data.queue_body_rows > 0),
                };
                buf.set_line(
                    area.x,
                    y,
                    &section_header(theme, area.width, expanded, section.label(), count),
                    area.width,
                );
                if data.focused && item_index == data.cursor {
                    highlight_row(buf, area, y, theme);
                }
                item_index += 1;
            }
            Visual::Row(section, index) => {
                let selected = data.focused && item_index == data.cursor;
                paint_row(
                    buf,
                    area,
                    y,
                    theme,
                    &data.rows(section)[index],
                    selected,
                    section == Section::Subagents,
                );
                if selected {
                    highlight_row(buf, area, y, theme);
                }
                item_index += 1;
            }
            Visual::More(count) => {
                buf.set_line(
                    area.x,
                    y,
                    &Line::from(Span::styled(
                        format!("    ▾ {count} more"),
                        Style::default().fg(theme.gray),
                    )),
                    area.width,
                );
            }
        }
    }
}

fn highlight_row(buf: &mut Buffer, area: Rect, y: u16, theme: &Theme) {
    for x in area.x..area.right() {
        buf[(x, y)].set_bg(theme.bg_highlight);
    }
}

fn section_header(
    theme: &Theme,
    width: u16,
    expanded: bool,
    label: &str,
    count: usize,
) -> Line<'static> {
    let chevron = if expanded { "▾ " } else { "▸ " };
    let count_text = format!(" {count} ");
    let used = Line::from(format!("{chevron}{label}{count_text}")).width();
    Line::from(vec![
        Span::styled(chevron, Style::default().fg(theme.gray)),
        Span::styled(
            label.to_owned(),
            Style::default()
                .fg(theme.gray_bright)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(count_text, Style::default().fg(theme.gray)),
        Span::styled(
            "─".repeat((width as usize).saturating_sub(used)),
            Style::default().fg(theme.gray_dim),
        ),
    ])
}

fn paint_row(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    theme: &Theme,
    row: &DockRow,
    selected: bool,
    openable: bool,
) {
    let accent = Style::default().fg(row.color);
    let mut spans = vec![
        Span::styled(format!("  {} ", row.icon), accent),
        Span::styled(row.kind.clone(), accent),
        Span::raw(" "),
        Span::styled(
            row.description.clone(),
            Style::default().fg(theme.text_primary),
        ),
    ];
    if let Some(activity) = row
        .activity
        .as_deref()
        .filter(|activity| !activity.is_empty())
    {
        spans.push(Span::styled(
            format!(" — {activity}"),
            Style::default().fg(theme.gray),
        ));
    }
    let left = Line::from(spans);
    let left_width = left.width() as u16;
    buf.set_line(area.x, y, &left, area.width);

    let mut meta = vec![Span::styled(
        row.meta.clone(),
        Style::default().fg(theme.gray),
    )];
    if selected {
        if openable {
            meta.push(Span::styled(" [↗]", Style::default().fg(theme.gray_bright)));
        }
        if row.killable {
            meta.push(Span::styled(
                " [stop]",
                Style::default().fg(theme.accent_error),
            ));
        }
    }
    let meta = Line::from(meta);
    let meta_width = meta.width() as u16;
    if left_width + 1 + meta_width <= area.width {
        buf.set_line(area.right() - meta_width, y, &meta, meta_width);
    }
}
