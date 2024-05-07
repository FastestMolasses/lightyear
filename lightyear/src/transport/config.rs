use bevy::prelude::Reflect;
use std::fmt::{Debug, Formatter};
use std::net::{IpAddr, SocketAddr};

use crossbeam_channel::{Receiver, Sender};

#[cfg(all(feature = "webtransport", not(target_family = "wasm")))]
use {
    crate::transport::webtransport::server::WebTransportServerSocketBuilder,
    wtransport::tls::Identity,
};

use crate::prelude::Io;
use crate::transport::channels::Channels;
use crate::transport::dummy::DummyIo;
use crate::transport::error::Result;
use crate::transport::io::IoStats;
use crate::transport::local::LocalChannelBuilder;
#[cfg(feature = "zstd")]
use crate::transport::middleware::compression::zstd::{
    compression::ZstdCompressor, decompression::ZstdDecompressor,
};
use crate::transport::middleware::compression::CompressionConfig;
use crate::transport::middleware::conditioner::{LinkConditioner, LinkConditionerConfig};
use crate::transport::middleware::{PacketReceiverWrapper, PacketSenderWrapper};
#[cfg(not(target_family = "wasm"))]
use crate::transport::udp::UdpSocketBuilder;
#[cfg(feature = "websocket")]
use crate::transport::websocket::client::WebSocketClientSocketBuilder;
#[cfg(all(feature = "websocket", not(target_family = "wasm")))]
use crate::transport::websocket::server::WebSocketServerSocketBuilder;
#[cfg(feature = "webtransport")]
use crate::transport::webtransport::client::WebTransportClientSocketBuilder;
use crate::transport::{BoxedReceiver, Transport, TransportBuilder, TransportBuilderEnum};

/// Use this to configure the [`Transport`] that will be used to establish a connection with the
/// remote.
pub enum TransportConfig {
    /// Use a [`UdpSocket`](std::net::UdpSocket)
    #[cfg(not(target_family = "wasm"))]
    UdpSocket(SocketAddr),
    /// Use [`WebTransport`](https://wicg.github.io/web-transport/) as a transport layer
    #[cfg(feature = "webtransport")]
    WebTransportClient {
        client_addr: SocketAddr,
        server_addr: SocketAddr,
        /// On wasm, we need to provide a hash of the certificate to the browser
        #[cfg(target_family = "wasm")]
        certificate_digest: String,
    },
    /// Use [`WebTransport`](https://wicg.github.io/web-transport/) as a transport layer
    #[cfg(all(feature = "webtransport", not(target_family = "wasm")))]
    WebTransportServer {
        server_addr: SocketAddr,
        /// Certificate that will be used for authentication
        certificate: Identity,
    },
    /// Use [`WebSocket`](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket) as a transport
    #[cfg(feature = "websocket")]
    WebSocketClient { server_addr: SocketAddr },
    /// Use [`WebSocket`](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket) as a transport
    #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
    WebSocketServer { server_addr: SocketAddr },
    /// Use a crossbeam_channel as a transport. This is useful for testing.
    /// This is server-only: each tuple corresponds to a different client.
    Channels {
        channels: Vec<(SocketAddr, Receiver<Vec<u8>>, Sender<Vec<u8>>)>,
    },
    /// Use a crossbeam_channel as a transport. This is useful for testing.
    /// This is mostly for clients.
    LocalChannel {
        recv: Receiver<Vec<u8>>,
        send: Sender<Vec<u8>>,
    },
    /// Dummy transport if the connection handles its own io (for example steam sockets)
    Dummy,
}

/// We provide a manual implementation because wtranport's `Identity` does not implement Clone
impl ::core::clone::Clone for TransportConfig {
    #[inline]
    fn clone(&self) -> TransportConfig {
        match self {
            #[cfg(not(target_family = "wasm"))]
            TransportConfig::UdpSocket(__self_0) => {
                TransportConfig::UdpSocket(::core::clone::Clone::clone(__self_0))
            }
            #[cfg(feature = "webtransport")]
            TransportConfig::WebTransportClient {
                client_addr: __self_0,
                server_addr: __self_1,
                #[cfg(target_family = "wasm")]
                    certificate_digest: __self_2,
            } => TransportConfig::WebTransportClient {
                client_addr: ::core::clone::Clone::clone(__self_0),
                server_addr: ::core::clone::Clone::clone(__self_1),
                #[cfg(target_family = "wasm")]
                certificate_digest: ::core::clone::Clone::clone(__self_2),
            },
            #[cfg(all(feature = "webtransport", not(target_family = "wasm")))]
            TransportConfig::WebTransportServer {
                server_addr: __self_0,
                certificate: __self_1,
            } => TransportConfig::WebTransportServer {
                server_addr: ::core::clone::Clone::clone(__self_0),
                certificate: __self_1.clone_identity(),
            },
            #[cfg(feature = "websocket")]
            TransportConfig::WebSocketClient {
                server_addr: __self_0,
            } => TransportConfig::WebSocketClient {
                server_addr: ::core::clone::Clone::clone(__self_0),
            },
            #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
            TransportConfig::WebSocketServer {
                server_addr: __self_0,
            } => TransportConfig::WebSocketServer {
                server_addr: ::core::clone::Clone::clone(__self_0),
            },
            TransportConfig::Channels { channels: __self_0 } => TransportConfig::Channels {
                channels: ::core::clone::Clone::clone(__self_0),
            },
            TransportConfig::LocalChannel {
                recv: __self_0,
                send: __self_1,
            } => TransportConfig::LocalChannel {
                recv: ::core::clone::Clone::clone(__self_0),
                send: ::core::clone::Clone::clone(__self_1),
            },
            TransportConfig::Dummy => TransportConfig::Dummy,
        }
    }
}

