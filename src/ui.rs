//! Rendu du tableau de bord : palette, lignes stylées, cartes et sérialisation ANSI.

use crate::model::{Gpu, Metrics};
use crate::sys::Sampler;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const MIN_WIDTH: usize = 76;
pub const MIN_HEIGHT: usize = 22;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

const BORDER: Rgb = Rgb(0x3b, 0x42, 0x61);
const DIM: Rgb = Rgb(0x73, 0x7a, 0xa2);
const DIM_SOFT: Rgb = Rgb(0x56, 0x5f, 0x89);
const TEXT: Rgb = Rgb(0xc0, 0xca, 0xf5);
const TITLE: Rgb = Rgb(0x7a, 0xa2, 0xf7);
const CPU_C: Rgb = Rgb(0x7d, 0xcf, 0xff);
const GPU_C: Rgb = Rgb(0xbb, 0x9a, 0xf7);
const GREEN: Rgb = Rgb(0x9e, 0xce, 0x6a);
const YELLOW: Rgb = Rgb(0xe0, 0xaf, 0x68);
const ORANGE: Rgb = Rgb(0xff, 0x9e, 0x64);
const RED: Rgb = Rgb(0xf7, 0x76, 0x8e);
const TRACK: Rgb = Rgb(0x36, 0x3c, 0x55);

const LOAD_STOPS: [(f64, Rgb); 6] = [
    (0.00, Rgb(86, 182, 255)),
    (0.35, Rgb(125, 207, 255)),
    (0.55, Rgb(158, 206, 106)),
    (0.72, Rgb(224, 175, 104)),
    (0.86, Rgb(255, 158, 100)),
    (1.00, Rgb(247, 118, 142)),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Style {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
}

impl Style {
    pub fn fg(color: Rgb) -> Self {
        Self {
            fg: Some(color),
            ..Self::default()
        }
    }

    pub fn strong(color: Rgb) -> Self {
        Self {
            fg: Some(color),
            bold: true,
            ..Self::default()
        }
    }

    pub fn fg_bg(fg: Rgb, bg: Rgb) -> Self {
        Self {
            fg: Some(fg),
            bg: Some(bg),
            bold: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, Default)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, text: &str, style: Style) {
        if !text.is_empty() {
            self.spans.push(Span {
                text: text.to_string(),
                style,
            });
        }
    }

    pub fn extend(&mut self, other: &Line) {
        for span in &other.spans {
            if !span.text.is_empty() {
                self.spans.push(span.clone());
            }
        }
    }

    pub fn width(&self) -> usize {
        self.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum()
    }

    pub fn blank(width: usize) -> Self {
        let mut line = Self::new();
        if width > 0 {
            line.push(&" ".repeat(width), Style::default());
        }
        line
    }

    pub fn spaces(width: usize) -> Self {
        Self::blank(width)
    }

    pub fn pad_to(&mut self, width: usize) {
        let current = self.width();
        if current < width {
            self.push(&" ".repeat(width - current), Style::default());
        } else if current > width {
            self.truncate(width, false);
        }
    }

    /// Coupe la ligne à `max` colonnes, avec une ellipse optionnelle.
    pub fn truncate(&mut self, max: usize, ellipsis: bool) {
        if self.width() <= max {
            return;
        }
        let target = if ellipsis { max.saturating_sub(1) } else { max };
        let mut result = Line::new();
        let mut used = 0usize;
        let mut last_style = Style::default();
        'outer: for span in &self.spans {
            let mut buffer = String::new();
            for character in span.text.chars() {
                let char_width = character.width().unwrap_or(0);
                if used + char_width > target {
                    if !buffer.is_empty() {
                        result.push(&buffer, span.style);
                    }
                    break 'outer;
                }
                buffer.push(character);
                used += char_width;
                last_style = span.style;
            }
            result.push(&buffer, span.style);
        }
        if ellipsis && max > 0 {
            result.push("…", last_style);
        }
        *self = result;
    }
}

#[derive(Clone, Copy)]
pub struct Glyphs {
    pub unicode: bool,
}

impl Glyphs {
    pub fn tl(&self) -> &'static str {
        if self.unicode {
            "╭"
        } else {
            "+"
        }
    }
    pub fn tr(&self) -> &'static str {
        if self.unicode {
            "╮"
        } else {
            "+"
        }
    }
    pub fn bl(&self) -> &'static str {
        if self.unicode {
            "╰"
        } else {
            "+"
        }
    }
    pub fn br(&self) -> &'static str {
        if self.unicode {
            "╯"
        } else {
            "+"
        }
    }
    pub fn h(&self) -> &'static str {
        if self.unicode {
            "─"
        } else {
            "-"
        }
    }
    pub fn v(&self) -> &'static str {
        if self.unicode {
            "│"
        } else {
            "|"
        }
    }
    pub fn full(&self) -> &'static str {
        if self.unicode {
            "█"
        } else {
            "#"
        }
    }
    pub fn track(&self) -> &'static str {
        if self.unicode {
            "░"
        } else {
            "."
        }
    }
    pub fn dot(&self) -> &'static str {
        if self.unicode {
            "●"
        } else {
            "*"
        }
    }
    pub fn pair_top(&self) -> &'static str {
        if self.unicode {
            "▀"
        } else {
            "^"
        }
    }
    pub fn pair_bottom(&self) -> &'static str {
        if self.unicode {
            "▄"
        } else {
            "v"
        }
    }
    pub fn partial(&self, eighth: usize) -> &'static str {
        if !self.unicode {
            return "#";
        }
        const PARTIAL: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];
        PARTIAL[eighth.clamp(1, 7) - 1]
    }
    pub fn spark(&self, level: usize) -> &'static str {
        const UNICODE: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        const ASCII: [&str; 8] = [" ", ".", ":", "-", "=", "+", "*", "#"];
        let table = if self.unicode { UNICODE } else { ASCII };
        table[level.min(7)]
    }
}

