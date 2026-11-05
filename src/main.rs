mod clean;
mod config;
mod doctor;
mod exec;
mod fix;
mod resolve;
mod scan;
mod sync;
mod tags;
mod text;
mod trash;
mod tui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::tags::Edit;
use crate::trash::Run;

#[derive(Parser)]
#[command(name = "musort", version, about = "Organizes a music library for Jellyfin and portable players")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Checks the library and reports problems (read-only)
    Doctor {
        /// Library root folder
        library: PathBuf,
        /// Show every item of each problem, not just the first few
        #[arg(short, long)]
        all: bool,
    },
    /// Removes empty folders, system junk and orphan lyrics (into musort's trash)
    Clean {
        /// Library root folder
        library: PathBuf,
        /// Apply the changes; without it, only show what would happen
        #[arg(long)]
        apply: bool,
    },
    /// Fixes tags (artists, album_artist, junk) and renames files and folders after the tags
    Fix {
        /// Library root folder
        library: PathBuf,
        /// Apply the changes; without it, only show what would happen
        #[arg(long)]
        apply: bool,
        /// Also convert names stored as NFD to NFC (Jellyfin will see them as new songs)
        #[arg(long)]
        unicode: bool,
        /// Show every change, not just the first few
        #[arg(short, long)]
        all: bool,
    },
    /// Copies music to the player's card: only what's new or changed, and deletes what's gone
    Sync {
        /// Library root folder
        library: PathBuf,
        /// Root of the player's card (e.g. /Volumes/JM21)
        dest: PathBuf,
        /// Apply the changes; without it, only show what would happen
        #[arg(long)]
        apply: bool,
        /// Show every change, not just the first few
        #[arg(short, long)]
        all: bool,
    },
    /// Screen to decide what `fix` can't: wrong artists and possible aliases
    Review {
        /// Library root folder
        library: PathBuf,
    },
    /// Reverts the last operation (clean, fix or review)
    Undo {
        /// Library root folder
        library: PathBuf,
    },
}

const PREVIEW: usize = 8;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let library = match &cli.command {
        Command::Doctor { library, .. }
        | Command::Clean { library, .. }
        | Command::Fix { library, .. }
        | Command::Sync { library, .. }
        | Command::Review { library }
        | Command::Undo { library } => library.clone(),
    };
    if !library.is_dir() {
        eprintln!("Folder not found: {}", library.display());
        return ExitCode::FAILURE;
    }
    match cli.command {
        Command::Doctor { all, .. } => doctor_cmd(&library, all),
        Command::Clean { apply, .. } => clean_cmd(&library, apply),
        Command::Fix { apply, unicode, all, .. } => fix_cmd(&library, apply, unicode, all),
        Command::Sync { dest, apply, all, .. } => sync_cmd(&library, &dest, apply, all),
        Command::Review { .. } => match tui::run(&library) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error: {e}");
                ExitCode::FAILURE
            }
        },
        Command::Undo { .. } => undo_cmd(&library),
    }
}

fn section(title: &str, why: &str, items: &[String], all: bool) {
    if items.is_empty() {
        return;
    }
    println!("■ {title} ({})", items.len());
    if !why.is_empty() {
        println!("  {why}");
    }
    let shown = if all { items.len() } else { PREVIEW };
    for item in items.iter().take(shown) {
        println!("    {item}");
    }
    if items.len() > shown {
        println!("    … and {} more (use --all)", items.len() - shown);
    }
    println!();
}

fn doctor_cmd(library: &Path, all: bool) -> ExitCode {
    let lib = scan::scan(library);
    println!("{} songs in {}\n", lib.tracks.len(), library.display());
    let checks = doctor::run(&lib, &config::Config::load(library));
    if checks.is_empty() {
        println!("No problems found.");
    }
    for c in &checks {
        section(c.title, c.why, &c.items, all);
    }
    ExitCode::SUCCESS
}

/// Refuses to change anything while some files can't be read: their folders might look empty.
fn scan_for_writing(library: &Path) -> Option<scan::Library> {
    let lib = scan::scan(library);
    if !lib.errors.is_empty() {
        eprintln!("{} files couldn't be read; run `doctor` and check them first.", lib.errors.len());
        return None;
    }
    Some(lib)
}

