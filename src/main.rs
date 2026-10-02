use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use hexed::app::{run_script, App};
use hexed::doc::Document;
use hexed::term;

#[derive(Parser)]
#[command(name = "hx", version, about = "Terminal hex editor for files of any size: memory-mapped, with insert, delete, search, replace and undo")]
struct Cli {
    /// The file to edit (created on save if it does not exist)
    file: PathBuf,
    /// Open at this offset (decimal or 0x hex)
    #[arg(short = 'o', long, value_name = "OFFSET")]
    offset: Option<String>,
    /// Bytes per row (default: as many as fit, up to 16)
    #[arg(short = 'c', long, value_name = "N")]
    columns: Option<usize>,
    /// Do not allow any change
    #[arg(short = 'r', long)]
    readonly: bool,
    /// Replay comma-separated keys without a terminal and print the final screen (see the README)
    #[arg(long, value_name = "KEYS")]
    keys: Option<String>,
    /// Screen size for --keys, as WIDTHxHEIGHT
    #[arg(long, value_name = "WxH", default_value = "80x24")]
    screen: String,
    /// With --keys, print ANSI colours
    #[arg(long)]
    ansi: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hx: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let doc = Document::open(&cli.file).map_err(|e| format!("{}: {e}", cli.file.display()))?;
    let mut app = App::new(doc).readonly(cli.readonly).columns(cli.columns.unwrap_or(0));
    if let Some(o) = &cli.offset {
        let off = hexed::app::parse_goto(o, 0, app.doc.len())?;
        app.set_cursor(off);
    }
    if let Some(keys) = &cli.keys {
        let (w, h) = cli.screen.split_once('x').and_then(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?))).ok_or("--screen must look like 80x24")?;
        let screen = run_script(&mut app, keys, w, h)?;
        println!("{}", if cli.ansi { screen.ansi() } else { screen.plain() });
        return Ok(());
    }
    if !std::io::stdout().is_terminal() {
        return Err("standard output is not a terminal (use --keys to replay keys and print the screen)".to_string());
    }
    term::run(app).map_err(|e| e.to_string())
}