fn blend(first: Rgb, second: Rgb, ratio: f64) -> Rgb {
    let mix = |a: u8, b: u8| {
        (a as f64 + (b as f64 - a as f64) * ratio)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgb(
        mix(first.0, second.0),
        mix(first.1, second.1),
        mix(first.2, second.2),
    )
}

pub fn shade(color: Rgb, factor: f64) -> Rgb {
    Rgb(
        (color.0 as f64 * factor).round().clamp(0.0, 255.0) as u8,
        (color.1 as f64 * factor).round().clamp(0.0, 255.0) as u8,
        (color.2 as f64 * factor).round().clamp(0.0, 255.0) as u8,
    )
}

pub fn load_color(fraction: f64) -> Rgb {
    let value = fraction.clamp(0.0, 1.0);
    for pair in LOAD_STOPS.windows(2) {
        let (start, from) = pair[0];
        let (end, to) = pair[1];
        if value <= end {
            let ratio = if end == start {
                0.0
            } else {
                (value - start) / (end - start)
            };
            return blend(from, to, ratio);
        }
    }
    LOAD_STOPS[LOAD_STOPS.len() - 1].1
}

pub fn value_color(fraction: f64) -> Rgb {
    if fraction < 0.04 {
        DIM
    } else {
        load_color(fraction)
    }
}

pub fn temp_color(temp: Option<f64>) -> Rgb {
    match temp {
        None => DIM,
        Some(value) if value < 60.0 => GREEN,
        Some(value) if value < 72.0 => YELLOW,
        Some(value) if value < 84.0 => ORANGE,
        Some(_) => RED,
    }
}

fn bar_shade(color: Rgb, index: usize, width: usize) -> Rgb {
    shade(
        color,
        0.90 + 0.20 * (index as f64 / width.saturating_sub(1).max(1) as f64),
    )
}

pub fn bar(fraction: f64, width: usize, color: Option<Rgb>, glyphs: &Glyphs) -> Line {
    let width = width.max(1);
    let value = fraction.clamp(0.0, 1.0);
    let base = color.unwrap_or_else(|| load_color(value));
    let filled = value * width as f64;
    let mut full = filled as usize;
    let mut eighth = ((filled - full as f64) * 8.0) as usize;
    if eighth >= 8 {
        full += 1;
        eighth = 0;
    }
    full = full.min(width);
    let mut line = Line::new();
    for index in 0..width {
        if index < full {
            line.push(glyphs.full(), Style::fg(bar_shade(base, index, width)));
        } else if index == full && eighth > 0 {
            line.push(glyphs.partial(eighth), Style::fg(shade(base, 1.02)));
        } else {
            line.push(glyphs.track(), Style::fg(TRACK));
        }
    }
    line
}

/// Barre unique : moitié haute = premier thread, moitié basse = second.
pub fn paired_bar(top: f64, bottom: f64, width: usize, glyphs: &Glyphs) -> Line {
    paired_bar_colors(
        top,
        bottom,
        width,
        glyphs,
        load_color(top.clamp(0.0, 1.0)),
        load_color(bottom.clamp(0.0, 1.0)),
    )
}

/// Variante bicolore : TX en haut, RX en bas pour les liens PCIe.
pub fn paired_bar_colors(
    top: f64,
    bottom: f64,
    width: usize,
    glyphs: &Glyphs,
    top_color: Rgb,
    bottom_color: Rgb,
) -> Line {
    let width = width.max(1);
    let top = top.clamp(0.0, 1.0);
    let bottom = bottom.clamp(0.0, 1.0);
    let top_cells = (top * width as f64 + 0.5) as usize;
    let bottom_cells = (bottom * width as f64 + 0.5) as usize;

    let mut line = Line::new();
    for index in 0..width {
        let has_top = index < top_cells;
        let has_bottom = index < bottom_cells;
        let top_shade = bar_shade(top_color, index, width);
        let bottom_shade = bar_shade(bottom_color, index, width);
        if has_top && has_bottom {
            if glyphs.unicode {
                line.push(glyphs.pair_top(), Style::fg_bg(top_shade, bottom_shade));
            } else {
                line.push("#", Style::strong(top_color));
            }
        } else if has_top {
            line.push(glyphs.pair_top(), Style::fg_bg(top_shade, TRACK));
        } else if has_bottom {
            line.push(glyphs.pair_bottom(), Style::fg_bg(bottom_shade, TRACK));
        } else {
            line.push(glyphs.track(), Style::fg(TRACK));
        }
    }
    line
}

pub fn sparkline(
    values: &[f64],
    width: usize,
    color: Rgb,
    alerts: Option<&[bool]>,
    glyphs: &Glyphs,
) -> Line {
    if width == 0 {
        return Line::new();
    }
    let start = values.len().saturating_sub(width);
    let visible = &values[start..];
    let flags = alerts.map(|series| {
        let flag_start = series.len().saturating_sub(visible.len());
        &series[flag_start..]
    });
    let flag_offset = visible
        .len()
        .saturating_sub(flags.map(|series| series.len()).unwrap_or(0));

    let mut line = Line::new();
    if width > visible.len() {
        line.push(&" ".repeat(width - visible.len()), Style::default());
    }
    for (index, value) in visible.iter().enumerate() {
        let level = (value.clamp(0.0, 100.0) / 100.0 * 7.999) as usize;
        let alert = flags
            .map(|series| {
                index >= flag_offset && series.get(index - flag_offset).copied().unwrap_or(false)
            })
            .unwrap_or(false);
        let style = if alert {
            Style::fg(RED)
        } else if level > 0 {
            Style::fg(color)
        } else {
            Style::fg(shade(color, 0.55))
        };
        line.push(glyphs.spark(level), style);
    }
    line
}

pub fn pct(value: f64) -> String {
    format!("{:>3.0}%", value.clamp(0.0, 100.0))
}

pub fn fmt_gio(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

pub fn fmt_uptime(seconds: f64) -> String {
    let total = seconds.max(0.0) as i64;
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let minutes = (total % 3600) / 60;
    if days > 0 {
        format!("{}j {:02}h", days, hours)
    } else if hours > 0 {
        format!("{}h {:02}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

pub fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut result = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let char_width = character.width().unwrap_or(0);
        if used + char_width + 1 > width {
            break;
        }
        result.push(character);
        used += char_width;
    }
    result.push('…');
    result
}

fn details_line(parts: &[(String, Rgb)], width: usize) -> Line {
    let mut chosen: Vec<(String, Rgb)> = Vec::new();
    for (label, color) in parts {
        let mut candidate: Vec<String> = chosen.iter().map(|(text, _)| text.clone()).collect();
        candidate.push(label.clone());
        if UnicodeWidthStr::width(candidate.join(" · ").as_str()) <= width {
            chosen.push((label.clone(), *color));
        } else if chosen.is_empty() {
            chosen.push((fit(label, width), *color));
        } else {
            break;
        }
    }
    let mut line = Line::new();
    for (index, (label, color)) in chosen.iter().enumerate() {
        if index > 0 {
            line.push(" · ", Style::fg(DIM_SOFT));
        }
        line.push(label, Style::fg(*color));
    }
    line
}

fn choose(candidates: Vec<Line>, width: usize) -> Option<Line> {
    for candidate in &candidates {
        if candidate.width() <= width {
            return Some(candidate.clone());
        }
    }
    candidates.into_iter().last()
}

/// Construit une carte de `width` × `height` lignes.
pub fn card(
    width: usize,
    height: usize,
    title: &Line,
    right: Option<&Line>,
    body: Vec<Line>,
    glyphs: &Glyphs,
) -> Vec<Line> {
    let width = width.max(12);
    let height = height.max(3);
    let inner = width.saturating_sub(2);
    let content_width = inner.saturating_sub(2);
    let left_width = title.width();
    let max_right = inner.saturating_sub(left_width + 8);

    let mut right_line = right.cloned();
    if max_right < 4 {
        right_line = None;
    } else if let Some(line) = right_line.as_mut() {
        if line.width() > max_right {
            line.truncate(max_right, true);
        }
    }
    let right_width = right_line.as_ref().map(Line::width).unwrap_or(0);

    let mut top = Line::new();
    top.push(glyphs.tl(), Style::fg(BORDER));
    top.push(&format!("{} ", glyphs.h()), Style::fg(BORDER));
    top.extend(title);
    top.push(" ", Style::fg(BORDER));
    if let Some(line) = &right_line {
        let fill = inner.saturating_sub(left_width + 3 + right_width + 3);
        top.push(&glyphs.h().repeat(fill), Style::fg(BORDER));
        top.push(" ", Style::fg(BORDER));
        top.extend(line);
        top.push(&format!(" {}", glyphs.h()), Style::fg(BORDER));
    } else {
        let fill = inner.saturating_sub(left_width + 3);
        top.push(&glyphs.h().repeat(fill), Style::fg(BORDER));
    }
    top.push(glyphs.tr(), Style::fg(BORDER));
    top.pad_to(width);

    let mut lines = vec![top];
    for mut row in body.into_iter().take(height - 2) {
        row.pad_to(content_width);
        let mut line = Line::new();
        line.push(&format!("{} ", glyphs.v()), Style::fg(BORDER));
        line.extend(&row);
        line.push(&format!(" {}", glyphs.v()), Style::fg(BORDER));
        line.pad_to(width);
        lines.push(line);
    }
    while lines.len() < height - 1 {
        let mut line = Line::new();
        line.push(glyphs.v(), Style::fg(BORDER));
        line.push(&" ".repeat(width.saturating_sub(2)), Style::default());
        line.push(glyphs.v(), Style::fg(BORDER));
        lines.push(line);
    }

    let mut bottom = Line::new();
    bottom.push(glyphs.bl(), Style::fg(BORDER));
    bottom.push(&glyphs.h().repeat(inner), Style::fg(BORDER));
    bottom.push(glyphs.br(), Style::fg(BORDER));
    lines.push(bottom);
    lines
}

fn side_by_side(left: &[Line], right: &[Line], gap: usize) -> Vec<Line> {
    left.iter()
        .zip(right.iter())
        .map(|(left, right)| {
            let mut line = left.clone();
            line.push(&" ".repeat(gap), Style::default());
            line.extend(right);
            line
        })
        .collect()
}

fn header_block(
    m: &Metrics,
    width: usize,
    clock: &str,
    dot: Rgb,
    glyphs: &Glyphs,
    app_title: &str,
) -> Vec<Line> {
    let content_width = width.saturating_sub(4);
    let gap = 3;
    let budget = 20.max(content_width.saturating_sub(2 * gap) / 3);
    let bar_width = 6.max(budget.saturating_sub(10));

    let chips = [
        ("CPU", m.cpu_total / 100.0, CPU_C),
        ("RAM", m.ram_pct / 100.0, GREEN),
        ("VRAM", m.vram_pct / 100.0, GPU_C),
    ];
    let mut row1 = Line::new();
    for (index, (label, fraction, accent)) in chips.iter().enumerate() {
        if index > 0 {
            row1.push(&" ".repeat(gap), Style::default());
        }
        let mut chip = Line::new();
        chip.push(&format!("{:<4} ", label), Style::strong(*accent));
        chip.extend(&bar(*fraction, bar_width, None, glyphs));
        chip.push(
            &format!(" {}", pct(fraction * 100.0)),
            Style::strong(value_color(*fraction)),
        );
        chip.pad_to(budget);
        row1.extend(&chip);
    }
    row1.pad_to(content_width);

    let mut cpu_parts: Vec<(String, Rgb)> = vec![(format!("{} thr", m.cpu_pct.len()), DIM)];
    if let Some(freq) = m.freq_avg {
        cpu_parts.push((format!("{:.2} GHz", freq), DIM));
    }
    let temps: Vec<f64> = m.socket_temp.values().filter_map(|value| *value).collect();
    if !temps.is_empty() {
        let joined = temps
            .iter()
            .map(|value| format!("{:.0}", value))
            .collect::<Vec<_>>()
            .join("/");
        let hottest = temps.iter().copied().fold(f64::MIN, f64::max);
        cpu_parts.push((format!("{}°C", joined), temp_color(Some(hottest))));
    }
    let cpu_detail = details_line(&cpu_parts, budget);

    let mut ram_parts: Vec<(String, Rgb)> = vec![(
        format!("{}/{} Gio", fmt_gio(m.ram_used), fmt_gio(m.ram_total)),
        DIM,
    )];
    if m.swap_total > 0 {
        ram_parts.push((format!("swap {}", fmt_gio(m.swap_used)), DIM));
    }
    let ram_detail = details_line(&ram_parts, budget);
    let vram_detail = details_line(
        &[(
            format!("{}/{} Gio", fmt_gio(m.vram_used), fmt_gio(m.vram_total)),
            DIM,
        )],
        budget,
    );

    let mut row2 = Line::new();
    for (index, detail) in [cpu_detail, ram_detail, vram_detail].iter().enumerate() {
        if index > 0 {
            row2.push(&" ".repeat(gap), Style::default());
        }
        let mut detail = detail.clone();
        detail.pad_to(budget);
        row2.extend(&detail);
    }
    row2.pad_to(content_width);

    let mut title_line = Line::new();
    title_line.push(glyphs.dot(), Style::fg(dot));
    title_line.push(&format!(" {app_title}"), Style::strong(TITLE));

    let right_candidates = vec![
        Line::from_text(
            &format!(
                "{}  ·  charge {:.2}  ·  up {}",
                clock,
                m.load.0,
                fmt_uptime(m.uptime)
            ),
            Style::fg(DIM),
        ),
        Line::from_text(
            &format!("{}  ·  charge {:.2}", clock, m.load.0),
            Style::fg(DIM),
        ),
        Line::from_text(clock, Style::fg(DIM)),
    ];
    let max_right = width.saturating_sub(2 + title_line.width() + 8);
    let right = choose(right_candidates, max_right);
    card(
        width,
        4,
        &title_line,
        right.as_ref(),
        vec![row1, row2],
        glyphs,
    )
}

impl Line {
    fn from_text(text: &str, style: Style) -> Line {
        let mut line = Line::new();
        line.push(text, style);
        line
    }
}

fn cpu_card(
    package: usize,
    m: &Metrics,
    sampler: &Sampler,
    width: usize,
    glyphs: &Glyphs,
) -> Vec<Line> {
    let content_width = width.saturating_sub(4);
    let bar_width = 8.max(content_width.saturating_sub(21));

    let cores: Vec<_> = sampler.topology.cores_of(package).collect();
    let mut body = Vec::new();
    for core in &cores {
        let mut values = Vec::new();
        for &thread in &core.threads {
            values.push(m.cpu_pct.get(thread).copied().unwrap_or(0.0));
        }
        while values.len() < 2 {
            values.push(0.0);
        }

        let pair = core
            .threads
            .iter()
            .map(|thread| format!("{thread:02}"))
            .collect::<Vec<_>>()
            .join("/");
        let mut row = Line::new();
        row.push(&format!("{:02} ", core.index), Style::strong(CPU_C));
        row.push(&format!("{pair:<5}"), Style::fg(DIM));
        row.push(" ", Style::default());
        row.extend(&paired_bar(
            values[0] / 100.0,
            values[1] / 100.0,
            bar_width,
            glyphs,
        ));
        row.push(" ", Style::default());
        let top_color = value_color(values[0] / 100.0);
        let bottom_color = value_color(values[1] / 100.0);
        row.push(glyphs.pair_top(), Style::strong(top_color));
        row.push(&pct(values[0]), Style::strong(top_color));
        row.push(" ", Style::default());
        row.push(glyphs.pair_bottom(), Style::strong(bottom_color));
        row.push(&pct(values[1]), Style::strong(bottom_color));
        row.pad_to(content_width);
        body.push(row);
    }

    let load = m.socket_pct.get(&package).copied().unwrap_or(0.0);
    let freq = m.socket_freq.get(&package).copied().flatten();
    let temp = m.socket_temp.get(&package).copied().flatten();
    let threads: usize = cores.iter().map(|core| core.threads.len()).sum();

    let title_full = {
        let mut line = Line::from_text(&format!("CPU {package}"), Style::strong(CPU_C));
        line.push(&format!(" · {}", sampler.topology.model), Style::fg(DIM));
        line
    };
    let title_medium = {
        let mut line = Line::from_text(&format!("CPU {package}"), Style::strong(CPU_C));
        line.push(&format!(" · {}C/{}T", cores.len(), threads), Style::fg(DIM));
        line
    };
    let title_short = Line::from_text(&format!("CPU {package}"), Style::strong(CPU_C));
    let mut title = title_short.clone();
    for candidate in [&title_full, &title_medium, &title_short] {
        if width.saturating_sub(2 + candidate.width() + 8) >= 10 {
            title = candidate.clone();
            break;
        }
    }

    let color = value_color(load / 100.0);
    let make_right = |with_temp: bool, with_freq: bool| -> Line {
        let mut line = Line::from_text(&pct(load), Style::strong(color));
        if with_temp {
            if let Some(value) = temp {
                line.push(" · ", Style::fg(DIM_SOFT));
                line.push(
                    &format!("{value:.0}°C"),
                    Style::strong(temp_color(Some(value))),
                );
            }
        }
        if with_freq {
            if let Some(value) = freq {
                line.push(" · ", Style::fg(DIM_SOFT));
                line.push(&format!("{value:.2} GHz"), Style::fg(DIM));
            }
        }
        line
    };
    let mut right_candidates = Vec::new();
    if temp.is_some() && freq.is_some() {
        right_candidates.push(make_right(true, true));
    }
    if temp.is_some() {
        right_candidates.push(make_right(true, false));
    }
    if freq.is_some() {
        right_candidates.push(make_right(false, true));
    }
    right_candidates.push(make_right(false, false));
    let max_right = width.saturating_sub(2 + title.width() + 8);
    let right = choose(right_candidates, max_right);
    card(width, 12, &title, right.as_ref(), body, glyphs)
}

fn gpu_body(gpu: &Gpu, content_width: usize, glyphs: &Glyphs) -> Vec<Line> {
    let value_width = 13usize;
    let bar_width = 6.max(content_width.saturating_sub(6 + value_width + 2 + 4));
    let mut rows = Vec::new();

    let util = gpu.util;
    let mut row = Line::new();
    row.push("UTIL", Style::strong(GPU_C));
    row.push("  ", Style::default());
    row.extend(&bar(util.unwrap_or(0.0) / 100.0, bar_width, None, glyphs));
    let util_label = util.map(pct).unwrap_or_else(|| "  —%".to_string());
    row.push(
        &format!(" {util_label}"),
        Style::strong(value_color(util.unwrap_or(0.0) / 100.0)),
    );
    if let Some(fan) = gpu.fan {
        let fan_text = format!("{fan:.0}% fan");
        let padding = content_width as i64
            - row.width() as i64
            - UnicodeWidthStr::width(fan_text.as_str()) as i64;
        if padding > 0 {
            row.push(&" ".repeat(padding as usize), Style::default());
            row.push(&fan_text, Style::fg(DIM));
        }
    }
    row.pad_to(content_width);
    rows.push(row);

    let mem_value = format!("{}/{} Gio", fmt_gio(gpu.mem_used), fmt_gio(gpu.mem_total));
    let mut row = Line::new();
    row.push("VRAM", Style::strong(GPU_C));
    row.push("  ", Style::default());
    row.extend(&bar(gpu.mem_fraction(), bar_width, None, glyphs));
    row.push(&format!(" {mem_value}"), Style::fg(TEXT));
    let tail = pct(gpu.mem_fraction() * 100.0);
    let padding =
        content_width as i64 - row.width() as i64 - UnicodeWidthStr::width(tail.as_str()) as i64;
    row.push(&" ".repeat(padding.max(1) as usize), Style::default());
    row.push(&tail, Style::strong(value_color(gpu.mem_fraction())));
    row.pad_to(content_width);
    rows.push(row);

    let mut row = Line::new();
    row.push("PWR", Style::strong(GPU_C));
    row.push("   ", Style::default());
    if let Some(power) = gpu.power {
        row.extend(&bar(gpu.power_fraction(), bar_width, None, glyphs));
        let value = match gpu.power_limit {
            Some(limit) => format!("{power:.0}/{limit:.0} W"),
            None => format!("{power:.0} W"),
        };
        row.push(&format!(" {value}"), Style::fg(TEXT));
        if gpu.power_limit.is_some() {
            let tail = pct(gpu.power_fraction() * 100.0);
            let padding = content_width as i64
                - row.width() as i64
                - UnicodeWidthStr::width(tail.as_str()) as i64;
            row.push(&" ".repeat(padding.max(1) as usize), Style::default());
            row.push(&tail, Style::strong(value_color(gpu.power_fraction())));
        }
    } else {
        row.push("—", Style::fg(DIM));
    }
    row.pad_to(content_width);
    rows.push(row);

    let mut row = Line::new();
    row.push("CLK", Style::strong(GPU_C));
    if let Some(clock) = gpu.clock {
        row.push(&format!("   {clock:.0} MHz"), Style::fg(TEXT));
    }
    if let Some(mem_clock) = gpu.mem_clock {
        row.push(&format!(" · MEM {mem_clock:.0} MHz"), Style::fg(DIM));
    }
    row.pad_to(content_width);
    rows.push(row);
    rows
}

fn gpu_card(gpu: &Gpu, width: usize, height: usize, glyphs: &Glyphs) -> Vec<Line> {
    let content_width = width.saturating_sub(4);
    let mut title = Line::from_text(&format!("GPU {}", gpu.index), Style::strong(GPU_C));
    let name_room = title.width() + 3 + UnicodeWidthStr::width(gpu.name.as_str()) + 1;
    if name_room <= width.saturating_sub(14) {
        title.push(&format!(" · {}", gpu.name), Style::fg(DIM));
    }
    let mut right = Line::new();
    match gpu.temp {
        Some(temp) => right.push(
            &format!("{temp:.0}°C"),
            Style::strong(temp_color(Some(temp))),
        ),
        None => right.push("—", Style::fg(DIM)),
    }
    let mut body = gpu_body(gpu, content_width, glyphs);
    if height < 6 {
        body.truncate(height.saturating_sub(2).max(1));
    }
    card(width, height, &title, Some(&right), body, glyphs)
}

fn gpu_placeholder(width: usize, height: usize, message: &str, glyphs: &Glyphs) -> Vec<Line> {
    let title = Line::from_text("GPU", Style::strong(GPU_C));
    let body = vec![Line::from_text(
        &fit(message, width.saturating_sub(6)),
        Style::fg(ORANGE),
    )];
    card(width, height, &title, None, body, glyphs)
}

struct HistoryEntry<'a> {
    label: String,
    series: &'a [f64],
    color: Rgb,
    current: f64,
    alerts: Option<&'a [bool]>,
}

fn history_block(
    m: &Metrics,
    sampler: &Sampler,
    width: usize,
    rows: usize,
    glyphs: &Glyphs,
) -> Vec<Line> {
    let content_width = width.saturating_sub(4);
    let mut entries: Vec<HistoryEntry> = Vec::new();
    if rows >= 6 && m.socket_pct.len() >= 2 {
        for &package in m.socket_pct.keys() {
            entries.push(HistoryEntry {
                label: format!("CPU {package}"),
                series: sampler
                    .history
                    .socket
                    .get(&package)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                color: CPU_C,
                current: *m.socket_pct.get(&package).unwrap_or(&0.0),
                alerts: Some(
                    sampler
                        .history
                        .socket_saturated
                        .get(&package)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                ),
            });
        }
    } else {
        entries.push(HistoryEntry {
            label: "CPU".to_string(),
            series: &sampler.history.cpu,
            color: CPU_C,
            current: m.cpu_total,
            alerts: Some(&sampler.history.saturated),
        });
    }
    for gpu in m.gpus.iter().take(2) {
        entries.push(HistoryEntry {
            label: format!("GPU {}", gpu.index),
            series: sampler
                .history
                .gpu
                .get(&gpu.index)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            color: GPU_C,
            current: gpu.util.unwrap_or(0.0),
            alerts: None,
        });
    }
    entries.push(HistoryEntry {
        label: "RAM".to_string(),
        series: &sampler.history.ram,
        color: GREEN,
        current: m.ram_pct,
        alerts: None,
    });
    entries.push(HistoryEntry {
        label: "VRAM".to_string(),
        series: &sampler.history.vram,
        color: YELLOW,
        current: m.vram_pct,
        alerts: None,
    });
    entries.truncate(rows);

    let spark_width = 4.max(content_width.saturating_sub(12));
    let mut body = Vec::new();
    for entry in &entries {
        let mut row = Line::new();
        row.push(&format!("{:<6}", entry.label), Style::strong(entry.color));
        row.extend(&sparkline(
            entry.series,
            spark_width,
            entry.color,
            entry.alerts,
            glyphs,
        ));
        row.push(
            &format!(" {}", pct(entry.current)),
            Style::strong(entry.color),
        );
        let current_alert = entry
            .alerts
            .and_then(|alerts| alerts.last())
            .copied()
            .unwrap_or(false);
        if current_alert {
            row.push("!", Style::strong(RED));
        } else {
            row.push(" ", Style::default());
        }
        row.pad_to(content_width);
        body.push(row);
    }

    let title = Line::from_text("HISTORIQUE", Style::strong(TITLE));
    let window = sampler.history.cpu.len().min(spark_width) as f64 * sampler.interval.as_secs_f64();
    let right = Line::from_text(&format!("{window:.0} s"), Style::fg(DIM));
    card(width, entries.len() + 2, &title, Some(&right), body, glyphs)
}

fn append_parts(row: &mut Line, parts: &[(String, Rgb)]) {
    for (index, (text, style)) in parts.iter().enumerate() {
        if index > 0 {
            row.push(" · ", Style::fg(DIM_SOFT));
        }
        row.push(text, Style::fg(*style));
    }
}

fn pcie_log_fraction(mb_s: Option<f64>) -> f64 {
    const FLOOR: f64 = 1.0;
    const CEIL: f64 = 10_000.0;
    match mb_s {
        Some(value) if value > FLOOR => {
            ((value / FLOOR).ln() / (CEIL / FLOOR).ln()).clamp(0.0, 1.0)
        }
        _ => 0.0,
    }
}

fn push_row(rows: &mut Vec<Line>, mut row: Line, content_width: usize) {
    row.pad_to(content_width);
    rows.push(row);
}

/// Barres PCIe par lien, puis NVLink/QPI et tensor/AMD (5 lignes).
fn links_block(m: &Metrics, width: usize, glyphs: &Glyphs) -> Vec<Line> {
    let content_width = width.saturating_sub(4).max(10);
    let compact = content_width < 100;
    let compact_pcie = content_width < 88;
    let wide_amd = content_width >= 150;
    let bar_width = content_width.saturating_sub(46).clamp(6, 30);
    let mut rows = Vec::new();

    // --- PCIe : une barre par lien, TX en haut et RX en bas -------------
    for gpu in m.gpus.iter().take(2) {
        let generation = match (gpu.pcie_gen, gpu.pcie_gen_max) {
            (Some(current), Some(maximum)) if !compact_pcie && current != maximum => {
                format!("Gen{current}/{maximum}")
            }
            (Some(current), _) => format!("Gen{current}"),
            _ => "Gen?".to_string(),
        };
        let lane = match (gpu.pcie_width, gpu.pcie_width_max) {
            (Some(current), Some(maximum)) if !compact_pcie && current != maximum => {
                format!("x{current}/{maximum}")
            }
            (Some(current), _) => format!("x{current}"),
            _ => "x?".to_string(),
        };
        let desc = if compact_pcie {
            format!("G{} {generation}{lane}", gpu.index)
        } else {
            format!("GPU {} {generation} {lane}", gpu.index)
        };
        let mut row = Line::from_text("PCIE   ", Style::strong(TITLE));
        row.push(&format!("{desc} "), Style::fg(TEXT));
        row.extend(&paired_bar_colors(
            pcie_log_fraction(gpu.pcie_tx_mb_s),
            pcie_log_fraction(gpu.pcie_rx_mb_s),
            bar_width,
            glyphs,
            TITLE,
            GREEN,
        ));
        row.push(" ", Style::fg(TRACK));
        match (gpu.pcie_tx_mb_s, gpu.pcie_rx_mb_s) {
            (Some(tx), Some(rx)) => {
                row.push(glyphs.pair_top(), Style::strong(TITLE));
                row.push(&format!("{tx:.1} "), Style::fg(TEXT));
                row.push(glyphs.pair_bottom(), Style::strong(GREEN));
                row.push(&format!("{rx:.1} Mo/s"), Style::fg(TEXT));
            }
            _ => row.push("débit n/a", Style::fg(DIM)),
        }
        push_row(&mut rows, row, content_width);
    }

    // --- PCIe AMD : lien connu, débit non exposé ------------------------
    let mut amd_row = Line::from_text("PCIE   ", Style::strong(TITLE));
    let mut amd_link = false;
    if let Some(amd) = &m.amd {
        if let Some(generation) = amd.pcie_gen {
            let lane = amd
                .pcie_width
                .map(|value| format!("x{value}"))
                .unwrap_or_default();
            amd_row.push("AMD ", Style::strong(ORANGE));
            amd_row.push(
                &format!(
                    "Gen{generation}{}{lane} ",
                    if compact_pcie { "" } else { " " }
                ),
                Style::fg(TEXT),
            );
            amd_row.extend(&paired_bar_colors(
                0.0, 0.0, bar_width, glyphs, TITLE, GREEN,
            ));
            amd_row.push(" débit n/a", Style::fg(DIM));
            amd_link = true;
        }
    }
    if !amd_link {
        amd_row.push("aucune carte AMD détectée", Style::fg(DIM));
    }
    push_row(&mut rows, amd_row, content_width);

    // --- NVLink + QPI ---------------------------------------------------
    let mut nvlink: Vec<(String, Rgb)> = Vec::new();
    if let Some(speed) = m.nvlink.speed_gbps {
        if compact {
            nvlink.push((format!("{speed:.1}Go/s"), TEXT));
            nvlink.push((format!("{}lien", m.nvlink.links), DIM));
        } else {
            nvlink.push((format!("{speed:.3} Go/s"), TEXT));
            nvlink.push((format!("{} lien actif", m.nvlink.links), DIM));
        }
        if m.nvlink.errors > 0 {
            nvlink.push((
                if compact {
                    format!("{}err", m.nvlink.errors)
                } else {
                    format!("{} erreur(s)", m.nvlink.errors)
                },
                RED,
            ));
        } else {
            nvlink.push(((if compact { "0err" } else { "0 erreur" }).to_string(), DIM));
        }
        if let Some(throughput) = m.nvlink.throughput {
            nvlink.push((
                if compact {
                    format!("{throughput:.1}Go/s")
                } else {
                    format!("{throughput:.1} Go/s")
                },
                TEXT,
            ));
        } else {
            nvlink.push(((if compact { "n/a" } else { "débit n/a" }).to_string(), DIM));
        }
    } else {
        nvlink.push((
            (if compact {
                "aucun lien actif"
            } else {
                "aucun lien NVLink actif"
            })
            .to_string(),
            DIM,
        ));
    }

    let mut qpi: Vec<(String, Rgb)> = Vec::new();
    if compact {
        qpi.push((format!("{}s", m.qpi.sockets), TEXT));
        if let Some(links) = m.qpi.links {
            qpi.push((format!("{links}liens"), TEXT));
        }
        if let Some(speed) = m.qpi.speed_gt_s {
            qpi.push((format!("{speed:.1}GT/s"), TEXT));
        }
        if let Some(traffic) = m.qpi.traffic {
            qpi.push((format!("{traffic:.1}Go/s"), TEXT));
        } else {
            qpi.push(("n/a".to_string(), DIM));
        }
    } else {
        qpi.push((format!("{} sockets", m.qpi.sockets), TEXT));
        if let Some(links) = m.qpi.links {
            qpi.push((format!("{links} liens"), TEXT));
        }
        if let Some(speed) = m.qpi.speed_gt_s {
            qpi.push((format!("{speed:.1} GT/s"), TEXT));
        }
        if let Some(traffic) = m.qpi.traffic {
            qpi.push((format!("{traffic:.1} Go/s"), TEXT));
        } else if let Some(reason) = &m.qpi.reason {
            qpi.push((format!("n/a ({reason})"), DIM));
        } else {
            qpi.push(("n/a".to_string(), DIM));
        }
    }

    let mut nvqpi = Line::from_text("NVLINK ", Style::strong(GPU_C));
    append_parts(&mut nvqpi, &nvlink);
    nvqpi.push("  ", Style::fg(DIM_SOFT));
    nvqpi.push("QPI ", Style::strong(CPU_C));
    append_parts(&mut nvqpi, &qpi);
    push_row(&mut rows, nvqpi, content_width);

    // --- Tensor + AMD ---------------------------------------------------
    let mut tensor: Vec<(String, Rgb)> = Vec::new();
    for gpu in m.gpus.iter().filter(|gpu| gpu.tensor.is_some()) {
        tensor.push((
            format!("GPU{} {:.0}%", gpu.index, gpu.tensor.unwrap_or(0.0)),
            TEXT,
        ));
    }
    if tensor.is_empty() {
        tensor.push((
            (if compact {
                "n/a"
            } else {
                "n/a (GPM non supporté, DCGM absent)"
            })
            .to_string(),
            DIM,
        ));
    }

    let mut amd_parts: Vec<(String, Rgb)> = Vec::new();
    if let Some(amd) = &m.amd {
        if wide_amd {
            amd_parts.push((amd.name.clone(), TEXT));
        }
        if let Some(temp) = amd.temp {
            amd_parts.push((format!("{temp:.0}°C"), TEXT));
        }
        if compact {
            amd_parts.push((
                format!("{}/{}G", fmt_gio(amd.vram_used), fmt_gio(amd.vram_total)),
                TEXT,
            ));
        } else {
            amd_parts.push((
                format!("{}/{} Gio", fmt_gio(amd.vram_used), fmt_gio(amd.vram_total)),
                TEXT,
            ));
        }
        if let Some(sclk) = amd.sclk_mhz {
            let clocks = match amd.mclk_mhz {
                Some(mclk) => format!("{sclk:.0}/{mclk:.0}"),
                None => format!("{sclk:.0}"),
            };
            amd_parts.push((
                format!("{clocks}{}", if compact { "MHz" } else { " MHz" }),
                TEXT,
            ));
        }
        if let Some(fan) = amd.fan_pct {
            amd_parts.push((
                if compact {
                    format!("{fan:.0}%")
                } else {
                    format!("vent. {fan:.0}%")
                },
                DIM,
            ));
        }
        if wide_amd {
            if let Some(pstate) = &amd.pstate {
                amd_parts.push((pstate.clone(), DIM));
            }
        }
        if let Some(busy) = amd.busy {
            amd_parts.push((
                if compact {
                    format!("act.{busy:.0}%")
                } else {
                    format!("activité {busy:.0}%")
                },
                TEXT,
            ));
        } else {
            amd_parts.push((
                (if compact { "act.n/a" } else { "activité n/a" }).to_string(),
                DIM,
            ));
        }
    } else {
        amd_parts.push(("aucune carte AMD détectée".to_string(), DIM));
    }

    let mut tensor_amd = Line::from_text("TENSOR ", Style::strong(GPU_C));
    append_parts(&mut tensor_amd, &tensor);
    tensor_amd.push("  ", Style::fg(DIM_SOFT));
    tensor_amd.push("AMD ", Style::strong(ORANGE));
    append_parts(&mut tensor_amd, &amd_parts);
    push_row(&mut rows, tensor_amd, content_width);

    let title = Line::from_text("LIAISONS", Style::strong(TITLE));
    let right = if compact {
        Line::from_text("PCIe log", Style::fg(DIM))
    } else {
        let mut line = Line::from_text("PCIe log · ", Style::fg(DIM));
        line.push(glyphs.pair_top(), Style::strong(TITLE));
        line.push("TX ", Style::fg(DIM));
        line.push(glyphs.pair_bottom(), Style::strong(GREEN));
        line.push("RX", Style::fg(DIM));
        line
    };
    card(width, 7, &title, Some(&right), rows, glyphs)
}

fn footer_block(
    width: usize,
    m: &Metrics,
    interval: f64,
    samples: usize,
    gpu_ms: f64,
    paused: bool,
) -> Line {
    let mut left = Line::new();
    if paused {
        left.push("PAUSE", Style::strong(ORANGE));
        left.push("   ", Style::fg(DIM));
    }
    left.push("q", Style::strong(TEXT));
    left.push(" quitter", Style::fg(DIM));
    left.push("   ", Style::fg(DIM));
    left.push("p", Style::strong(TEXT));
    left.push(" pause", Style::fg(DIM));
    left.push("   ", Style::fg(DIM));
    left.push("r", Style::strong(TEXT));
    left.push(" reset", Style::fg(DIM));

    let mut right = if let Some(error) = &m.gpu_error {
        let message = if error.to_lowercase().starts_with("nvidia-smi") {
            error.clone()
        } else {
            format!("nvidia-smi : {error}")
        };
        Line::from_text(&fit(&message, 10.max(width / 2)), Style::fg(ORANGE))
    } else {
        Line::from_text(
            &format!(
                "intervalle {:.1} s  ·  nvidia-smi {:.0} ms  ·  {} éch.",
                interval, gpu_ms, samples
            ),
            Style::fg(DIM),
        )
    };
    let mut gap = width as i64 - left.width() as i64 - right.width() as i64;
    if gap <= 0 && m.gpu_error.is_none() {
        right = Line::from_text(
            &format!("{:.1} s · {:.0} ms", interval, gpu_ms),
            Style::fg(DIM),
        );
        gap = width as i64 - left.width() as i64 - right.width() as i64;
    }
    let mut line = left;
    if gap > 0 {
        line.push(&" ".repeat(gap as usize), Style::default());
        line.extend(&right);
    }
    line.pad_to(width);
    line
}

fn too_small(width: usize, height: usize, glyphs: &Glyphs) -> Vec<Line> {
    let title = Line::from_text("hauru-top", Style::strong(TITLE));
    let body = vec![
        Line::from_text(
            &format!(
                "terminal trop petit : {}×{} — minimum {}×{}",
                width, height, MIN_WIDTH, MIN_HEIGHT
            ),
            Style::fg(ORANGE),
        ),
        Line::from_text(
            "agrandir la fenêtre (ou --width / --height)",
            Style::fg(DIM),
        ),
        Line::from_text("q pour quitter", Style::fg(DIM)),
    ];
    let card_width = width.min(64);
    let card_height = height.clamp(5, 7);
    let body = body.into_iter().take(card_height - 2).collect();
    let lines = card(card_width, card_height, &title, None, body, glyphs);
    let left = width.saturating_sub(card_width) / 2;
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|line| {
            let mut prefixed = Line::spaces(left);
            prefixed.extend(&line);
            prefixed
        })
        .collect();
    let top = height.saturating_sub(lines.len()) / 2;
    let mut result = vec![Line::blank(width); top];
    result.extend(lines);
    result
}

/// Assemble la frame complète, exactement `height` lignes de `width` colonnes.
pub fn build_frame(
    sampler: &Sampler,
    width: usize,
    height: usize,
    paused: bool,
    clock: &str,
    glyphs: &Glyphs,
    app_title: &str,
) -> Vec<Line> {
    let lines = if width < MIN_WIDTH || height < MIN_HEIGHT {
        too_small(width, height, glyphs)
    } else {
        let Some(m) = sampler.metrics.as_ref() else {
            return vec![Line::blank(width); height];
        };
        let gpu_height = if height >= 24 { 6 } else { 5 };
        let mut blocks: Vec<Vec<Line>> = Vec::new();
        let dot = if m.gpu_error.is_some() || m.gpus.is_empty() {
            ORANGE
        } else {
            GREEN
        };
        blocks.push(header_block(m, width, clock, dot, glyphs, app_title));

        let half_left = (width - 2) / 2;
        let half_right = width - 2 - half_left;
        if sampler.topology.packages.len() > 1 {
            let left = cpu_card(sampler.topology.packages[0], m, sampler, half_left, glyphs);
            let right = cpu_card(sampler.topology.packages[1], m, sampler, half_right, glyphs);
            blocks.push(side_by_side(&left, &right, 2));
        } else {
            blocks.push(cpu_card(
                sampler.topology.packages[0],
                m,
                sampler,
                width,
                glyphs,
            ));
        }

        if m.gpus.len() >= 2 {
            let left = gpu_card(&m.gpus[0], half_left, gpu_height, glyphs);
            let right = gpu_card(&m.gpus[1], half_right, gpu_height, glyphs);
            blocks.push(side_by_side(&left, &right, 2));
        } else if m.gpus.len() == 1 {
            blocks.push(gpu_card(&m.gpus[0], width, gpu_height, glyphs));
        } else {
            let message = m
                .gpu_error
                .clone()
                .unwrap_or_else(|| "aucun GPU NVIDIA interrogé".to_string());
            blocks.push(gpu_placeholder(width, gpu_height, &message, glyphs));
        }

        let links_height = if height >= 33 { 7 } else { 0 };
        if links_height > 0 {
            blocks.push(links_block(m, width, glyphs));
        }

        let base_height = blocks.iter().map(Vec::len).sum::<usize>() + 1;
        let reserved_gaps = if links_height > 0 { 5 } else { 4 };
        let max_history = height as isize - base_height as isize - reserved_gaps as isize;
        let history_rows = if max_history >= 8 {
            6
        } else if max_history >= 7 {
            5
        } else if max_history >= 6 {
            4
        } else if max_history >= 5 {
            3
        } else if max_history >= 4 {
            2
        } else if max_history >= 3 {
            1
        } else {
            0
        };
        if history_rows > 0 {
            blocks.push(history_block(m, sampler, width, history_rows, glyphs));
        }

        let total = blocks.iter().map(Vec::len).sum::<usize>() + 1;
        let available_gaps = height.saturating_sub(total);
        let gaps = blocks.len().min(available_gaps);
        let mut result: Vec<Line> = Vec::new();
        let mut remaining_gaps = gaps;
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 && remaining_gaps > 0 {
                result.push(Line::blank(width));
                remaining_gaps -= 1;
            }
            result.extend(block.iter().cloned());
        }
        if remaining_gaps > 0 {
            result.push(Line::blank(width));
        }
        let room = height as isize - result.len() as isize - 1;
        if (0..=8).contains(&room) {
            while result.len() < height - 1 {
                result.push(Line::blank(width));
            }
        }
        result.push(footer_block(
            width,
            m,
            sampler.interval.as_secs_f64(),
            sampler.samples,
            sampler.gpu_latency_ms,
            paused,
        ));
        result
    };

    let mut final_lines = Vec::with_capacity(height);
    let mut source = lines.into_iter();
    for _ in 0..height {
        if let Some(mut line) = source.next() {
            line.pad_to(width);
            final_lines.push(line);
        } else {
            final_lines.push(Line::blank(width));
        }
    }
    final_lines
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    True,
    Ansi256,
    Ansi16,
    None,
}