fn finish(run: Run, failed: &[String], library: &Path) -> ExitCode {
    for f in failed {
        eprintln!("  error: {f}");
    }
    match run.finish() {
        Ok(Some(dir)) => {
            println!("Log in {}", dir.display());
            println!("To revert: musort undo {}", library.display());
        }
        Ok(None) => {}
        Err(e) => eprintln!("Couldn't close the log: {e}"),
    }
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn clean_cmd(library: &Path, apply: bool) -> ExitCode {
    let Some(lib) = scan_for_writing(library) else { return ExitCode::FAILURE };
    let targets = clean::plan(&lib);
    if targets.is_empty() {
        println!("Nothing to clean.");
        return ExitCode::SUCCESS;
    }
    for t in &targets {
        let slash = if library.join(&t.rel).is_dir() { "/" } else { "" };
        println!("  {}{slash}  ({})", t.rel.display(), t.reason);
    }
    if !apply {
        println!("\n{} items. Nothing was moved; use --apply to apply.", targets.len());
        return ExitCode::SUCCESS;
    }

    let mut run = match Run::start(library) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Couldn't create the trash: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = Vec::new();
    for t in &targets {
        if let Err(e) = run.trash(&t.rel) {
            failed.push(format!("{}: {e}", t.rel.display()));
        }
    }
    println!("\n{} items moved to the trash.", targets.len() - failed.len());
    finish(run, &failed, library)
}

fn describe(e: &Edit) -> Vec<String> {
    let mut out = Vec::new();
    let q = |s: &str| format!("\"{s}\"");
    if let Some(v) = &e.artists {
        out.push(format!("artists {}", v.iter().map(|a| q(a)).collect::<Vec<_>>().join(" + ")));
    }
    if let Some(v) = &e.album_artist {
        out.push(format!("album_artist {}", q(v)));
    }
    if let Some(v) = &e.title {
        out.push(format!("title {}", q(v)));
    }
    if let Some(v) = &e.album {
        out.push(format!("album {}", q(v)));
    }
    if let Some(v) = &e.comment {
        out.push(if v.is_empty() { "remove comment".into() } else { format!("comment {}", q(v)) });
    }
    out
}

fn fix_cmd(library: &Path, apply: bool, unicode: bool, all: bool) -> ExitCode {
    let Some(lib) = scan_for_writing(library) else { return ExitCode::FAILURE };
    let plan = fix::plan(&lib, unicode);

    let renames = |rs: &[fix::Rename]| -> Vec<String> {
        rs.iter().map(|r| format!("{} → {}", r.from.display(), r.to.file_name().unwrap_or_default().to_string_lossy())).collect()
    };
    let only = |pred: &dyn Fn(&Edit) -> bool| -> Vec<String> {
        plan.tags
            .iter()
            .filter(|f| pred(&f.after))
            .map(|f| format!("{} — {}", f.rel.display(), describe(&f.after).join(", ")))
            .collect()
    };
    let album_artist_only =
        |e: &Edit| e.album_artist.is_some() && e.artists.is_none() && e.title.is_none() && e.album.is_none() && e.comment.is_none();
    section("album_artist only", "Filled with the main artist.", &only(&album_artist_only), all);
    section("Other tag changes", "", &only(&|e| !album_artist_only(e)), all);
    section("Renamed files", "To match the title.", &renames(&plan.files), all);
    section("Renamed folders", "To match the artist.", &renames(&plan.folders), all);
    section("Names to NFC", "", &renames(&plan.unicode), all);
    section("Review by hand (left untouched)", "", &plan.review, all);

    let total = plan.tags.len() + plan.files.len() + plan.folders.len() + plan.unicode.len();
    if total == 0 {
        println!("Nothing to fix.");
        return ExitCode::SUCCESS;
    }
    if !apply {
        println!(
            "{} tags, {} renames. Nothing was changed; use --apply to apply.",
            plan.tags.len(),
            plan.files.len() + plan.folders.len() + plan.unicode.len()
        );
        return ExitCode::SUCCESS;
    }

    let changes = exec::Changes {
        tags: plan.tags,
        renames: plan.files.into_iter().chain(plan.unicode).chain(plan.folders).collect(),
        remove_dirs: Vec::new(),
    };
    report(library, exec::execute(library, &changes))
}

fn report(library: &Path, outcome: Result<exec::Outcome, String>) -> ExitCode {
    match outcome {
        Ok(o) => {
            println!("{} changes applied.", o.applied);
            for f in &o.failed {
                eprintln!("  error: {f}");
            }
            if let Some(dir) = o.run_dir {
                println!("Log in {}", dir.display());
                println!("To revert: musort undo {}", library.display());
            }
            if o.failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn sync_cmd(library: &Path, dest: &Path, apply: bool, all: bool) -> ExitCode {
    let (Ok(lib), Ok(card)) = (library.canonicalize(), dest.canonicalize()) else {
        eprintln!("Destination not found: {}", dest.display());
        return ExitCode::FAILURE;
    };
    if card.starts_with(&lib) || lib.starts_with(&card) {
        eprintln!("The library and the destination can't be inside one another.");
        return ExitCode::FAILURE;
    }
    let plan = match sync::plan(library, dest) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Couldn't read: {e}");
            return ExitCode::FAILURE;
        }
    };
    let show = |ps: &[PathBuf]| ps.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
    let imports: Vec<String> = plan.imports.iter().map(|(c, _)| c.display().to_string()).collect();
    section("Lyrics brought into the library", "Only on the card, and the song has no lyrics in the library.", &imports, all);
    section("To delete from the card", "No longer in the library.", &show(&plan.deletes), all);
    section("Kept", "Not in the library, but not music either.", &show(&plan.kept), all);
    let copies: Vec<String> = plan.copies.iter().map(|c| c.dest.clone()).collect();
    section("To copy", "New or changed.", &copies, all);
    let gb = plan.bytes as f64 / 1e9;
    println!(
        "{} unchanged, {} to copy ({gb:.1} GB), {} to delete, {} lyrics into the library.",
        plan.unchanged,
        plan.copies.len(),
        plan.deletes.len(),
        plan.imports.len()
    );
    if !apply {
        println!("Nothing was changed; use --apply to apply.");
        return ExitCode::SUCCESS;
    }
    if plan.copies.is_empty() && plan.deletes.is_empty() && plan.imports.is_empty() {
        return ExitCode::SUCCESS;
    }

    let failed = sync::apply(library, dest, plan, |i, n, name| {
        eprint!("\r\x1b[K[{i}/{n}] {name}");
    });
    eprintln!();
    for f in &failed {
        eprintln!("  error: {f}");
    }
    if failed.is_empty() {
        println!("Card synced.");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn undo_cmd(library: &Path) -> ExitCode {
    match trash::undo(library) {
        Ok(None) => {
            println!("Nothing to revert.");
            ExitCode::SUCCESS
        }
        Ok(Some(r)) => {
            println!("{} operations reverted.", r.restored);
            if r.failed.is_empty() {
                return ExitCode::SUCCESS;
            }
            eprintln!("Couldn't revert (still in {}):", r.run_dir.display());
            for f in &r.failed {
                eprintln!("  {f}");
            }
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("Error while reverting: {e}");
            ExitCode::FAILURE
        }
    }
}
