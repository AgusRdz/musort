//! Review screen for what `fix` can't decide alone: tracks whose tag disagrees with their
//! folder, and folders that look like the same artist.

use std::io;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::config::Config;
use crate::exec;
use crate::resolve::{self, Alias, Decision, Merge, Mismatch};
use crate::scan::{self, Library};
use crate::text::sanitize_filename;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Review,
    Alias,
}

enum Mode {
    Browse,
    /// Typing an artist name for the selected item.
    Input(String),
    /// Waiting for a second `a` (apply) or `q` (quit with pending decisions).
    Confirm(char),
}

struct App {
    root: PathBuf,
    lib: Library,
    cfg: Config,
    mismatches: Vec<Mismatch>,
    aliases: Vec<Alias>,
    decisions: Vec<Option<Decision>>,
    merges: Vec<Option<Merge>>,
    tab: Tab,
    review_state: ListState,
    alias_state: ListState,
    mode: Mode,
    message: String,
}

impl App {
    fn load(root: &Path, message: String) -> App {
        let lib = scan::scan(root);
        let cfg = Config::load(root);
        let mismatches = resolve::mismatches(&lib);
        let aliases = resolve::aliases(&lib, &cfg);
        let first = |n: usize| ListState::default().with_selected((n > 0).then_some(0));
        App {
            root: root.to_path_buf(),
            decisions: vec![None; mismatches.len()],
            merges: vec![None; aliases.len()],
            review_state: first(mismatches.len()),
            alias_state: first(aliases.len()),
            lib,
            cfg,
            mismatches,
            aliases,
            tab: Tab::Review,
            mode: Mode::Browse,
            message,
        }
    }

    fn pending(&self) -> usize {
        self.decisions.iter().flatten().count() + self.merges.iter().flatten().count()
    }

    fn state(&mut self) -> (&mut ListState, usize) {
        match self.tab {
            Tab::Review => (&mut self.review_state, self.mismatches.len()),
            Tab::Alias => (&mut self.alias_state, self.aliases.len()),
        }
    }

    fn selected(&self) -> Option<usize> {
        match self.tab {
            Tab::Review => self.review_state.selected(),
            Tab::Alias => self.alias_state.selected(),
        }
    }

    fn move_by(&mut self, delta: isize) {
        let (state, len) = self.state();
        if len == 0 {
            return;
        }
        let i = state.selected().unwrap_or(0) as isize + delta;
        state.select(Some(i.clamp(0, len as isize - 1) as usize));
    }

    fn set_review(&mut self, d: Option<Decision>) {
        if let Some(i) = self.review_state.selected() {
            self.decisions[i] = d;
            self.move_by(1);
        }
    }

    fn set_alias(&mut self, m: Option<Merge>) {
        if let Some(i) = self.alias_state.selected() {
            self.merges[i] = m;
            self.move_by(1);
        }
    }

    fn apply(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        self.message = "Aplicando…".into();
        terminal.draw(|f| draw(f, self))?;

        let decisions: Vec<(&Mismatch, Decision)> =
            self.mismatches.iter().zip(&self.decisions).filter_map(|(m, d)| Some((m, d.clone()?))).collect();
        let merges: Vec<(&Alias, Merge)> =
            self.aliases.iter().zip(&self.merges).filter_map(|(a, m)| Some((a, m.clone()?))).collect();
        let (changes, distinct) = resolve::plan(&self.lib, &decisions, &merges);

        let mut notes = Vec::new();
        if !distinct.is_empty() {
            for (a, b) in &distinct {
                self.cfg.mark_distinct(a, b);
            }
            match self.cfg.save(&self.root) {
                Ok(()) => notes.push(format!("{} pares marcados como distintos", distinct.len())),
                Err(e) => notes.push(format!("no se pudo guardar .musort.json: {e}")),
            }
        }
        if changes.len() > 0 {
            match exec::execute(&self.root, &changes) {
                Ok(o) => {
                    notes.push(format!("{} cambios aplicados", o.applied));
                    if let Some(first) = o.failed.first() {
                        notes.push(format!("{} errores, el primero: {first}", o.failed.len()));
                    }
                    if o.run_dir.is_some() {
                        notes.push("se revierte con `musort undo`".into());
                    }
                }
                Err(e) => notes.push(e),
            }
        }
        notes.push("corre `musort fix` para limpiar títulos".into());
        *self = App::load(&self.root, notes.join(" · "));
        Ok(())
    }
}