fn nearest_level(value: u8) -> usize {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    LEVELS
        .iter()
        .enumerate()
        .min_by_key(|(_, level)| ((value as i32) - (**level as i32)).abs())
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn color_256(color: Rgb) -> u8 {
    let (red, green, blue) = (color.0, color.1, color.2);
    if red == green && green == blue {
        if red < 8 {
            return 16;
        }
        if red > 248 {
            return 231;
        }
        return 232 + ((red as u16 - 8) / 10).min(23) as u8;
    }
    let r = nearest_level(red) as u8;
    let g = nearest_level(green) as u8;
    let b = nearest_level(blue) as u8;
    16 + 36 * r + 6 * g + b
}

const ANSI16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (128, 0, 0),
    (0, 128, 0),
    (128, 128, 0),
    (0, 0, 128),
    (128, 0, 128),
    (0, 128, 128),
    (192, 192, 192),
    (128, 128, 128),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (0, 0, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

fn color_16(color: Rgb) -> u8 {
    ANSI16
        .iter()
        .enumerate()
        .min_by_key(|(_, (r, g, b))| {
            let dr = color.0 as i32 - *r as i32;
            let dg = color.1 as i32 - *g as i32;
            let db = color.2 as i32 - *b as i32;
            dr * dr + dg * dg + db * db
        })
        .map(|(index, _)| index as u8)
        .unwrap_or(7)
}

fn color_code(color: Rgb, mode: ColorMode, background: bool) -> String {
    match mode {
        ColorMode::True => {
            if background {
                format!("48;2;{};{};{}", color.0, color.1, color.2)
            } else {
                format!("38;2;{};{};{}", color.0, color.1, color.2)
            }
        }
        ColorMode::Ansi256 => {
            let index = color_256(color);
            if background {
                format!("48;5;{index}")
            } else {
                format!("38;5;{index}")
            }
        }
        ColorMode::Ansi16 => {
            let index = color_16(color);
            if background {
                if index < 8 {
                    format!("{}", 40 + index)
                } else {
                    format!("{}", 100 + index - 8)
                }
            } else if index < 8 {
                format!("{}", 30 + index)
            } else {
                format!("{}", 90 + index - 8)
            }
        }
        ColorMode::None => String::new(),
    }
}

fn sgr(style: Style, mode: ColorMode) -> String {
    let mut params = Vec::new();
    if style.bold {
        params.push("1".to_string());
    }
    if let Some(fg) = style.fg {
        params.push(color_code(fg, mode, false));
    }
    if let Some(bg) = style.bg {
        params.push(color_code(bg, mode, true));
    }
    if params.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", params.join(";"))
    }
}

pub fn serialize_line(line: &Line, mode: ColorMode) -> String {
    if mode == ColorMode::None {
        return line
            .spans
            .iter()
            .map(|span| span.text.as_str())
            .collect::<String>();
    }
    let mut output = String::new();
    let mut current = Style::default();
    for span in &line.spans {
        if span.style != current {
            if current != Style::default() {
                output.push_str("\x1b[0m");
            }
            if span.style != Style::default() {
                output.push_str(&sgr(span.style, mode));
            }
            current = span.style;
        }
        output.push_str(&span.text);
    }
    if current != Style::default() {
        output.push_str("\x1b[0m");
    }
    output
}

pub fn render_live(lines: &[Line], mode: ColorMode) -> String {
    let mut output = String::from("\x1b[H");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            output.push_str("\r\n");
        }
        output.push_str(&serialize_line(line, mode));
    }
    output
}

