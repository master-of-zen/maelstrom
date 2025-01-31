use anyhow::{Context as _, Result};
use clap::Parser;
use figment::{
    error::Kind,
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use maelstrom_linux::{
    self as linux, CloneArgs, CloneFlags, PollEvents, PollFd, Signal, WaitStatus,
};
use maelstrom_util::{config::LogLevel, fs::Fs};
use maelstrom_worker::config::{Config, ConfigOptions};
use slog::{o, Drain, Level, LevelFilter, Logger};
use slog_async::Async;
use slog_term::{FullFormat, TermDecorator};
use std::{path::PathBuf, process, slice, time::Duration};
use tokio::runtime::Runtime;

/// The maelstrom worker. This process executes jobs as directed by the broker.
#[derive(Parser)]
#[command(
    after_help = r#"Configuration values can be specified in three ways: fields in a config file, environment variables, or command-line options. Command-line options have the highest precendence, followed by environment variables.

The configuration value 'config_value' would be set via the '--config-value' command-line option, the MAELSTROM_WORKER_CONFIG_VALUE environment variable, and the 'config_value' key in a configuration file.

All values except for 'broker' have reasonable defaults.
"#
)]
#[command(version)]
#[command(styles = maelstrom_util::clap::styles())]
struct CliOptions {
    /// Configuration file. Values set in the configuration file will be overridden by values set
    /// through environment variables and values set on the command line.
    #[arg(
        long,
        short = 'c',
        value_name = "PATH",
        default_value = PathBuf::from(".config/maelstrom-worker.toml").into_os_string()
    )]
    config_file: PathBuf,

    /// Print configuration and exit
    #[arg(long, short = 'P')]
    print_config: bool,

    /// Socket address of broker. Examples: "[::]:5000", "host.example.com:2000".
    #[arg(long, short, value_name = "SOCKADDR")]
    broker: Option<String>,

    /// The number of job slots available. Most jobs will take one job slot
    #[arg(long, short, value_name = "N")]
    slots: Option<u16>,

    /// The directory to use for the cache.
    #[arg(long, short = 'r', value_name = "PATH")]
    cache_root: Option<PathBuf>,

    /// The target amount of disk space to use for the cache. This bound won't be followed
    /// strictly, so it's best to be conservative.
    #[arg(long, short = 'B', value_name = "BYTES")]
    cache_bytes_used_target: Option<u64>,

    /// The maximum amount of bytes to return inline for captured stdout and stderr.
    #[arg(long, short, value_name = "BYTES")]
    inline_limit: Option<u64>,

    /// Minimum log level to output.
    #[arg(long, short = 'l', value_name = "LEVEL", value_enum)]
    log_level: Option<LogLevel>,
}

impl CliOptions {
    fn to_config_options(&self) -> ConfigOptions {
        ConfigOptions {
            broker: self.broker.clone(),
            slots: self.slots,
            cache_root: self.cache_root.clone(),
            cache_bytes_used_target: self.cache_bytes_used_target,
            inline_limit: self.inline_limit,
            log_level: self.log_level,
        }
    }
}

/// Clone a child process and continue executing in the child. The child process will be in a new
/// pid namespace, meaning when it terminates all of its descendant processes will also terminate.
/// The child process will also be in a new user namespace, and have uid 0, gid 0 in that
/// namespace. The user namespace is required in order to create the pid namespace.
///
/// WARNING: This function must only be called while the program is single-threaded.
fn clone_into_pid_and_user_namespace() -> Result<()> {
    let parent_uid = linux::getuid();
    let parent_gid = linux::getgid();

    // Create a parent pidfd. We'll use this in the child to see if the parent has terminated
    // early.
    let parent_pidfd = linux::pidfd_open(linux::getpid())?;

    // Clone a new process into new user and pid namespaces.
    let mut clone_args = CloneArgs::default()
        .flags(CloneFlags::NEWUSER | CloneFlags::NEWPID)
        .exit_signal(Signal::CHLD);
    match linux::clone3(&mut clone_args)? {
        None => {
            // Child.

            // Set parent death signal.
            linux::prctl_set_pdeathsig(Signal::KILL)?;

            // Check if the parent has already terminated.
            let mut pollfd = PollFd::new(parent_pidfd, PollEvents::IN);
            if linux::poll(slice::from_mut(&mut pollfd), Duration::ZERO)? == 1 {
                process::abort();
            }

            // We are done with the parent_pidfd now.
            linux::close(parent_pidfd)?;

            // Map uid and guid.
            let fs = Fs::new();
            fs.write("/proc/self/setgroups", "deny\n")?;
            fs.write("/proc/self/uid_map", format!("0 {parent_uid} 1\n"))?;
            fs.write("/proc/self/gid_map", format!("0 {parent_gid} 1\n"))?;

            Ok(())
        }
        Some(child_pid) => {
            // Parent.

            // The parent_pidfd is only used in the child.
            linux::close(parent_pidfd)
                .unwrap_or_else(|err| panic!("unexpected error closing pidfd: {}", err));

            // Wait for the child and mimick how it terminated.
            match linux::waitpid(child_pid).unwrap_or_else(|e| {
                panic!("unexpected error waiting on child process {child_pid}: {e}")
            }) {
                WaitStatus::Exited(code) => {
                    process::exit(code.as_u8().into());
                }
                WaitStatus::Signaled(signal) => {
                    linux::raise(signal).unwrap_or_else(|e| {
                        panic!("unexpected error raising signal {signal}: {e}")
                    });
                    process::abort();
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let cli_options = CliOptions::parse();
    let config: Config = Figment::new()
        .merge(Serialized::defaults(ConfigOptions::default()))
        .merge(Toml::file(&cli_options.config_file))
        .merge(Env::prefixed("MAELSTROM_WORKER_"))
        .merge(Serialized::globals(cli_options.to_config_options()))
        .extract()
        .map_err(|mut e| {
            if let Kind::MissingField(field) = &e.kind {
                e.kind = Kind::Message(format!("configuration value \"{field}\" was no provided"));
                e
            } else {
                e
            }
        })
        .context("reading configuration")?;
    if cli_options.print_config {
        println!("{config:#?}");
        return Ok(());
    }
    clone_into_pid_and_user_namespace()?;
    let decorator = TermDecorator::new().build();
    let drain = FullFormat::new(decorator).build().fuse();
    let drain = Async::new(drain).build().fuse();
    let level = match config.log_level {
        LogLevel::Error => Level::Error,
        LogLevel::Warning => Level::Warning,
        LogLevel::Info => Level::Info,
        LogLevel::Debug => Level::Debug,
    };
    let drain = LevelFilter::new(drain, level).fuse();
    let log = Logger::root(drain, o!());
    Runtime::new()
        .context("starting tokio runtime")?
        .block_on(async move { maelstrom_worker::main(config, log).await })?;
    Ok(())
}

#[test]
fn test_cli() {
    use clap::CommandFactory;
    CliOptions::command().debug_assert()
}
