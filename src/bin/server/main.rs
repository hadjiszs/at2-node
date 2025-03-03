use std::{io, process};

use at2_node::proto;
use drop::crypto::{key::exchange, sign};
use snafu::{OptionExt, ResultExt, Snafu};
use structopt::StructOpt;
use tokio::net;
use tonic::transport::Server;
use tracing::{subscriber, Level};
use tracing_fmt::FmtSubscriber;

mod accounts;
mod config;
mod recent_transactions;
mod rpc;

#[derive(Debug, StructOpt)]
enum Commands {
    Config(CommandsConfig),
    Run,
}

#[derive(Debug, StructOpt)]
enum CommandsConfig {
    New {
        node_address: String,
        rpc_address: String,
    },
    GetNode,
}

#[derive(Debug, Snafu)]
enum RunError {
    #[snafu(display("logging: {}", source))]
    Logging {
        source: tracing::dispatcher::SetGlobalDefaultError,
    },
    #[snafu(display("service: {}", source))]
    Service { source: rpc::Error },
    #[snafu(display("rpc: {}", source))]
    Rpc { source: tonic::transport::Error },
}

#[derive(Debug, Snafu)]
enum Error {
    #[snafu(display("config: {}", source))]
    Config { source: config::Error },
    #[snafu(display("config: resolve host: {}", source))]
    UnknownHost { source: io::Error },
    #[snafu(display("config: no host resolved"))]
    NoHost,
    #[snafu(display("run server: {}", source))]
    Run { source: RunError },
}

fn config(cmd: CommandsConfig) -> Result<(), Error> {
    match cmd {
        CommandsConfig::New {
            node_address,
            rpc_address,
        } => config::Config {
            addresses: config::ConfigAddresses {
                node: node_address,
                rpc: rpc_address,
            },
            keys: config::ConfigKeys {
                sign: sign::KeyPair::random().private(),
                network: exchange::KeyPair::random().secret().to_owned(),
            },
            nodes: vec![],
        }
        .to_writer(io::stdout())
        .context(Config),
        CommandsConfig::GetNode => {
            let config = config::from_reader(io::stdin()).context(Config)?;

            config::Nodes {
                nodes: vec![config::Node {
                    address: config.addresses.node,
                    public_key: exchange::KeyPair::new(config.keys.network)
                        .public()
                        .to_owned(),
                }],
            }
            .to_writer(io::stdout())
            .context(Config)
        }
    }
}

async fn run() -> Result<(), Error> {
    let config = config::from_reader(io::stdin()).context(Config)?;

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();
    subscriber::set_global_default(subscriber)
        .context(Logging)
        .context(Run)?;

    let service = rpc::Service::new(
        config.addresses.node,
        exchange::KeyPair::new(config.keys.network),
        config.nodes,
    )
    .await
    .context(Service)
    .context(Run)?;

    let web_config = tonic_web::config().allow_all_origins();

    Server::builder()
        .accept_http1(true)
        .add_service(web_config.enable(proto::at2_server::At2Server::new(service)))
        .serve(
            net::lookup_host(config.addresses.rpc)
                .await
                .context(UnknownHost)?
                .next()
                .context(NoHost)?,
        )
        .await
        .context(Rpc)
        .context(Run)?;

    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let ret = match Commands::from_args() {
        Commands::Config(cmd) => config(cmd),
        Commands::Run => run().await,
    };

    if let Err(err) = ret {
        eprintln!("error running cmd: {}", err);
        process::exit(1);
    }
}
