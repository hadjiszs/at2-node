use futures::future::join_all;
use std::iter::{repeat_with, Extend};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use std::{env, fs, io};
use tokio::{net::TcpStream, task::yield_now};

use duct::cmd;

const SERVER_BIN: &str = env!("CARGO_BIN_EXE_server");
const CRATE_ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn next_test_id() -> u8 {
    static COUNTER: AtomicU8 = AtomicU8::new(0);

    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn next_test_port() -> u16 {
    static PORT_OFFSET: AtomicU16 = AtomicU16::new(0);
    const PORT_START: u16 = 3000;

    PORT_START + PORT_OFFSET.fetch_add(1, Ordering::Relaxed)
}

fn next_test_ip4() -> SocketAddr {
    (Ipv4Addr::new(127, 0, 0, 1), next_test_port()).into()
}

struct Server {
    handle: duct::Handle,
}

impl Drop for Server {
    fn drop(&mut self) {
        use nix::sys::signal::{self, Signal};
        use nix::unistd::Pid;
        use std::thread;

        self.handle.pids().iter().for_each(|pid| {
            let _ = signal::kill(Pid::from_raw(*pid as i32), Signal::SIGTERM);
        });

        let timeout = Instant::now() + Duration::from_secs(1);
        while Instant::now() < timeout {
            if let Ok(None) = self.handle.try_wait() {
                thread::sleep(Duration::from_millis(10));
            }
        }

        self.handle.kill().expect("kill server");
    }
}

type ServerConfig = Vec<u8>;
type NodeConfig = Vec<u8>;

fn gen_server_cmd(server_args: Vec<&str>) -> duct::Expression {
    let kcov_args_env = env::var("KCOV_ARGS");
    if kcov_args_env.is_err() {
        return cmd(SERVER_BIN, server_args);
    }
    let kcov_args = kcov_args_env.unwrap();

    let mut args: Vec<String> = kcov_args.split(' ').map(|s| s.to_string()).collect();

    let outdir_prefix = args
        .pop()
        .expect("KCOV_ARGS should contains an outdir prefix");
    let mut outdir = PathBuf::new();
    outdir.push(CRATE_ROOT);
    outdir.push(format!("{}{}", outdir_prefix, next_test_id()));

    fs::create_dir(&outdir).expect("create output dir");
    args.push(outdir.to_str().unwrap().to_string());

    args.push(SERVER_BIN.into());
    args.extend(server_args.iter().map(|a| a.to_string()));

    cmd("kcov", &args)
}

fn gen_config(node: &SocketAddr, rpc: &SocketAddr) -> (ServerConfig, NodeConfig) {
    let full_config = gen_server_cmd(vec!["config", "new", &node.to_string(), &rpc.to_string()])
        .stdout_capture()
        .run()
        .expect("generate config")
        .stdout;

    let node_config = gen_server_cmd(vec!["config", "get-node"])
        .stdin_bytes(full_config.clone())
        .stdout_capture()
        .run()
        .expect("get node config")
        .stdout;

    (full_config, node_config)
}

fn start_server(server_config: ServerConfig) -> Server {
    Server {
        handle: gen_server_cmd(vec!["run"])
            .stdin_bytes(server_config)
            .start()
            .expect("run server"),
    }
}

async fn wait_until_connect(server: &Server, to_probe: &SocketAddr) {
    while let Err(err) = TcpStream::connect(to_probe).await {
        if err.kind() != io::ErrorKind::ConnectionRefused {
            panic!("connect server: {}", err)
        }

        if let Err(err) = server.handle.try_wait() {
            server
                .handle
                .wait()
                .unwrap_or_else(|_| panic!("server finished early: {}", err));
        }

        yield_now().await;
    }
}

#[tokio::test]
async fn server_without_network_fails() {
    let (node, rpc) = (next_test_ip4(), next_test_ip4());

    let (server_config, _) = gen_config(&node, &rpc);

    let server = start_server(server_config);
    let exit = server.handle.wait();
    assert_eq!(exit.err().map(|err| err.kind()), Some(io::ErrorKind::Other))
}

async fn start_network(size: usize) -> SocketAddr {
    let addresses = repeat_with(|| (next_test_ip4(), next_test_ip4()))
        .take(size)
        .collect::<Vec<_>>();

    let (mut server_configs, node_configs): (Vec<_>, Vec<_>) = addresses
        .iter()
        .map(|(node, rpc)| gen_config(&node, &rpc))
        .unzip();

    server_configs
        .iter_mut()
        .enumerate()
        .for_each(|(server_pos, server_config)| {
            server_config.extend(
                node_configs
                    .iter()
                    .enumerate()
                    .filter(|(node_pos, _)| *node_pos != server_pos)
                    .map(|(_, node_config)| node_config)
                    .flatten(),
            )
        });

    let servers: Vec<_> = server_configs
        .iter()
        .map(|server_config| start_server(server_config.clone()))
        .collect();

    join_all(servers.iter().zip(&addresses).flat_map(|(server, addrs)| {
        vec![
            wait_until_connect(&server, &addrs.0),
            wait_until_connect(&server, &addrs.1),
        ]
    }))
    .await;

    addresses
        .iter()
        .map(|(_, rpc)| rpc)
        .copied()
        .next()
        .expect("zero sized network")
}

#[tokio::test]
async fn can_run_network() {
    start_network(3).await;
}
