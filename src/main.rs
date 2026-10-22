//! hauru-top — tableau de bord terminal pour station Linux.

mod model;
mod sys;
mod ui;

use crate::sys::Sampler;
use crate::ui::{ColorMode, Glyphs};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use signal_hook::consts::{SIGHUP, SIGTERM};
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const APP: &str = "hauru-top";
const VERSION: &str = "1.3.0";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

struct Args {
    interval: f64,
    once: bool,
    width: Option<u16>,
    height: Option<u16>,
    color: ColorChoice,
    ascii: bool,
    title: Option<String>,
    privacy: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            interval: 1.0,
            once: false,
            width: None,
            height: None,
            color: ColorChoice::Auto,
            ascii: false,
            title: None,
            privacy: false,
        }
    }
}

fn print_help() {
    println!(
        "{APP} {VERSION} (rust)\n\
         Tableau de bord terminal : cœurs physiques (HT), GPU, RAM/VRAM.\n\n\
         USAGE:\n    {APP} [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   -i, --interval <SECONDES>  période d'échantillonnage (défaut : 1.0)\n\
         \x20       --once                n'affiche qu'une image puis quitte\n\
         \x20       --width <COLONNES>     largeur forcée\n\
         \x20       --height <LIGNES>      hauteur forcée\n\
         \x20       --color <MODE>         auto, always ou never\n\
         \x20       --ascii               bordures et barres en ASCII\n\
         \x20       --title <NOM>         nom affiché (défaut : nom d'hôte)\n\
         \x20       --privacy             masque utilisateurs et arguments\n\
         \x20   -h, --help                cette aide\n\
         \x20       --version             version\n\n\
         TOUCHES:\n\
         \x20   q quitter · p pause · r reset de l'historique"
    );
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let (flag, inline) = match argument.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (argument.clone(), None),
        };
        match flag.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--version" => {
                println!("{APP} {VERSION} (rust)");
                std::process::exit(0);
            }
            "--once" => args.once = true,
            "--ascii" => args.ascii = true,
            "--privacy" => args.privacy = true,
            "--title" => {
                args.title = Some(
                    inline
                        .or_else(|| arguments.next())
                        .ok_or_else(|| "valeur manquante pour --title".to_string())?,
                );
            }
            "-i" | "--interval" => {
                let value = inline
                    .or_else(|| arguments.next())
                    .ok_or_else(|| "valeur manquante pour --interval".to_string())?;
                args.interval = value
                    .parse()
                    .map_err(|_| format!("intervalle invalide : {value}"))?;
            }
            "--width" => {
                let value = inline
                    .or_else(|| arguments.next())
                    .ok_or_else(|| "valeur manquante pour --width".to_string())?;
                args.width = Some(
                    value
                        .parse()
                        .map_err(|_| format!("largeur invalide : {value}"))?,
                );
            }
            "--height" => {
                let value = inline
                    .or_else(|| arguments.next())
                    .ok_or_else(|| "valeur manquante pour --height".to_string())?;
                args.height = Some(
                    value
                        .parse()
                        .map_err(|_| format!("hauteur invalide : {value}"))?,
                );
            }
            "--color" => {
                let value = inline
                    .or_else(|| arguments.next())
                    .ok_or_else(|| "valeur manquante pour --color".to_string())?;
                args.color = match value.as_str() {
                    "auto" => ColorChoice::Auto,
                    "always" => ColorChoice::Always,
                    "never" => ColorChoice::Never,
                    other => return Err(format!("couleur invalide : {other}")),
                };
            }
            other => return Err(format!("argument inconnu : {other}")),
        }
    }
    if !(0.2..=30.0).contains(&args.interval) {
        return Err("--interval doit être compris entre 0.2 et 30 secondes".to_string());
    }
    if let Some(width) = args.width {
        if width < 20 {
            return Err("--width doit valoir au moins 20".to_string());
        }
    }
    if let Some(height) = args.height {
        if height < 5 {
            return Err("--height doit valoir au moins 5".to_string());
        }
    }
    Ok(args)
}

fn detect_color(choice: ColorChoice) -> ColorMode {
    match choice {
        ColorChoice::Always => ColorMode::True,
        ColorChoice::Never => ColorMode::None,
        ColorChoice::Auto => {
            if !io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some() {
                return ColorMode::None;
            }
            let term = std::env::var("TERM").unwrap_or_default();
            if term == "dumb" {
                return ColorMode::None;
            }
            let color_term = std::env::var("COLORTERM")
                .unwrap_or_default()
                .to_lowercase();
            if color_term.contains("truecolor") || color_term.contains("24bit") {
                ColorMode::True
            } else if term.contains("256color") {
                ColorMode::Ansi256
            } else {
                ColorMode::Ansi16
            }
        }
    }
}

