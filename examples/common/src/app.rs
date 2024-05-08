//! This example showcases how to use Lightyear with Bevy, to easily get replication along with prediction/interpolation working.
//!
//! There is a lot of setup code, but it's mostly to have the examples work in all possible configurations of transport.
//! (all transports are supported, as well as running the example in listen-server or host-server mode)
//!
//!
//! Run with
//! - `cargo run -- server`
//! - `cargo run -- client -c 1`
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]

use std::net::SocketAddr;
use std::str::FromStr;

use bevy::asset::ron;
use bevy::log::{Level, LogPlugin};
use bevy::prelude::*;
use bevy::DefaultPlugins;
use bevy_inspector_egui::quick::WorldInspectorPlugin;
use clap::{Parser, ValueEnum};
use lightyear::prelude::client::ClientConfig;
use lightyear::prelude::*;
use lightyear::prelude::{client, server};
use lightyear::server::config::ServerConfig;
use lightyear::shared::log::add_log_layer;
use lightyear::transport::LOCAL_SOCKET;
use serde::{Deserialize, Serialize};

use crate::settings::*;
use crate::shared::shared_config;

#[derive(Parser, PartialEq, Debug)]
pub enum Cli {
    /// We have the client and the server running inside the same app.
    /// The server will also act as a client.
    #[cfg(not(target_family = "wasm"))]
    HostServer {
        #[arg(short, long, default_value = None)]
        client_id: Option<u64>,
    },
    #[cfg(not(target_family = "wasm"))]
    /// We will create two apps: a client app and a server app.
    /// Data gets passed between the two via channels.
    ListenServer {
        #[arg(short, long, default_value = None)]
        client_id: Option<u64>,
    },
    #[cfg(not(target_family = "wasm"))]
    /// Dedicated server
    Server,
    /// The program will act as a client
    Client {
        #[arg(short, long, default_value = None)]
        client_id: Option<u64>,
    },
}

/// Pars the CLI arguments.
/// `clap` doesn't run in wasm; we simply run in Client mode with a random ClientId
pub fn cli() -> Cli {
    cfg_if::cfg_if! {
        if #[cfg(target_family = "wasm")] {
            let client_id = rand::random::<u64>();
            Cli::Client {
                client_id: Some(client_id)
            }
        } else {
            Cli::parse()
        }
    }
}

/// Apps that will be returned from the `build_apps` function
pub enum Apps {
    /// A single app that contains only the ClientPlugins
    Client { app: App, config: ClientConfig },
    /// A single app that contains only the ServerPlugins
    Server { app: App, config: ServerConfig },
    /// Two apps (Client and Server) that will run in separate threads
    ListenServer {
        client_app: App,
        client_config: ClientConfig,
        server_app: App,
        server_config: ServerConfig,
    },
    /// A single app that contains both the Client and Server plugins
    HostServer {
        app: App,
        client_config: ClientConfig,
        server_config: ServerConfig,
    },
}

impl Apps {
    pub fn add_plugin_groups(&mut self) {
        match self {
            Apps::Client { app, config } => {
                app.add_plugins(client::ClientPlugins {
                    config: config.clone(),
                });
            }
            Apps::Server { app, config } => {
                app.add_plugins(server::ServerPlugins {
                    config: config.clone(),
                });
            }
            Apps::ListenServer {
                client_app,
                server_app,
                client_config,
                server_config,
            } => {
                client_app.add_plugins(client::ClientPlugins {
                    config: client_config.clone(),
                });
                server_app.add_plugins(server::ServerPlugins {
                    config: server_config.clone(),
                });
            }
            Apps::HostServer {
                app,
                client_config,
                server_config,
            } => {
                app.add_plugins(client::ClientPlugins {
                    config: client_config.clone(),
                });
                app.add_plugins(server::ServerPlugins {
                    config: server_config.clone(),
                });
            }
        }
    }

    /// Start running the apps
    pub fn run(self) {
        match self {
            Apps::Client { mut app, .. } => app.run(),
            Apps::Server { mut app, .. } => app.run(),
            Apps::ListenServer {
                mut client_app,
                mut server_app,
                ..
            } => {
                std::thread::spawn(move || server_app.run());
                client_app.run();
            }
            Apps::HostServer { mut app, .. } => {
                app.run();
            }
        }
    }
}

