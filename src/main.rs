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
#[command(name = "musort", version, about = "Organiza una biblioteca de música para Jellyfin y reproductores portátiles")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Revisa la biblioteca y reporta problemas (solo lectura)
    Doctor {
        /// Carpeta raíz de la biblioteca
        library: PathBuf,
        /// Muestra todos los elementos de cada problema, no solo los primeros
        #[arg(short, long)]
        all: bool,
    },
    /// Quita carpetas vacías, basura del sistema y letras sin canción (a la papelera de musort)
    Clean {
        /// Carpeta raíz de la biblioteca
        library: PathBuf,
        /// Aplica los cambios; sin esto solo muestra lo que haría
        #[arg(long)]
        apply: bool,
    },
    /// Corrige tags (artistas, album_artist, basura) y renombra archivos y carpetas según el tag
    Fix {
        /// Carpeta raíz de la biblioteca
        library: PathBuf,
        /// Aplica los cambios; sin esto solo muestra lo que haría
        #[arg(long)]
        apply: bool,
        /// También convierte a NFC los nombres guardados en NFD (Jellyfin los verá como canciones nuevas)
        #[arg(long)]
        unicode: bool,
        /// Muestra todos los cambios, no solo los primeros
        #[arg(short, long)]
        all: bool,
    },
    /// Copia la música a la tarjeta del reproductor: solo lo nuevo o cambiado, y borra lo que ya no está
    Sync {
        /// Carpeta raíz de la biblioteca
        library: PathBuf,
        /// Raíz de la tarjeta del reproductor (p. ej. /Volumes/JM21)
        dest: PathBuf,
        /// Aplica los cambios; sin esto solo muestra lo que haría
        #[arg(long)]
        apply: bool,
        /// Muestra todos los cambios, no solo los primeros
        #[arg(short, long)]
        all: bool,
    },
    /// Pantalla para decidir lo que `fix` no puede: artistas equivocados y posibles alias
    Review {
        /// Carpeta raíz de la biblioteca
        library: PathBuf,
    },
    /// Revierte la última operación (clean, fix o review)
    Undo {
        /// Carpeta raíz de la biblioteca
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
        eprintln!("No existe la carpeta: {}", library.display());
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
        println!("    … y {} más (usa --all)", items.len() - shown);
    }
    println!();
}

