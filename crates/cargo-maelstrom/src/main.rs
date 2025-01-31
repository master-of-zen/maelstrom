use anyhow::{Context as _, Result};
use cargo_maelstrom::{
    cargo::{CompilationOptions, FeatureSelectionOptions, ManifestOptions},
    config::{Config, ConfigOptions, RunConfigOptions},
    main_app_new,
    progress::DefaultProgressDriver,
    ListAction, MainAppDeps,
};
use cargo_metadata::Metadata as CargoMetadata;
use clap::{Args, Parser, Subcommand};
use console::Term;
use figment::{
    error::Kind,
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use maelstrom_base::Timeout;
use maelstrom_client::ClientBgProcess;
use maelstrom_util::{clap as clap_util, process::ExitCode};
use std::{
    env,
    io::IsTerminal as _,
    path::{Path, PathBuf},
    process::Command,
};

/// Build Rust tests and send them to the broker for execution.
#[derive(Parser, Debug)]
#[command(
    after_help = r#"Configuration values can be specified in three ways: fields in a config file, environment variables, or command-line options. Command-line options have the highest precendence, followed by environment variables.

The configuration value 'config_value' would be set via the '--config-value' command-line option, the CARGO_MAELSTROM_CONFIG_VALUE environment variable, and the 'config_value' key in a configuration file.
"#
)]
#[command(version)]
#[command(styles = clap_util::styles())]
struct CliOptions {
    /// Configuration file. Values set in the configuration file will be overridden by values set
    /// through environment variables and values set on the command line. If not set, the file
    /// .config/cargo-maelstrom.toml in the workspace root will be used, if it exists.
    #[arg(long, short, value_name = "PATH")]
    config_file: Option<PathBuf>,

    /// Socket address of broker. Examples: "[::]:5000", "host.example.com:2000".
    #[arg(long, short, value_name = "SOCKADDR")]
    broker: Option<String>,

    #[command(subcommand)]
    command: CliCommand,

    #[command(flatten, next_help_heading = "Feature Selection")]
    cargo_feature_selection_options: FeatureSelectionOptions,

    #[command(flatten, next_help_heading = "Compilation Options")]
    cargo_compilation_options: CompilationOptions,

    #[command(flatten, next_help_heading = "Manifest Options")]
    cargo_manifest_options: ManifestOptions,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Run tests
    Run(CliRun),

    /// List tests, binaries, or packages
    List(CliList),
}

#[derive(Args, Debug)]
struct CliRun {
    /// Print configuration and exit
    #[arg(short = 'P', long)]
    print_config: bool,

    /// Don't output information about the tests being run
    #[arg(short, long)]
    quiet: bool,

    /// Only run tests which match the given filter. Can be specified multiple times
    #[arg(short, long, value_name = "FILTER_EXPRESSION", default_value = "all")]
    include: Vec<String>,

    /// Only run tests which don't match the given filter. Can be specified multiple times
    #[arg(short = 'x', long, value_name = "FILTER_EXPRESSION")]
    exclude: Vec<String>,

    /// Override timeout value for all tests specified (O indicates no timeout)
    #[arg(short, long)]
    timeout: Option<u32>,
}

#[derive(Args, Debug)]
struct CliList {
    #[command(subcommand)]
    what: Option<CliListType>,

    /// Print configuration and exit
    #[arg(short = 'P', long)]
    print_config: bool,

    /// Only list artifacts which match the given filter. Can be specified multiple times
    #[arg(short, long, value_name = "FILTER_EXPRESSION", default_value = "all")]
    include: Vec<String>,

