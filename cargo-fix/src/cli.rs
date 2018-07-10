use std::env;
use std::io::Write;
use std::process::Command;

use clap::{App, AppSettings, Arg, SubCommand};
use failure::{Error, ResultExt};
use termcolor::{ColorSpec, StandardStream, WriteColor};

use super::exit_with;
use diagnostics::{self, log_for_human, output_stream, write_warning, Message};
use lock;
use vcs::VersionControl;

static PLEASE_REPORT_THIS_BUG: &str =
    "\
     This likely indicates a bug in either rustc or rustfix itself,\n\
     and we would appreciate a bug report! You're likely to see \n\
     a number of compiler warnings after this message which rustfix\n\
     attempted to fix but failed. If you could open an issue at\n\
     https://github.com/rust-lang-nursery/rustfix/issues\n\
     quoting the full output of this command we'd be very appreciative!\n\n\
     ";

enum CargoArg {
    OneFlag(&'static str, &'static str),
    ManyValue(&'static str, &'static str),
    OneValue(&'static str, &'static str),
}

const CARGO_ARGS: &[CargoArg] = &[
    CargoArg::ManyValue("package", "Package(s) to fix"),
    CargoArg::OneFlag("all", "Fix all packages in the workspace"),
    CargoArg::ManyValue("exclude", "Exclude packages from the build"),
    CargoArg::OneFlag("lib", "Fix only this package's library"),
    CargoArg::ManyValue("bin", "Fix only the specified binary"),
    CargoArg::OneFlag("bins", "Fix all binaries"),
    CargoArg::ManyValue("example", "Fix only the specified example"),
    CargoArg::OneFlag("examples", "Fix all examples"),
    CargoArg::ManyValue("test", "Fix only the specified test"),
    CargoArg::OneFlag("tests", "Fix all tests"),
    CargoArg::ManyValue("bench", "Fix only the specified bench"),
    CargoArg::OneFlag("benches", "Fix all benchess"),
    CargoArg::OneFlag("all-targets", "Fix all targets (lib and bin are default)"),
    CargoArg::OneFlag("release", "Fix for release mode"),
    CargoArg::OneValue("features", "Space-separated list of features"),
    CargoArg::OneFlag("all-features", "Activate all available features"),
    CargoArg::OneFlag("no-default-features", "Don't activate `default` feature"),
    CargoArg::OneValue("target", "Build for the target triple"),
    CargoArg::OneValue("target-dir", "Directory for all generated artifacts"),
    CargoArg::OneValue("manifest-path", "Path to Cargo.toml"),
];

