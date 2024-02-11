use anyhow::Result;
use bevy::prelude::Resource;

use crate::_reexport::ReadWordBuffer;
use crate::connection::netcode::ClientId;
use crate::prelude::{Io, IoConfig};
use crate::server::config::NetcodeConfig;

pub trait NetServer: Send + Sync {
    /// Start the server
    fn start(&mut self);

    /// Return the list of connected clients
    fn connected_client_ids(&self) -> Vec<ClientId>;

    /// Update the connection states + internal bookkeeping (keep-alives, etc.)
    fn try_update(&mut self, delta_ms: f64) -> Result<()>;

    /// Receive a packet from one of the connected clients
    fn recv(&mut self) -> Option<(ReadWordBuffer, ClientId)>;

    /// Send a packet to one of the connected clients
    fn send(&mut self, buf: &[u8], client_id: ClientId) -> Result<()>;

    fn new_connections(&self) -> Vec<ClientId>;

    fn new_disconnections(&self) -> Vec<ClientId>;

    fn io(&self) -> &Io;
}

#[derive(Resource)]
pub struct ServerConnection {
    server: Box<dyn NetServer>,
}

/// Configuration for the server connection
#[derive(Clone, Debug)]
pub enum NetConfig {
    Netcode { config: NetcodeConfig, io: IoConfig },
    // TODO: add steam-specific config
    Steam,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig::Netcode {
            config: NetcodeConfig::default(),
            io: IoConfig::default(),
        }
    }
}

impl NetConfig {
    pub fn build_server(self) -> ServerConnection {
        match self {
            NetConfig::Netcode { config, io } => {
                let io = io.get_io();
                let server = super::netcode::Server::new(config, io);
                ServerConnection {
                    server: Box::new(server),
                }
            }
            NetConfig::Steam => {
                unimplemented!()
            }
        }
    }
}

impl NetServer for ServerConnection {
    fn start(&mut self) {
        self.server.start()
    }

    fn connected_client_ids(&self) -> Vec<ClientId> {
        self.server.connected_client_ids()
    }

    fn try_update(&mut self, delta_ms: f64) -> Result<()> {
        self.server.try_update(delta_ms)
    }

    fn recv(&mut self) -> Option<(ReadWordBuffer, ClientId)> {
        self.server.recv()
    }

    fn send(&mut self, buf: &[u8], client_id: ClientId) -> Result<()> {
        self.server.send(buf, client_id)
    }

    fn new_connections(&self) -> Vec<ClientId> {
        self.server.new_connections()
    }

    fn new_disconnections(&self) -> Vec<ClientId> {
        self.server.new_disconnections()
    }

    fn io(&self) -> &Io {
        self.server.io()
    }
}
