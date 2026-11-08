mod args;
mod cli;
mod client;
mod compat;
mod error;
#[cfg(test)]
mod history;
mod mcp;
mod spec;
#[cfg(test)]
mod testing;
mod upload;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::cli::Output;
use crate::client::{DEFAULT_PORT, Endpoint};

/// Controls VCam through the VCam API.
///
/// Call any API method by its name, e.g. `vcam scene.load --sceneId 3`.
/// Run `vcam methods` for the list and `vcam <method> --help` for its parameters.
/// Connections from other devices read the token from VCAM_TOKEN.
#[derive(Parser)]
#[command(name = "vcam", version, about, long_about)]
struct Cli {
    /// Host running VCam
    #[arg(long, env = "VCAM_HOST", default_value = "127.0.0.1", global = true)]
    host: String,

    /// Port of the VCam API
    #[arg(long, env = "VCAM_PORT", default_value_t = DEFAULT_PORT, global = true)]
    port: u16,

    /// Prints JSON on one line, including errors
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Lists the API methods
    Methods,

    /// Shows the parameters and result of a method
    Describe { method: String },

    /// Prints events as JSON Lines until stopped
    Events {
        /// Event names to receive. Omit to receive all.
        events: Vec<String>,

        /// Stops after this many events
        #[arg(long)]
        count: Option<usize>,

        /// Stops after this many seconds. Fails when --count was not reached by then.
        #[arg(long)]
        timeout: Option<f64>,
    },

    /// Runs as an MCP server over stdio
    Mcp {
        /// Offers only tools in these namespaces, e.g. scene,object,text
        #[arg(long, value_delimiter = ',')]
        tools: Vec<String>,
    },

    /// Calls an API method, e.g. `vcam scene.load --sceneId 3`
    #[command(external_subcommand)]
    Call(Vec<String>),
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let endpoint = Endpoint {
        host: cli.host,
        port: cli.port,
        token: std::env::var("VCAM_TOKEN").ok().filter(|t| !t.is_empty()),
    };
    let output = Output { json: cli.json };
    let outcome = match cli.command {
        Command::Methods => cli::methods(&endpoint, output).await,
        Command::Describe { method } => cli::describe(&endpoint, &method, output).await,
        Command::Events {
            events,
            count,
            timeout,
        } => cli::events(&endpoint, events, count, timeout).await,
        Command::Mcp { tools } => mcp::serve(endpoint, tools).await,
        Command::Call(args) => cli::call(&endpoint, args, output).await,
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            output.print_error(&error);
            ExitCode::from(error.exit_code())
        }
    }
}
