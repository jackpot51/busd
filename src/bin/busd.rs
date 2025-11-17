extern crate busd;

use std::{fs::File, io::Write, os::fd::FromRawFd, path::PathBuf};

use busd::{bus, config::Config};

use anyhow::Result;
use clap::Parser;
use tokio::{process, select, signal::unix::SignalKind};
use tracing::{error, info, warn};

/// A simple D-Bus broker.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// The address to listen on.
    /// Takes precedence over any `<listen>` element in the configuration file.
    #[clap(short = 'a', long, value_parser)]
    address: Option<String>,

    #[clap(short = 'c', long, value_parser)]
    command: Option<String>,

    /// Use the given configuration file.
    #[clap(long)]
    config: Option<PathBuf>,

    /// Print the address of the message bus to standard output.
    #[clap(long)]
    print_address: bool,

    /// File descriptor to which readiness notifications are sent.
    ///
    /// Once the server is listening to connections on the specified socket, it will print
    /// `READY=1\n` into this file descriptor and close it.
    ///
    /// This readiness notification mechanism which works on both systemd and s6.
    ///
    /// This feature is only available on unix-like platforms.
    #[clap(long)]
    ready_fd: Option<i32>,

    /// Equivalent to `--config /usr/share/dbus-1/session.conf`.
    /// This is the default if `--config` and `--system` are unspecified.
    #[clap(long)]
    session: bool,

    /// Equivalent to `--config /usr/share/dbus-1/system.conf`.
    #[clap(long)]
    system: bool,
}

async fn run_command(command_opt: Option<String>, bus_address: String) -> Result<()> {
    let Some(command) = command_opt else {
        // Simulate never ending command
        std::future::pending().await
    };
    //TODO: use shlex instead of sh -c?
    let mut child = process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("DBUS_SESSION_BUS_ADDRESS", bus_address)
        .spawn()?;
    let status = child.wait().await?;
    //TODO: use exit_status_error when stable
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("exit status {}", status))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    busd::tracing_subscriber::init();

    let args = Args::parse();

    let config_path = if args.system {
        PathBuf::from("/usr/share/dbus-1/system.conf")
    } else if let Some(config_path) = args.config {
        config_path
    } else {
        PathBuf::from("/usr/share/dbus-1/session.conf")
    };
    info!("reading configuration file {} ...", config_path.display());
    let config = Config::read_file(&config_path)?;

    let address = if let Some(address) = args.address {
        Some(address)
    } else {
        config.listen.as_ref().map(ToString::to_string)
    };

    let mut bus = bus::Bus::for_address(address.as_deref()).await?;

    if let Some(fd) = args.ready_fd {
        // SAFETY: We don't have any way to know if the fd is valid or not. The parent process is
        // responsible for passing a valid fd.
        let mut ready_file = unsafe { File::from_raw_fd(fd) };
        ready_file.write_all(b"READY=1\n")?;
    }

    if args.print_address {
        println!("{}", bus.address());
    }

    let command_future = run_command(args.command, bus.address().to_string());

    let mut sig_int = tokio::signal::unix::signal(SignalKind::interrupt())?;

    select! {
        _ = sig_int.recv() => {
            info!("Received SIGINT, shutting down..");
        },
        res = bus.run() => match res {
            Ok(()) => warn!("Bus stopped, shutting down.."),
            Err(e) => error!("Bus stopped with an error: {}", e),
        },
        res = command_future => match res {
            Ok(()) => info!("Command exited, shutting down.."),
            Err(err) => error!("Command exited with an error: {}", err),
        }
    }

    if let Err(e) = bus.cleanup().await {
        error!("Failed to clean up: {}", e);
    }

    Ok(())
}