    /// Only list artifacts which don't match the given filter. Can be specified multiple times
    #[arg(short = 'x', long, value_name = "FILTER_EXPRESSION")]
    exclude: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum CliListType {
    /// Only list the tests that would be run, don't actually run them. This is the default
    Tests,

    /// Only list the binaries that would be built, don't actually build them or run tests
    Binaries,

    /// Only list the package that exist, don't build anything or run any tests
    Packages,
}

fn config(config_file: impl AsRef<Path>, cli_options: ConfigOptions) -> Result<Config> {
    Figment::new()
        .merge(Serialized::defaults(ConfigOptions::default()))
        .merge(Toml::file(config_file))
        .merge(Env::prefixed("CARGO_MAELSTROM_"))
        .merge(Serialized::globals(cli_options))
        .extract()
        .map_err(|mut e| {
            if let Kind::MissingField(field) = &e.kind {
                e.kind = Kind::Message(format!("configuration value \"{field}\" was no provided"));
                e
            } else {
                e
            }
        })
        .context("reading configuration")
}

/// The main function for the client. This should be called on a task of its own. It will return
/// when a signal is received or when all work has been processed by the broker.
pub fn main() -> Result<ExitCode> {
    let bg_proc = ClientBgProcess::new_from_fork()?;

    let mut args = Vec::from_iter(env::args());
    if args.len() > 1 && args[0].ends_with(format!("cargo-{}", args[1]).as_str()) {
        args.remove(1);
    }
    let cli_options = CliOptions::parse_from(args);

    let cargo_metadata = Command::new("cargo")
        .args(["metadata", "--format-version=1"])
        .args(cli_options.cargo_feature_selection_options.iter())
        .args(cli_options.cargo_manifest_options.iter())
        .output()
        .context("getting cargo metadata")?;
    let cargo_metadata: CargoMetadata =
        serde_json::from_slice(&cargo_metadata.stdout).context("parsing cargo metadata")?;

    let config_file = match &cli_options.config_file {
        Some(path) => {
            if !path.exists() {
                eprintln!("warning: config file {} not found", path.display());
            }
            path.clone()
        }
        None => cargo_metadata
            .workspace_root
            .join(".config")
            .join("cargo-maelstrom.toml")
            .into(),
    };

    let (config, include, exclude, list_action, timeout_override) = match cli_options.command {
        CliCommand::List(CliList {
            what,
            include,
            exclude,
            print_config,
        }) => {
            let config = config(
                config_file,
                ConfigOptions {
                    broker: cli_options.broker,
                    run: RunConfigOptions { quiet: None },
                },
            )?;
            if print_config {
                println!("{config:#?}");
                return Ok(ExitCode::SUCCESS);
            }
            (
                config,
                include,
                exclude,
                Some(match what {
                    None | Some(CliListType::Tests) => ListAction::ListTests,
                    Some(CliListType::Binaries) => ListAction::ListBinaries,
                    Some(CliListType::Packages) => ListAction::ListPackages,
                }),
                None,
            )
        }
        CliCommand::Run(CliRun {
            include,
            exclude,
            print_config,
            quiet,
            timeout,
        }) => {
            let config = config(
                config_file,
                ConfigOptions {
                    broker: cli_options.broker,
                    run: RunConfigOptions {
                        quiet: quiet.then_some(true),
                    },
                },
            )?;
            if print_config {
                println!("{config:#?}");
                return Ok(ExitCode::SUCCESS);
            }
            (config, include, exclude, None, timeout.map(Timeout::new))
        }
    };

    let deps = MainAppDeps::new(
        bg_proc,
        "cargo".into(),
        include,
        exclude,
        list_action,
        std::io::stderr(),
        std::io::stderr().is_terminal(),
        &cargo_metadata.workspace_root,
        &cargo_metadata.workspace_packages(),
        config.broker,
        Default::default(),
        cli_options.cargo_feature_selection_options,
        cli_options.cargo_compilation_options,
        cli_options.cargo_manifest_options,
    )?;

    let stdout_tty = std::io::stdout().is_terminal();
    std::thread::scope(|scope| {
        let mut app = main_app_new(
            &deps,
            stdout_tty,
            config.run.quiet,
            Term::buffered_stdout(),
            DefaultProgressDriver::new(scope),
            timeout_override,
        )?;
        while !app.enqueue_one()?.is_done() {}
        app.drain()?;
        app.finish()
    })
}

#[test]
fn test_cli() {
    use clap::CommandFactory;
    CliOptions::command().debug_assert()
}