impl TransportConfig {
    fn build(self) -> TransportBuilderEnum {
        match self {
            #[cfg(not(target_family = "wasm"))]
            TransportConfig::UdpSocket(addr) => {
                TransportBuilderEnum::UdpSocket(UdpSocketBuilder { local_addr: addr })
            }
            #[cfg(all(feature = "webtransport", not(target_family = "wasm")))]
            TransportConfig::WebTransportClient {
                client_addr,
                server_addr,
            } => TransportBuilderEnum::WebTransportClient(WebTransportClientSocketBuilder {
                client_addr,
                server_addr,
            }),
            #[cfg(all(feature = "webtransport", target_family = "wasm"))]
            TransportConfig::WebTransportClient {
                client_addr,
                server_addr,
                certificate_digest,
            } => TransportBuilderEnum::WebTransportClient(WebTransportClientSocketBuilder {
                client_addr,
                server_addr,
                certificate_digest,
            }),
            #[cfg(all(feature = "webtransport", not(target_family = "wasm")))]
            TransportConfig::WebTransportServer {
                server_addr,
                certificate,
            } => TransportBuilderEnum::WebTransportServer(WebTransportServerSocketBuilder {
                server_addr,
                certificate,
            }),
            #[cfg(feature = "websocket")]
            TransportConfig::WebSocketClient { server_addr } => {
                TransportBuilderEnum::WebSocketClient(WebSocketClientSocketBuilder { server_addr })
            }
            #[cfg(all(feature = "websocket", not(target_family = "wasm")))]
            TransportConfig::WebSocketServer { server_addr } => {
                TransportBuilderEnum::WebSocketServer(WebSocketServerSocketBuilder { server_addr })
            }
            TransportConfig::Channels { channels } => {
                TransportBuilderEnum::Channels(Channels::new(channels))
            }
            TransportConfig::LocalChannel { recv, send } => {
                TransportBuilderEnum::LocalChannel(LocalChannelBuilder { recv, send })
            }
            TransportConfig::Dummy => TransportBuilderEnum::Dummy(DummyIo),
        }
    }
}

// TODO: derive Debug directly on TransportConfig once the new version of wtransport is out
impl Debug for TransportConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

#[derive(Clone, Debug, Reflect)]
#[reflect(from_reflect = false)]
pub struct IoConfig {
    #[reflect(ignore)]
    pub transport: TransportConfig,
    pub conditioner: Option<LinkConditionerConfig>,
    pub compression: CompressionConfig,
}

impl Default for IoConfig {
    #[cfg(not(target_family = "wasm"))]
    fn default() -> Self {
        Self {
            transport: TransportConfig::UdpSocket(SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0)),
            conditioner: None,
            compression: CompressionConfig::default(),
        }
    }

    #[cfg(target_family = "wasm")]
    fn default() -> Self {
        let (send, recv) = crossbeam_channel::unbounded();
        Self {
            transport: TransportConfig::LocalChannel { recv, send },
            conditioner: None,
            compression: CompressionConfig::default(),
        }
    }
}

impl IoConfig {
    pub fn from_transport(transport: TransportConfig) -> Self {
        Self {
            transport,
            conditioner: None,
            compression: CompressionConfig::default(),
        }
    }
    pub fn with_conditioner(mut self, conditioner_config: LinkConditionerConfig) -> Self {
        self.conditioner = Some(conditioner_config);
        self
    }

    pub fn with_compression(mut self, compression_config: CompressionConfig) -> Self {
        self.compression = compression_config;
        self
    }

    pub fn connect(self) -> Result<Io> {
        let (transport, state) = self.transport.build().connect()?;
        let local_addr = transport.local_addr();
        #[allow(unused_mut)]
        let (mut sender, receiver, close_fn) = transport.split();
        #[allow(unused_mut)]
        let mut receiver: BoxedReceiver = if let Some(conditioner_config) = self.conditioner {
            let conditioner = LinkConditioner::new(conditioner_config);
            Box::new(conditioner.wrap(receiver))
        } else {
            Box::new(receiver)
        };
        match self.compression {
            CompressionConfig::None => {}
            #[cfg(feature = "zstd")]
            CompressionConfig::Zstd { level } => {
                let compressor = ZstdCompressor::new(level);
                sender = Box::new(compressor.wrap(sender));
                let decompressor = ZstdDecompressor::new();
                receiver = Box::new(decompressor.wrap(receiver));
            }
        }
        Ok(Io {
            local_addr,
            sender,
            receiver,
            close_fn,
            state,
            stats: IoStats::default(),
        })
    }
}