pub fn run(root: &Path) -> io::Result<()> {
    let mut app = App::load(root, String::new());
    ratatui::run(|terminal| loop {
        terminal.draw(|f| draw(f, &mut app))?;
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match std::mem::replace(&mut app.mode, Mode::Browse) {
            Mode::Input(mut text) => match key.code {
                KeyCode::Enter if !text.trim().is_empty() => {
                    let name = text.trim().to_owned();
                    match app.tab {
                        Tab::Review => app.set_review(Some(Decision::Artist(name))),
                        Tab::Alias => app.set_alias(Some(Merge::Name(name))),
                    }
                }
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    text.pop();
                    app.mode = Mode::Input(text);
                }
                KeyCode::Char(c) => {
                    text.push(c);
                    app.mode = Mode::Input(text);
                }
                _ => app.mode = Mode::Input(text),
            },
            Mode::Confirm(c) if key.code == KeyCode::Char(c) => match c {
                'q' => return Ok(()),
                _ => app.apply(terminal)?,
            },
            Mode::Confirm(_) => app.message.clear(),
            Mode::Browse => {
                app.message.clear();
                match (app.tab, key.code) {
                    (_, KeyCode::Char('q')) | (_, KeyCode::Esc) => {
                        if app.pending() == 0 {
                            return Ok(());
                        }
                        app.message = format!("Hay {} decisiones sin aplicar. q otra vez para salir.", app.pending());
                        app.mode = Mode::Confirm('q');
                    }
                    (_, KeyCode::Tab) | (_, KeyCode::BackTab) => {
                        app.tab = if app.tab == Tab::Review { Tab::Alias } else { Tab::Review };
                    }
                    (_, KeyCode::Down) | (_, KeyCode::Char('j')) => app.move_by(1),
                    (_, KeyCode::Up) | (_, KeyCode::Char('k')) => app.move_by(-1),
                    (_, KeyCode::Char('a')) if app.pending() > 0 => {
                        app.message = format!("Aplicar {} decisiones? a otra vez para confirmar.", app.pending());
                        app.mode = Mode::Confirm('a');
                    }
                    (_, KeyCode::Char('x')) => match app.tab {
                        Tab::Review => app.set_review(None),
                        Tab::Alias => app.set_alias(None),
                    },
                    (_, KeyCode::Char('e')) if app.selected().is_some() => app.mode = Mode::Input(String::new()),
                    (Tab::Review, KeyCode::Char('c')) => app.set_review(Some(Decision::UseFolder)),
                    (Tab::Review, KeyCode::Char('t')) => app.set_review(Some(Decision::UseTag)),
                    (Tab::Alias, KeyCode::Char('1')) => app.set_alias(Some(Merge::IntoA)),
                    (Tab::Alias, KeyCode::Char('2')) => app.set_alias(Some(Merge::IntoB)),
                    (Tab::Alias, KeyCode::Char('d')) => app.set_alias(Some(Merge::Distinct)),
                    _ => {}
                }
            }
        }
    })
}

fn quoted(s: &str) -> String {
    format!("\"{s}\"")
}

fn mismatch_preview(app: &App, m: &Mismatch, d: &Decision) -> String {
    let Some(t) = app.lib.tracks.iter().find(|t| t.rel == m.rel) else { return String::new() };
    let (artist, folder) = match d {
        Decision::UseFolder => (resolve::display_name(&app.lib, &m.folder), m.folder.clone()),
        Decision::UseTag => (m.tag_artist.clone(), resolve::folder_for(&app.lib, &m.tag_artist)),
        Decision::Artist(n) => (n.clone(), resolve::folder_for(&app.lib, n)),
    };
    let artists = resolve::reassigned_artists(&app.lib, t, &artist);
    format!(
        "artistas {} · album_artist {} · carpeta {}",
        artists.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(" + "),
        quoted(&artist),
        quoted(&folder)
    )
}

fn merge_preview(app: &App, a: &Alias, m: &Merge) -> String {
    let count = |f: &str| resolve::tracks_in(&app.lib, f).len();
    match m {
        Merge::IntoA => format!("{} canciones de {} pasan a {}", count(&a.b), quoted(&a.b), quoted(&a.a)),
        Merge::IntoB => format!("{} canciones de {} pasan a {}", count(&a.a), quoted(&a.a), quoted(&a.b)),
        Merge::Name(n) => format!("las dos carpetas pasan a {} (carpeta {})", quoted(n), quoted(&sanitize_filename(n))),
        Merge::Distinct => "son artistas distintos; no se vuelve a preguntar".into(),
    }
}

fn mismatch_label(d: &Option<Decision>) -> &'static str {
    match d {
        None => "  ",
        Some(Decision::UseFolder) => "C ",
        Some(Decision::UseTag) => "T ",
        Some(Decision::Artist(_)) => "E ",
    }
}

