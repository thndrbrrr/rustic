use std::fs::File;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use merge::Merge;
use serde::Deserialize;
use serde_with::{serde_as, DisplayFromStr};
use simplelog::*;

use crate::backend::{FileType, ReadBackend};
use crate::repository::{Repository, RepositoryOptions};

use helpers::*;

mod backup;
mod cat;
mod check;
mod completions;
mod config;
mod copy;
mod diff;
mod dump;
mod forget;
mod helpers;
mod init;
mod key;
mod list;
mod ls;
mod merge_cmd;
mod prune;
mod repair;
mod repoinfo;
mod restore;
mod rustic_config;
mod self_update;
mod snapshots;
mod tag;

use rustic_config::RusticConfig;

#[derive(Parser)]
#[clap(about, version, name="rustic", version = option_env!("PROJECT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")))]
struct Opts {
    /// Config profile to use. This parses the file `<PROFILE>.toml` in the config directory.
    #[clap(
        short = 'P',
        long,
        value_name = "PROFILE",
        global = true,
        default_value = "rustic",
        help_heading = "Global options"
    )]
    config_profile: String,

    #[clap(flatten, next_help_heading = "Global options")]
    global: GlobalOpts,

    #[clap(flatten, next_help_heading = "Repository options")]
    repository: RepositoryOptions,

    #[clap(subcommand)]
    command: Command,
}

#[serde_as]
#[derive(Default, Parser, Deserialize, Merge)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
struct GlobalOpts {
    /// Only show what would be done without modifying anything. Does not affect read-only commands
    #[clap(long, short = 'n', global = true, env = "RUSTIC_DRY_RUN")]
    #[merge(strategy = merge::bool::overwrite_false)]
    dry_run: bool,

    /// Use this log level [default: info]
    #[clap(long, global = true, env = "RUSTIC_LOG_LEVEL")]
    #[serde_as(as = "Option<DisplayFromStr>")]
    log_level: Option<LevelFilter>,

    /// Write log messages to the given file instead of printing them.
    /// Note: warnings and errors are still additionally printed unless they are ignored by --log-level
    #[clap(long, global = true, env = "RUSTIC_LOG_FILE", value_name = "LOGFILE")]
    log_file: Option<PathBuf>,

    /// Don't show any progress bar
    #[clap(long, global = true, env = "RUSTIC_NO_PROGRESS")]
    #[merge(strategy=merge::bool::overwrite_false)]
    no_progress: bool,

    /// Interval to update progress bars
    #[clap(
        long,
        global = true,
        env = "RUSTIC_PROGRESS_INTERVAL",
        value_name = "DURATION",
        conflicts_with = "no_progress"
    )]
    #[serde_as(as = "Option<DisplayFromStr>")]
    progress_interval: Option<humantime::Duration>,
}

#[derive(Subcommand)]
enum Command {
    /// Backup to the repository
    Backup(backup::Opts),

    /// Show raw data of repository files and blobs
    Cat(cat::Opts),

    /// Change the repository configuration
    Config(config::Opts),

    /// Generate shell completions
    Completions(completions::Opts),

    /// Check the repository
    Check(check::Opts),

    /// Copy snapshots to other repositories. Note: The target repositories must be given in the config file!
    Copy(copy::Opts),

    /// Compare two snapshots/paths
    /// Note that the exclude options only apply for comparison with a local path
    Diff(diff::Opts),

    /// dump the contents of a file in a snapshot to stdout
    Dump(dump::Opts),

    /// Remove snapshots from the repository
    Forget(forget::Opts),

    /// Initialize a new repository
    Init(init::Opts),

    /// Manage keys
    Key(key::Opts),

    /// List repository files
    List(list::Opts),

    /// List file contents of a snapshot
    Ls(ls::Opts),

    /// Merge snapshots
    Merge(merge_cmd::Opts),

    /// Show a detailed overview of the snapshots within the repository
    Snapshots(snapshots::Opts),

    /// Update to the latest rustic release
    SelfUpdate(self_update::Opts),

    /// Remove unused data or repack repository pack files
    Prune(prune::Opts),

    /// Restore a snapshot/path
    Restore(restore::Opts),