/// Build the bevy App.
pub fn build_app(settings: Settings, cli: Cli) -> Apps {
    match cli {
        // ListenServer using a single app
        #[cfg(not(target_family = "wasm"))]
        Cli::HostServer { client_id } => {
            let client_net_config = client::NetConfig::Local {
                id: client_id.unwrap_or(settings.client.client_id),
            };
            let (app, client_config, server_config) =
                combined_app(settings, vec![], client_net_config);
            Apps::HostServer {
                app,
                client_config,
                server_config,
            }
        }
        #[cfg(not(target_family = "wasm"))]
        Cli::ListenServer { client_id } => {
            // create client app
            let (from_server_send, from_server_recv) = crossbeam_channel::unbounded();
            let (to_server_send, to_server_recv) = crossbeam_channel::unbounded();
            // we will communicate between the client and server apps via channels
            let transport_config = TransportConfig::LocalChannel {
                recv: from_server_recv,
                send: to_server_send,
            };
            let net_config = build_client_netcode_config(
                client_id.unwrap_or(settings.client.client_id),
                // when communicating via channels, we need to use the address `LOCAL_SOCKET` for the server
                LOCAL_SOCKET,
                settings.client.conditioner.as_ref(),
                &settings.shared,
                transport_config,
            );
            let (client_app, client_config) = client_app(settings.clone(), net_config);

            // create server app
            let extra_transport_configs = vec![TransportConfig::Channels {
                // even if we communicate via channels, we need to provide a socket address for the client
                channels: vec![(LOCAL_SOCKET, to_server_recv, from_server_send)],
            }];
            let (server_app, server_config) = server_app(settings, extra_transport_configs);
            Apps::ListenServer {
                client_app,
                client_config,
                server_app,
                server_config,
            }
        }
        #[cfg(not(target_family = "wasm"))]
        Cli::Server => {
            let (app, config) = server_app(settings, vec![]);
            Apps::Server { app, config }
        }
        Cli::Client { client_id } => {
            let server_addr = SocketAddr::new(
                settings.client.server_addr.into(),
                settings.client.server_port,
            );
            // use the cli-provided client id if it exists, otherwise use the settings client id
            let client_id = client_id.unwrap_or(settings.client.client_id);
            let net_config = get_client_net_config(&settings, client_id);
            let (app, config) = client_app(settings, net_config);
            Apps::Client { app, config }
        }
    }
}

/// Build the client app with the `ClientPlugins` added.
/// Takes in a `net_config` parameter so that we configure the network transport.
fn client_app(settings: Settings, net_config: client::NetConfig) -> (App, ClientConfig) {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.build().set(LogPlugin {
        level: Level::INFO,
        filter: "wgpu=error,bevy_render=info,bevy_ecs=warn".to_string(),
        update_subscriber: Some(add_log_layer),
    }));
    if settings.client.inspector {
        app.add_plugins(WorldInspectorPlugin::new());
    }
    let client_config = client::ClientConfig {
        shared: shared_config(Mode::Separate),
        net: net_config,
        ..default()
    };
    (app, client_config)
}

/// Build the server app with the `ServerPlugins` added.
#[cfg(not(target_family = "wasm"))]
fn server_app(
    settings: Settings,
    extra_transport_configs: Vec<TransportConfig>,
) -> (App, ServerConfig) {
    let mut app = App::new();
    if !settings.server.headless {
        app.add_plugins(DefaultPlugins.build().disable::<LogPlugin>());
    } else {
        app.add_plugins(MinimalPlugins);
    }
    app.add_plugins(LogPlugin {
        level: Level::INFO,
        filter: "wgpu=error,bevy_render=info,bevy_ecs=warn".to_string(),
        update_subscriber: Some(add_log_layer),
    });

    if settings.server.inspector {
        app.add_plugins(WorldInspectorPlugin::new());
    }
    // configure the network configuration
    let mut net_configs = get_server_net_configs(&settings);
    let extra_net_configs = extra_transport_configs.into_iter().map(|c| {
        build_server_netcode_config(settings.server.conditioner.as_ref(), &settings.shared, c)
    });
    net_configs.extend(extra_net_configs);
    let server_config = ServerConfig {
        shared: shared_config(Mode::Separate),
        net: net_configs,
        ..default()
    };
    (app, server_config)
}

/// An `App` that contains both the client and server plugins
#[cfg(not(target_family = "wasm"))]
fn combined_app(
    settings: Settings,
    extra_transport_configs: Vec<TransportConfig>,
    client_net_config: client::NetConfig,
) -> (App, ClientConfig, ServerConfig) {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.build().set(LogPlugin {
        level: Level::INFO,
        filter: "wgpu=error,bevy_render=info,bevy_ecs=warn".to_string(),
        update_subscriber: Some(add_log_layer),
    }));
    if settings.client.inspector {
        app.add_plugins(WorldInspectorPlugin::new());
    }

    // server config
    let mut net_configs = get_server_net_configs(&settings);
    let extra_net_configs = extra_transport_configs.into_iter().map(|c| {
        build_server_netcode_config(settings.server.conditioner.as_ref(), &settings.shared, c)
    });
    net_configs.extend(extra_net_configs);
    let server_config = server::ServerConfig {
        shared: shared_config(Mode::HostServer),
        net: net_configs,
        ..default()
    };

    // client config
    let client_config = client::ClientConfig {
        shared: shared_config(Mode::HostServer),
        net: client_net_config,
        ..default()
    };
    (app, client_config, server_config)
}