fn merge_label(m: &Option<Merge>) -> &'static str {
    match m {
        None => "  ",
        Some(Merge::IntoA) => "1 ",
        Some(Merge::IntoB) => "2 ",
        Some(Merge::Name(_)) => "E ",
        Some(Merge::Distinct) => "D ",
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [tabs_area, body, help_area, msg_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(2), Constraint::Length(1)])
            .areas(f.area());
    let [list_area, detail_area] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body);

    let titles = vec![
        format!(" Artista equivocado ({}) ", app.mismatches.len()),
        format!(" Posibles alias ({}) ", app.aliases.len()),
    ];
    let selected_tab = if app.tab == Tab::Review { 0 } else { 1 };
    f.render_widget(
        Tabs::new(titles).select(selected_tab).highlight_style(Style::new().bold().reversed()),
        tabs_area,
    );

    let highlight = Style::new().add_modifier(Modifier::REVERSED);
    match app.tab {
        Tab::Review => {
            let items: Vec<ListItem> = app
                .mismatches
                .iter()
                .zip(&app.decisions)
                .map(|(m, d)| {
                    let name = m.rel.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    ListItem::new(Line::from(vec![Span::raw(mismatch_label(d)).green().bold(), Span::raw(name)]))
                })
                .collect();
            let list = List::new(items).block(Block::bordered().title(" Canciones ")).highlight_style(highlight);
            f.render_stateful_widget(list, list_area, &mut app.review_state);
            render_review_detail(f, app, detail_area);
        }
        Tab::Alias => {
            let items: Vec<ListItem> = app
                .aliases
                .iter()
                .zip(&app.merges)
                .map(|(a, m)| {
                    ListItem::new(Line::from(vec![
                        Span::raw(merge_label(m)).green().bold(),
                        Span::raw(format!("{} ~ {}", a.a, a.b)),
                    ]))
                })
                .collect();
            let list = List::new(items).block(Block::bordered().title(" Carpetas ")).highlight_style(highlight);
            f.render_stateful_widget(list, list_area, &mut app.alias_state);
            render_alias_detail(f, app, detail_area);
        }
    }

    let help = match (&app.mode, app.tab) {
        (Mode::Input(text), _) => format!("Nombre del artista: {text}▏   (Enter acepta, Esc cancela)"),
        (_, Tab::Review) => "c usar carpeta · t usar tag · e escribir artista · x quitar · ↑↓ mover · Tab cambiar · a aplicar · q salir".into(),
        (_, Tab::Alias) => "1 todo a la izquierda · 2 todo a la derecha · e escribir nombre · d son distintos · x quitar · Tab · a aplicar · q salir".into(),
    };
    f.render_widget(Paragraph::new(help).wrap(Wrap { trim: true }).fg(Color::DarkGray), help_area);
    let msg = if app.message.is_empty() { format!("{} decisiones pendientes", app.pending()) } else { app.message.clone() };
    f.render_widget(Paragraph::new(msg).fg(Color::Yellow), msg_area);
}

fn field<'a>(label: &'a str, value: String) -> Line<'a> {
    Line::from(vec![Span::raw(format!("{label:<14}")).fg(Color::DarkGray), Span::raw(value)])
}

fn render_review_detail(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(" Detalle ");
    let Some(i) = app.review_state.selected() else {
        f.render_widget(Paragraph::new("Nada que revisar.").block(block), area);
        return;
    };
    let m = &app.mismatches[i];
    let t = app.lib.tracks.iter().find(|t| t.rel == m.rel);
    let mut lines = vec![
        field("Archivo", m.rel.display().to_string()),
        field("Carpeta", m.folder.clone()),
        field("Artista (tag)", m.tag_artist.clone()),
    ];
    if let Some(t) = t {
        lines.push(field("Artistas", t.artist_display()));
        lines.push(field("Título", t.title.clone()));
        lines.push(field("Álbum", t.album.clone()));
    }
    lines.push(Line::raw(""));
    let options = [("c", Decision::UseFolder), ("t", Decision::UseTag)];
    for (key, d) in options {
        lines.push(Line::from(vec![Span::raw(format!("[{key}] ")).bold(), Span::raw(mismatch_preview(app, m, &d))]));
    }
    if let Some(d) = &app.decisions[i] {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![Span::raw("Elegido: ").green().bold(), Span::raw(mismatch_preview(app, m, d))]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), area);
}

fn render_alias_detail(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(" Detalle ");
    let Some(i) = app.alias_state.selected() else {
        f.render_widget(Paragraph::new("No hay alias por revisar.").block(block), area);
        return;
    };
    let a = &app.aliases[i];
    let mut lines = Vec::new();
    for (key, folder) in [("1", &a.a), ("2", &a.b)] {
        let tracks = resolve::tracks_in(&app.lib, folder);
        lines.push(Line::from(vec![
            Span::raw(format!("[{key}] ")).bold(),
            Span::raw(format!("{folder} ({} canciones)", tracks.len())).bold(),
        ]));
        for t in tracks.iter().take(6) {
            lines.push(Line::raw(format!("     {}", t.title)));
        }
        if tracks.len() > 6 {
            lines.push(Line::raw(format!("     … y {} más", tracks.len() - 6)).fg(Color::DarkGray));
        }
        lines.push(Line::raw(""));
    }
    if let Some(m) = &app.merges[i] {
        lines.push(Line::from(vec![Span::raw("Elegido: ").green().bold(), Span::raw(merge_preview(app, a, m))]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), area);
}