    /// Restore a snapshot/path
    Repair(repair::Opts),

    /// Show general information about the repository
    Repoinfo(repoinfo::Opts),

    /// Change tags of snapshots
    Tag(tag::Opts),
}

pub fn execute() -> Result<()> {
    let command: Vec<_> = std::env::args_os().collect();
    let args = Opts::parse_from(&command);

    // get global options from command line / env and config file
    let config_file = RusticConfig::new(&args.config_profile)?;
    let mut gopts = args.global;
    config_file.merge_into("global", &mut gopts)?;

    // start logger
    let level_filter = gopts.log_level.unwrap_or(LevelFilter::Info);
    match &gopts.log_file {
        None => TermLogger::init(
            level_filter,
            ConfigBuilder::new()
                .set_time_level(LevelFilter::Off)
                .build(),
            TerminalMode::Stderr,
            ColorChoice::Auto,
        )?,

        Some(file) => CombinedLogger::init(vec![
            TermLogger::new(
                level_filter.max(LevelFilter::Warn),
                ConfigBuilder::new()
                    .set_time_level(LevelFilter::Off)
                    .build(),
                TerminalMode::Stderr,
                ColorChoice::Auto,
            ),
            WriteLogger::new(
                level_filter,
                Config::default(),
                File::options().create(true).append(true).open(file)?,
            ),
        ])?,
    }

    if gopts.no_progress {
        let mut no_progress = NO_PROGRESS.lock().unwrap();
        *no_progress = true;
    }

    if let Some(duration) = gopts.progress_interval {
        let mut interval = PROGRESS_INTERVAL.lock().unwrap();
        *interval = *duration;
    }

    if let Command::SelfUpdate(opts) = args.command {
        self_update::execute(opts)?;
        return Ok(());
    }

    if let Command::Completions(opts) = args.command {
        completions::execute(opts);
        return Ok(());
    }

    let command: String = command
        .into_iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");

    let mut repo_opts = args.repository;
    config_file.merge_into("repository", &mut repo_opts)?;
    let repo = Repository::new(repo_opts)?;

    if let Command::Init(opts) = args.command {
        let config_ids = repo.be.list(FileType::Config)?;
        return init::execute(&repo.be, &repo.be_hot, opts, repo.password()?, config_ids);
    }

    let repo = repo.open()?;

    #[allow(clippy::match_same_arms)]
    match args.command {
        Command::Backup(opts) => backup::execute(repo, gopts, opts, config_file, command)?,
        Command::Config(opts) => config::execute(repo, opts)?,
        Command::Cat(opts) => cat::execute(repo, opts, config_file)?,
        Command::Check(opts) => check::execute(repo, opts)?,
        Command::Completions(_) => {} // already handled above
        Command::Copy(opts) => copy::execute(repo, gopts, opts, config_file)?,
        Command::Diff(opts) => diff::execute(repo, opts, config_file)?,
        Command::Dump(opts) => dump::execute(repo, opts, config_file)?,
        Command::Forget(opts) => forget::execute(repo, gopts, opts, config_file)?,
        Command::Init(_) => {} // already handled above
        Command::Key(opts) => key::execute(repo, opts)?,
        Command::List(opts) => list::execute(repo, opts)?,
        Command::Ls(opts) => ls::execute(repo, opts, config_file)?,
        Command::Merge(opts) => merge_cmd::execute(repo, opts, config_file, command)?,
        Command::SelfUpdate(_) => {} // already handled above
        Command::Snapshots(opts) => snapshots::execute(repo, opts, config_file)?,
        Command::Prune(opts) => prune::execute(repo, gopts, opts, vec![])?,
        Command::Restore(opts) => restore::execute(repo, gopts, opts, config_file)?,
        Command::Repair(opts) => repair::execute(repo, gopts, opts, config_file)?,
        Command::Repoinfo(opts) => repoinfo::execute(repo, opts)?,
        Command::Tag(opts) => tag::execute(repo, gopts, opts, config_file)?,
    };

    Ok(())
}

#[test]
fn verify_cli() {
    use clap::CommandFactory;
    Opts::command().debug_assert()
}