fn doctor_cmd(library: &Path, all: bool) -> ExitCode {
    let lib = scan::scan(library);
    println!("{} canciones en {}\n", lib.tracks.len(), library.display());
    let checks = doctor::run(&lib, &config::Config::load(library));
    if checks.is_empty() {
        println!("Sin problemas.");
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
        eprintln!("Hay {} archivos que no se pudieron leer; corre `doctor` y revisa antes.", lib.errors.len());
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
            println!("Registro en {}", dir.display());
            println!("Para revertir: musort undo {}", library.display());
        }
        Ok(None) => {}
        Err(e) => eprintln!("No se pudo cerrar el registro: {e}"),
    }
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn clean_cmd(library: &Path, apply: bool) -> ExitCode {
    let Some(lib) = scan_for_writing(library) else { return ExitCode::FAILURE };
    let targets = clean::plan(&lib);
    if targets.is_empty() {
        println!("Nada que limpiar.");
        return ExitCode::SUCCESS;
    }
    for t in &targets {
        let slash = if library.join(&t.rel).is_dir() { "/" } else { "" };
        println!("  {}{slash}  ({})", t.rel.display(), t.reason);
    }
    if !apply {
        println!("\n{} elementos. Nada se movió; usa --apply para aplicar.", targets.len());
        return ExitCode::SUCCESS;
    }

    let mut run = match Run::start(library) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("No se pudo crear la papelera: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = Vec::new();
    for t in &targets {
        if let Err(e) = run.trash(&t.rel) {
            failed.push(format!("{}: {e}", t.rel.display()));
        }
    }
    println!("\n{} elementos movidos a la papelera.", targets.len() - failed.len());
    finish(run, &failed, library)
}

fn describe(e: &Edit) -> Vec<String> {
    let mut out = Vec::new();
    let q = |s: &str| format!("\"{s}\"");
    if let Some(v) = &e.artists {
        out.push(format!("artistas {}", v.iter().map(|a| q(a)).collect::<Vec<_>>().join(" + ")));
    }
    if let Some(v) = &e.album_artist {
        out.push(format!("album_artist {}", q(v)));
    }
    if let Some(v) = &e.title {
        out.push(format!("título {}", q(v)));
    }
    if let Some(v) = &e.album {
        out.push(format!("álbum {}", q(v)));
    }
    if let Some(v) = &e.comment {
        out.push(if v.is_empty() { "quita comentario".into() } else { format!("comentario {}", q(v)) });
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
    section("Solo album_artist", "Se llena con el artista principal.", &only(&album_artist_only), all);
    section("Otros cambios de tags", "", &only(&|e| !album_artist_only(e)), all);
    section("Archivos renombrados", "Para que coincidan con el título.", &renames(&plan.files), all);
    section("Carpetas renombradas", "Para que coincidan con el artista.", &renames(&plan.folders), all);
    section("Nombres a NFC", "", &renames(&plan.unicode), all);
    section("Revisar a mano (no se toca)", "", &plan.review, all);

    let total = plan.tags.len() + plan.files.len() + plan.folders.len() + plan.unicode.len();
    if total == 0 {
        println!("Nada que corregir.");
        return ExitCode::SUCCESS;
    }
    if !apply {
        println!(
            "{} tags, {} renombres. Nada se cambió; usa --apply para aplicar.",
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
            println!("{} cambios aplicados.", o.applied);
            for f in &o.failed {
                eprintln!("  error: {f}");
            }
            if let Some(dir) = o.run_dir {
                println!("Registro en {}", dir.display());
                println!("Para revertir: musort undo {}", library.display());
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
        eprintln!("No existe el destino: {}", dest.display());
        return ExitCode::FAILURE;
    };
    if card.starts_with(&lib) || lib.starts_with(&card) {
        eprintln!("La biblioteca y el destino no pueden estar uno dentro del otro.");
        return ExitCode::FAILURE;
    }
    let plan = match sync::plan(library, dest) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("No se pudo leer: {e}");
            return ExitCode::FAILURE;
        }
    };
    let show = |ps: &[PathBuf]| ps.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
    let imports: Vec<String> = plan.imports.iter().map(|(c, _)| c.display().to_string()).collect();
    section("Letras que se traen al NAS", "Solo están en la tarjeta y la canción no tiene letra en la biblioteca.", &imports, all);
    section("A borrar en la tarjeta", "Ya no están en la biblioteca.", &show(&plan.deletes), all);
    section("Se conservan", "No están en la biblioteca, pero no son música.", &show(&plan.kept), all);
    let copies: Vec<String> = plan.copies.iter().map(|c| c.dest.clone()).collect();
    section("A copiar", "Nuevos o cambiados.", &copies, all);
    let gb = plan.bytes as f64 / 1e9;
    println!(
        "{} sin cambios, {} a copiar ({gb:.1} GB), {} a borrar, {} letras al NAS.",
        plan.unchanged,
        plan.copies.len(),
        plan.deletes.len(),
        plan.imports.len()
    );
    if !apply {
        println!("Nada se cambió; usa --apply para aplicar.");
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
        println!("Tarjeta sincronizada.");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn undo_cmd(library: &Path) -> ExitCode {
    match trash::undo(library) {
        Ok(None) => {
            println!("No hay nada que revertir.");
            ExitCode::SUCCESS
        }
        Ok(Some(r)) => {
            println!("{} operaciones revertidas.", r.restored);
            if r.failed.is_empty() {
                return ExitCode::SUCCESS;
            }
            eprintln!("No se pudieron revertir (siguen en {}):", r.run_dir.display());
            for f in &r.failed {
                eprintln!("  {f}");
            }
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("Error al revertir: {e}");
            ExitCode::FAILURE
        }
    }
}
