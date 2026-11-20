//! `headhub`: client CLI that shares the hub's headset with this PC.

mod adapters;
mod agent;
mod ports;

use std::{env, fs, net::UdpSocket, path::PathBuf, process::Command, time::Duration};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use headhub_core::protocol::{DEFAULT_PORT, MAX_PACKET, Packet};
use serde::{Deserialize, Serialize};

use crate::{
    adapters::Platform,
    ports::{Autostart, VirtualCable},
};

/// How long `activate` and `status` wait for the hub before reporting it unreachable.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Parser)]
#[command(about = "Usa o headset do hub head-hub neste PC")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Configura este PC, confere os dispositivos virtuais e inicia o agente no login.
    Setup {
        /// Endereço do hub (IP ou host, porta opcional).
        #[arg(long)]
        hub: String,
        /// Token de pareamento definido no hub.
        #[arg(long)]
        token: String,
        /// Nome deste PC no hub; padrão: hostname.
        #[arg(long)]
        name: Option<String>,
    },
    /// Roda o agente em primeiro plano (o serviço de login usa este comando).
    Run,
    /// Passa o headset para este PC, ou para o PC informado.
    Activate { name: Option<String> },
    /// Mostra o PC ativo e os PCs conectados.
    Status,
    /// Remove o início automático.
    Uninstall,
}

/// Saved by `setup`, read by every other command.
#[derive(Serialize, Deserialize)]
pub(crate) struct Config {
    /// `host:port` of the hub.
    pub(crate) hub: String,
    /// Pairing token; kept out of `Debug` output.
    pub(crate) token: String,
    /// Name this PC registers with.
    pub(crate) name: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("hub", &self.hub)
            .field("token", &"<redacted>")
            .field("name", &self.name)
            .finish()
    }
}

impl Config {
    fn path() -> Result<PathBuf> {
        let home = env::var_os("HOME").context("HOME não definido")?;
        Ok(PathBuf::from(home).join(".config/headhub/config.toml"))
    }

    fn load() -> Result<Self> {
        let path = Self::path()?;
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "sem configuração em {}; rode `headhub setup`",
                path.display()
            )
        })?;
        Ok(toml::from_str(&text)?)
    }

    fn save(&self) -> Result<()> {
        let path = Self::path()?;
        fs::create_dir_all(path.parent().expect("config path has a parent"))?;
        fs::write(&path, toml::to_string(self)?)?;
        Ok(())
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let platform = Platform;
    match Cli::parse().command {
        Cmd::Setup { hub, token, name } => {
            let name = match name {
                Some(name) => name,
                None => hostname()?,
            };
            ensure!(
                !name.is_empty() && name.len() <= 255,
                "nome inválido: '{name}'"
            );
            let hub = if hub.contains(':') {
                hub
            } else {
                format!("{hub}:{DEFAULT_PORT}")
            };
            let config = Config { hub, token, name };
            platform.prepare()?;
            config.save()?;
            platform.install(&env::current_exe()?)?;
            println!("pronto: '{}' usando o hub {}", config.name, config.hub);
        }
        Cmd::Run => agent::run(&Config::load()?, &platform.prepare()?)?,
        Cmd::Activate { name } => {
            let config = Config::load()?;
            let name = name.unwrap_or_else(|| config.name.clone());
            print_status(
                &config,
                &Packet::Activate {
                    token: &config.token,
                    name: &name,
                },
            )?;
        }
        Cmd::Status => {
            let config = Config::load()?;
            print_status(
                &config,
                &Packet::Query {
                    token: &config.token,
                },
            )?;
        }
        Cmd::Uninstall => platform.uninstall()?,
    }
    Ok(())
}

/// Sends one control packet and prints the hub's status reply.
fn print_status(config: &Config, request: &Packet) -> Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect(&config.hub)?;
    socket.set_read_timeout(Some(REPLY_TIMEOUT))?;
    let mut out = Vec::new();
    request.encode(&mut out)?;
    socket.send(&out)?;
    let mut buf = [0; MAX_PACKET];
    loop {
        let len = socket
            .recv(&mut buf)
            .with_context(|| format!("hub {} não respondeu", config.hub))?;
        if let Ok(Packet::Status { active, clients }) = Packet::decode(&buf[..len]) {
            println!("ativo: {}", active.unwrap_or("nenhum"));
            println!(
                "conectados: {}",
                if clients.is_empty() {
                    "nenhum".into()
                } else {
                    clients.join(", ")
                }
            );
            return Ok(());
        }
    }
}

fn hostname() -> Result<String> {
    let out = Command::new("hostname").output()?;
    let full = String::from_utf8(out.stdout)?;
    Ok(full.trim().split('.').next().unwrap_or_default().to_owned())
}
