//! `ddc du`: allocated size per root, comparable with `du -skx`.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use ddc_core::env::Env;
use ddc_core::rules::{Catalog, RuleVisitor, classify};
use ddc_core::scan::{self, NullVisitor, Progress, ReaderKind, WalkOptions};

use crate::cli::{DuArgs, ReaderArg};

pub fn run(args: DuArgs) -> anyhow::Result<ExitCode> {
    let roots = if args.paths.is_empty() { vec![PathBuf::from(".")] } else { args.paths };
    let opts = WalkOptions {
        threads: args.threads,
        reader: match args.reader {
            ReaderArg::Bulk => ReaderKind::Bulk,
            ReaderArg::Std => ReaderKind::Std,
        },
        private_everywhere: args.private,
        ..WalkOptions::default()
    };
    let progress = Progress::default();
    let mut stdout = std::io::stdout().lock();
    let out = if args.rules {
        let env = Env::from_process(None)?;
        let catalog = Catalog::builtin(env.home());
        let visitor = RuleVisitor::new(&catalog, Vec::new());
        let out = scan::walk(&roots, &visitor, &opts, &progress)?;
        for f in classify(&out, &visitor) {
            let notes: Vec<String> = f.notes.iter().map(|n| format!("{n:?}")).collect();
            writeln!(
                stdout,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                f.owner.path.display(),
                f.owner.bytes,
                f.rule.id,
                f.class.as_str(),
                f.owner.private_bytes.map_or_else(|| "-".to_owned(), |b| b.to_string()),
                f.owner.shared_bytes,
                notes.join(","),
            )?;
        }
        out
    } else {
        scan::walk(&roots, &NullVisitor, &opts, &progress)?
    };

    for root in out.roots.iter().filter(|_| !args.rules) {
        // `du -k` rounds 512-byte blocks up to whole KiB.
        writeln!(stdout, "{}\t{}", root.bytes.div_ceil(1024), root.path.display())?;
    }
    let files: u64 = out.roots.iter().map(|r| r.files).sum();
    let dirs: u64 = out.roots.iter().map(|r| r.dirs).sum();
    eprintln!(
        "ddc du: {files} files, {dirs} dirs, {:.2} GiB allocated, {} errors, {} skipped, {:.2}s",
        out.total_bytes as f64 / f64::from(1u32 << 30),
        out.errors_total,
        out.skipped_total,
        out.elapsed.as_secs_f64(),
    );
    for err in out.errors.iter().take(5) {
        eprintln!("ddc du: {}: {}", err.path.display(), err.error);
    }
    // Like du: non-zero exit when something could not be read.
    Ok(if out.errors_total > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}