fn terminal_size(args: &Args) -> (usize, usize) {
    let (terminal_width, terminal_height) = crossterm::terminal::size()
        .ok()
        .filter(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or((80, 24));
    (
        args.width.unwrap_or(terminal_width).max(20) as usize,
        args.height.unwrap_or(terminal_height).max(5) as usize,
    )
}

fn local_clock() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return "--:--:--".to_string();
        }
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

fn restore_terminal() {
    let mut out = io::stdout();
    let _ = write!(out, "\x1b[?7h\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
    let _ = disable_raw_mode();
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        let result =
            write!(out, "\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[2J\x1b[H").and_then(|_| out.flush());
        if let Err(error) = result {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

fn draw(frame: &[ui::Line], color: ColorMode) -> io::Result<()> {
    let output = ui::render_live(frame, color);
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.flush()
}

fn run_once(
    mut sampler: Sampler,
    args: &Args,
    glyphs: &Glyphs,
    color: ColorMode,
    title: &str,
) -> io::Result<()> {
    sampler.prime();
    let (width, height) = terminal_size(args);
    let clock = local_clock();
    let frame = ui::build_frame(&sampler, width, height, false, &clock, glyphs, title);
    print!("{}", ui::render_once(&frame, color));
    println!();
    if let Some(error) = sampler.metrics.as_ref().and_then(|m| m.gpu_error.as_ref()) {
        eprintln!("{APP}: {error}");
    }
    Ok(())
}

fn run_live(
    mut sampler: Sampler,
    args: &Args,
    glyphs: &Glyphs,
    color: ColorMode,
    title: &str,
) -> io::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(SIGTERM, Arc::clone(&stop));
    let _ = signal_hook::flag::register(SIGHUP, Arc::clone(&stop));

    sampler.prime();
    let _terminal = TerminalGuard::enter()?;

    let mut paused = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let (width, height) = terminal_size(args);
        let frame = ui::build_frame(
            &sampler,
            width,
            height,
            paused,
            &local_clock(),
            glyphs,
            title,
        );
        draw(&frame, color)?;

        let deadline = Instant::now() + sampler.interval;
        let mut redraw = false;
        while Instant::now() < deadline {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let wait = (deadline - now).min(Duration::from_millis(200));
            match event::poll(wait) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(()),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(())
                            }
                            KeyCode::Char('p') | KeyCode::Char('P') | KeyCode::Char(' ') => {
                                paused = !paused;
                                if !paused {
                                    sampler.reset_cpu_baseline();
                                }
                                redraw = true;
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                sampler.history.clear();
                                sampler.samples = 0;
                                redraw = true;
                            }
                            _ => {}
                        }
                        if redraw {
                            break;
                        }
                    }
                    Ok(Event::Resize(_, _)) => {
                        redraw = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error),
                },
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        if redraw {
            continue;
        }
        if !paused {
            sampler.sample(true);
        }
    }
    Ok(())
}

fn default_title() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| APP.to_string())
}

/// Nettoie et borne le nom affiché dans l'en-tête.
fn display_title(text: &str) -> String {
    let cleaned: String = text.chars().filter(|ch| !ch.is_control()).collect();
    let cleaned = cleaned.trim();
    let source = if cleaned.is_empty() { APP } else { cleaned };
    let mut out = String::new();
    for ch in source.chars() {
        let width = ch.width().unwrap_or(0);
        if out.width() + width > 28 {
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        APP.to_string()
    } else {
        out
    }
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{APP} : {error}");
            eprintln!("essayez « {APP} --help »");
            std::process::exit(2);
        }
    };
    if !args.once && !io::stdout().is_terminal() {
        eprintln!("{APP} : la sortie n'est pas un terminal ; utiliser --once pour une image.");
        std::process::exit(2);
    }

    let color = detect_color(args.color);
    let glyphs = Glyphs {
        unicode: !args.ascii,
    };
    let sampler = Sampler::new(Duration::from_secs_f64(args.interval), args.privacy);
    let raw_title = args.title.clone().unwrap_or_else(default_title);
    let title = display_title(&raw_title);
    install_panic_hook();

    let result = if args.once {
        run_once(sampler, &args, &glyphs, color, &title)
    } else {
        run_live(sampler, &args, &glyphs, color, &title)
    };
    if let Err(error) = result {
        eprintln!("{APP} : {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_is_bounded() {
        let long = "x".repeat(80);
        assert!(display_title(&long).width() <= 28);
    }

    #[test]
    fn display_title_strips_controls_and_falls_back() {
        assert_eq!(display_title("a\nb"), "ab");
        assert_eq!(display_title("   "), APP);
    }
}
