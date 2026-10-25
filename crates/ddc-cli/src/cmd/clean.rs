//! `ddc clean`: fresh scan → plan → confirmation → execution.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::class::{Category, RiskClass};
use ddc_core::env::Env;
use ddc_core::exec::{
    self, Consent, ExecOptions, ExecProgress, FileJournal, Ports, SystemCommands, SystemProcesses, SystemTrash,
};
use ddc_core::plan::{PlanOptions, build_plan};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::settings::ScanFlags;
use ddc_core::store::{self, LockMode};

use crate::cli::{CategoryArg, ClassArg, CleanArgs};
use crate::exit;
use crate::progress;
use crate::prompt::{self, Decision};
use crate::render::{self, Style};

pub fn run(args: CleanArgs) -> anyhow::Result<ExitCode> {
    if ddc_core::sys::procs::is_root() {
        eprintln!("ddc clean: refusing to run as root; run it as your user");
        return Ok(ExitCode::from(exit::REFUSED));
    }
    if let Some(items) = &args.items {
        return super::clean_items::run(&args, items);
    }
    if args.yes && args.classes.contains(&ClassArg::Review) {
        eprintln!("ddc clean: --yes cannot include --class review: review items need an interactive confirmation");
        return Ok(ExitCode::from(exit::REFUSED));
    }
    let env = Env::from_process(args.data_dir.clone())?;
    let resolved = super::resolve(
        &env,
        ScanFlags { paths: args.paths.clone(), excludes: args.exclude.clone(), min_age_days: args.min_age },
    )?;
    super::note_missing(&resolved.missing);
    let dropped = &resolved.dropped_excludes;
    // Nobody to ask: a refusal names the folders the --exclude would drop, too.
    let unprotected =
        if dropped.is_empty() { String::new() } else { format!("; {}", prompt::dropped_excludes(dropped, env.home())) };
    let interactive = !args.yes && !args.dry_run;
    if interactive && !std::io::stdin().is_terminal() {
        eprintln!(
            "ddc clean: confirmation needs a terminal; use --dry-run to preview or --yes for safe items{unprotected}"
        );
        return Ok(ExitCode::from(exit::REFUSED));
    }
    // Asked before the lock and the scan: a declined answer leaves no trace.
    if !dropped.is_empty() {
        if args.dry_run {
            eprintln!("ddc clean: {}", prompt::dropped_excludes(dropped, env.home()));
        } else if !std::io::stdin().is_terminal() {
            eprintln!("ddc clean: name them in --exclude too, or confirm in a terminal{unprotected}");
            return Ok(ExitCode::from(exit::REFUSED));
        } else if !prompt::confirm_dropped_excludes(
            dropped,
            env.home(),
            std::io::stdin().lock(),
            &mut std::io::stdout(),
        )? {
            println!("Nothing was changed.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    let _lock = store::try_lock(&env, LockMode::Exclusive)?;

    let opts = ScanOptions { walk: WalkOptions::default(), ..resolved.options };
    let scan_progress = Progress::default();
    let scanned = progress::with_spinner(&scan_progress, "scanning", || report::scan(&env, &opts, &scan_progress))?;
    store::write_last_scan(&env, &scanned).context("cannot save the scan report")?;

    let mut classes: Vec<RiskClass> = args.classes.iter().map(|c| class(*c)).collect();
    if args.yes && classes.is_empty() {
        classes.push(RiskClass::Safe);
    }
    let plan_opts = PlanOptions {
        classes: classes.clone(),
        categories: args.categories.iter().map(|c| category(*c)).collect(),
        min_size: args.min_size.unwrap_or(0),
        permanent: args.permanent,
    };
    let catalog = Catalog::builtin(env.home());
    let mut plan = build_plan(&scanned, &catalog, &plan_opts);

    let style = Style { color: render::use_color(std::io::stdout().is_terminal(), Env::no_color()), all: false };
    let mut out = std::io::stdout().lock();
    prompt::print_plan(&mut out, &plan, &style)?;
    if args.dry_run {
        writeln!(out, "\nDry run: nothing was changed.")?;
        return Ok(ExitCode::SUCCESS);
    }
    let commands = SystemCommands::new(&env);
    let snapshots = exec::local_snapshots(&commands);
    if snapshots > 0 {
        writeln!(
            out,
            "{} {snapshots} local Time Machine snapshots keep deleted data until macOS thins them; \
             free space may not grow right away.",
            style.dim("note:")
        )?;
    }
    let consent = if args.yes {
        Consent::AssumeYes { classes }
    } else {
        match prompt::interactive(&mut plan, &scanned.home, std::io::stdin().lock(), &mut out, &style)? {
            Decision::Proceed => Consent::Interactive,
            Decision::Abort => {
                writeln!(out, "Nothing was changed.")?;
                return Ok(ExitCode::SUCCESS);
            }
        }
    };
    if !plan.items.iter().any(|i| i.selected) {
        writeln!(out, "Nothing selected; nothing was changed.")?;
        return Ok(ExitCode::SUCCESS);
    }

    let mut journal = FileJournal::open(&env).context("cannot open the action journal")?;
    let journal_path = journal.path.clone();
    // Ctrl-C now stops the clean after the current item and still reaches a
    // tool running in its own process group.
    let cancel = ddc_core::sys::signals::install().context("cannot install the signal handler")?;
    let exec_progress = ExecProgress::default();
    let result = progress::with_clean_line(&exec_progress, &scanned.home, || {
        exec::execute(
            &env,
            &catalog,
            &plan,
            &consent,
            Ports { commands: &commands, trash: &SystemTrash, procs: &SystemProcesses, journal: &mut journal },
            // The snapshots were counted for the note above: `tmutil` runs once.
            &ExecOptions { cancel, progress: Some(&exec_progress), local_snapshots: Some(snapshots) },
        )
    })?;
    prompt::print_result(&mut out, &result, &scanned.home, &journal_path, &style)?;
    out.flush()?;
    if let Some(sig) = ddc_core::sys::signals::received() {
        ddc_core::sys::signals::reraise(sig);
    }
    let failed = result.items.iter().any(|i| matches!(i.outcome, exec::Outcome::Failed { .. }));
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn class(c: ClassArg) -> RiskClass {
    match c {
        ClassArg::Safe => RiskClass::Safe,
        ClassArg::Rebuildable => RiskClass::Rebuildable,
        ClassArg::Review => RiskClass::Review,
    }
}

fn category(c: CategoryArg) -> Category {
    match c {
        CategoryArg::ProjectArtifact => Category::ProjectArtifact,
        CategoryArg::PackageCache => Category::PackageCache,
        CategoryArg::Xcode => Category::Xcode,
        CategoryArg::VmContainer => Category::VmContainer,
        CategoryArg::Toolchain => Category::Toolchain,
        CategoryArg::MlModel => Category::MlModel,
        CategoryArg::Ide => Category::Ide,
        CategoryArg::AppCache => Category::AppCache,
        CategoryArg::GenericCache => Category::GenericCache,
    }
}