pub fn run() -> Result<(), Error> {
    let mut cmd = SubCommand::with_name("fix")
        .version(env!("CARGO_PKG_VERSION"))
        .author("The Rust Project Developers")
        .about("Automatically apply rustc's suggestions about fixing code")
        .setting(AppSettings::TrailingVarArg)
        .arg(
            Arg::with_name("args")
                .multiple(true)
                .help("Arguments to forward to underlying `cargo check`")
        )
        .arg(
            Arg::with_name("broken-code")
                .long("broken-code")
                .help("Fix code even if it already has compiler errors"),
        )
        .arg(
            Arg::with_name("edition")
                .long("prepare-for")
                .help("Fix warnings in preparation of an edition upgrade")
                .takes_value(true)
                .possible_values(&["2018"]),
        )
        .arg(
            Arg::with_name("allow-no-vcs")
                .long("allow-no-vcs")
                .help("Fix code even if a VCS was not detected"),
        )
        .arg(
            Arg::with_name("allow-dirty")
                .long("allow-dirty")
                .help("Fix code even if the working directory is dirty"),
        )
        .after_help("\
This Cargo subcommmand will automatically take rustc's suggestions from
diagnostics like warnings and apply them to your source code. This is intended
to help automate tasks that rustc itself already knows how to tell you to fix!
The `cargo fix` subcommand is also being developed for the Rust 2018 edition
to provide code the ability to easily opt-in to the new edition without having
to worry about any breakage.

Executing `cargo fix` will under the hood execute `cargo check`. Any warnings
applicable to your crate will be automatically fixed (if possible) and all
remaining warnings will be displayed when the check process is finished. For
example if you'd like to prepare for the 2018 edition, you can do so by
executing:

    cargo fix --prepare-for 2018

Note that this is not guaranteed to fix all your code as it only fixes code that
`cargo check` would otherwise compile. For example unit tests are left out
from this command, but they can be checked with:

    cargo fix --prepare-for 2018 --all-targets

which behaves the same as `cargo check --all-targets`. Similarly if you'd like
to fix code for different platforms you can do:

    cargo fix --prepare-for 2018 --target x86_64-pc-windows-gnu

or if your crate has optional features:

    cargo fix --prepare-for 2018 --no-default-features --features foo

If you encounter any problems with `cargo fix` or otherwise have any questions
or feature requests please don't hesitate to file an issue at
https://github.com/rust-lang-nursery/rustfix
");

    for arg in CARGO_ARGS {
        match arg {
            CargoArg::OneFlag(name, help) => {
                cmd = cmd.arg(Arg::with_name(name).long(name).help(help));
            }
            CargoArg::OneValue(name, help) => {
                cmd = cmd.arg(
                    Arg::with_name(name)
                        .long(name)
                        .help(help)
                        .takes_value(true)
                );
            }
            CargoArg::ManyValue(name, help) => {
                let mut arg = Arg::with_name(name)
                    .long(name)
                    .help(help)
                    .takes_value(true)
                    .multiple(true);
                if *name == "package" {
                    arg = arg.short("p");
                }
                cmd = cmd.arg(arg);
            }
        }
    }
    let matches = App::new("Cargo Fix")
        .bin_name("cargo")
        .subcommand(cmd)
        .get_matches();

    let matches = match matches.subcommand() {
        ("fix", Some(matches)) => matches,
        _ => bail!("unknown CLI arguments passed"),
    };

    if matches.is_present("broken-code") {
        env::set_var("__CARGO_FIX_BROKEN_CODE", "1");
    }

    check_version_control(matches)?;

    // Spin up our lock server which our subprocesses will use to synchronize
    // fixes.
    let _lock_server = lock::Server::new()?.start()?;

    // Spin up our diagnostics server which our subprocesses will use to send
    // use their dignostics messages in an ordered way.
    let _diagnostics_server = diagnostics::Server::new()?.start(|m, stream| {
        if let Err(e) = log_message(&m, stream) {
            warn!("failed to log message: {}", e);
        }
    })?;

    let cargo = env::var_os("CARGO").unwrap_or("cargo".into());
    let mut cmd = Command::new(&cargo);
    // TODO: shouldn't hardcode `check` here, we want to allow things like
    // `cargo fix bench` or something like that
    //
    // TODO: somehow we need to force `check` to actually do something here, if
    // `cargo check` was previously run it won't actually do anything again.
    cmd.arg("check");

    // Handle all of Cargo's arguments that we parse and forward on to Cargo
    for arg in CARGO_ARGS {
        match arg {
            CargoArg::OneFlag(name, _) => {
                if matches.is_present(name) {
                    cmd.arg(format!("--{}", name));
                }
            }
            CargoArg::OneValue(name, _) => {
                if let Some(value) = matches.value_of(name) {
                    cmd.arg(format!("--{}", name)).arg(value);
                }
            }
            CargoArg::ManyValue(name, _) => {
                if let Some(values) = matches.values_of(name) {
                    for value in values {
                        cmd.arg(format!("--{}", name)).arg(value);
                    }
                }
            }
        }
    }

    // Handle the forwarded arguments after `--`
    if let Some(args) = matches.values_of("args") {
        cmd.args(args);
    }

    // Override the rustc compiler as ourselves. That way whenever rustc would
    // run we run instead and have an opportunity to inject fixes.
    let me = env::current_exe().context("failed to learn about path to current exe")?;
    cmd.env("RUSTC", &me).env("__CARGO_FIX_NOW_RUSTC", "1");
    if let Some(rustc) = env::var_os("RUSTC") {
        cmd.env("RUSTC_ORIGINAL", rustc);
    }

    // Trigger edition-upgrade mode. Currently only supports the 2018 edition.
    info!("edition upgrade? {:?}", matches.value_of("edition"));
    if let Some("2018") = matches.value_of("edition") {
        info!("edition upgrade!");
        let mut rustc_flags = env::var_os("RUSTFLAGS").unwrap_or_else(|| "".into());
        rustc_flags.push(" -W rust-2018-compatibility");
        cmd.env("RUSTFLAGS", &rustc_flags);
    }

    // An now execute all of Cargo! This'll fix everything along the way.
    //
    // TODO: we probably want to do something fancy here like collect results
    // from the client processes and print out a summary of what happened.
    let status = cmd.status()
        .with_context(|e| format!("failed to execute `{}`: {}", cargo.to_string_lossy(), e))?;
    exit_with(status);
}

fn check_version_control(matches: &::clap::ArgMatches) -> Result<(), Error> {
    // Useful for tests
    if env::var("__CARGO_FIX_IGNORE_VCS").is_ok() {
        return Ok(());
    }

    let version_control = VersionControl::new();
    match (version_control.is_present(), version_control.is_dirty()?) {
        (true, None) => {} // clean and versioned slate
        (false, _) => {
            let stream = &mut output_stream();

            write_warning(stream)?;
            stream.set_color(ColorSpec::new().set_bold(true))?;
            writeln!(stream, "Could not detect a version control system")?;
            stream.reset()?;
            writeln!(stream, "You should consider using a VCS so you can easily see and revert rustfix's changes.")?;

            if !matches.is_present("allow-no-vcs") {
                bail!("No VCS found, aborting. Overwrite this behavior with `--allow-no-vcs`.");
            }
        }
        (true, Some(output)) => {
            let stream = &mut output_stream();

            write_warning(stream)?;
            stream.set_color(ColorSpec::new().set_bold(true))?;
            writeln!(stream, "Working directory dirty")?;
            stream.reset()?;
            writeln!(stream, "Make sure your working directory is clean so you can easily revert rustfix's changes.")?;

            stream.write_all(&output)?;

            if !matches.is_present("allow-dirty") {
                bail!("Aborting because of dirty working directory. Overwrite this behavior with `--allow-dirty`.");
            }
        }
    }

    Ok(())
}

fn log_message(msg: &Message, stream: &mut StandardStream) -> Result<(), Error> {
    use diagnostics::Message::*;

    match *msg {
        Fixing {
            ref file,
            ref fixes,
        } => {
            log_for_human(
                "Fixing",
                &format!(
                    "{name} ({n} {fixes})",
                    name = file,
                    n = fixes,
                    fixes = if *fixes > 1 { "fixes" } else { "fix" },
                ),
                stream,
            )?;
        }
        ReplaceFailed {
            ref file,
            ref message,
        } => {
            write_warning(stream)?;
            stream.set_color(ColorSpec::new().set_bold(true))?;
            write!(stream, "error applying suggestions to `{}`\n", file)?;
            stream.reset()?;
            write!(stream, "The full error message was:\n\n> {}\n\n", message)?;
            stream.write(PLEASE_REPORT_THIS_BUG.as_bytes())?;
        }
        FixFailed {
            ref files,
            ref krate,
        } => {
            write_warning(stream)?;
            stream.set_color(ColorSpec::new().set_bold(true))?;
            if let Some(ref krate) = *krate {
                write!(
                    stream,
                    "failed to automatically apply fixes suggested by rustc \
                     to crate `{}`\n",
                    krate,
                )?;
            } else {
                write!(
                    stream,
                    "failed to automatically apply fixes suggested by rustc\n"
                )?;
            }
            if files.len() > 0 {
                write!(
                    stream,
                    "\nafter fixes were automatically applied the compiler \
                     reported errors within these files:\n\n"
                )?;
                for file in files {
                    write!(stream, "  * {}\n", file)?;
                }
                write!(stream, "\n")?;
            }
            stream.write(PLEASE_REPORT_THIS_BUG.as_bytes())?;
        }
    }

    stream.reset()?;
    stream.flush()?;
    Ok(())
}