pub fn render_once(lines: &[Line], mode: ColorMode) -> String {
    lines
        .iter()
        .map(|line| serialize_line(line, mode))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_have_exact_width() {
        let glyphs = Glyphs { unicode: true };
        for width in 1..40 {
            assert_eq!(bar(0.37, width, None, &glyphs).width(), width);
            assert_eq!(paired_bar(0.2, 0.9, width, &glyphs).width(), width);
        }
    }

    #[test]
    fn sparkline_has_exact_width() {
        let glyphs = Glyphs { unicode: true };
        let values: Vec<f64> = (0..100).map(|value| value as f64).collect();
        for width in 0..40 {
            assert_eq!(
                sparkline(&values, width, CPU_C, None, &glyphs).width(),
                width
            );
        }
    }

    #[test]
    fn percentages_are_aligned() {
        assert_eq!(pct(0.0), "  0%");
        assert_eq!(pct(42.0), " 42%");
        assert_eq!(pct(100.0), "100%");
    }

    #[test]
    fn fit_never_exceeds_width() {
        for width in 0..20 {
            assert!(UnicodeWidthStr::width(fit("abcdefghijklmnop", width).as_str()) <= width);
        }
    }

    #[test]
    fn load_gradient_endpoints() {
        assert_eq!(load_color(0.0), LOAD_STOPS[0].1);
        assert_eq!(load_color(1.0), LOAD_STOPS[LOAD_STOPS.len() - 1].1);
        assert_ne!(load_color(0.0), load_color(1.0));
    }

    #[test]
    fn unicode_widths_of_glyphs() {
        let glyphs = Glyphs { unicode: true };
        assert_eq!(UnicodeWidthStr::width(glyphs.pair_top()), 1);
        assert_eq!(UnicodeWidthStr::width(glyphs.pair_bottom()), 1);
        assert_eq!(UnicodeWidthStr::width(glyphs.h()), 1);
        assert_eq!(UnicodeWidthStr::width(glyphs.dot()), 1);
    }
}
