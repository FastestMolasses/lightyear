//! Defines the client bevy systems and run conditions
use std::ops::DerefMut;

use bevy::ecs::system::{RunSystemOnce, SystemChangeTick, SystemState};
use bevy::prelude::ResMut;
use bevy::prelude::*;
use tracing::{error, trace};

use crate::_reexport::{ClientMarker, ReplicationSend};
use crate::client::config::ClientConfig;
use crate::client::connection::ConnectionManager;
use crate::client::events::{ConnectEvent, DisconnectEvent, EntityDespawnEvent, EntitySpawnEvent};
use crate::client::sync::SyncSet;
use crate::connection::client::{ClientConnection, NetClient};
use crate::prelude::{SharedConfig, TickManager, TimeManager};
use crate::protocol::component::ComponentProtocol;
use crate::protocol::message::MessageProtocol;
use crate::protocol::Protocol;
use crate::shared::config::Mode;
use crate::shared::events::connection::{IterEntityDespawnEvent, IterEntitySpawnEvent};
use crate::shared::sets::InternalMainSet;
use crate::shared::tick_manager::TickEvent;
use crate::shared::time_manager::is_client_ready_to_send;

pub(crate) struct ClientNetworkingPlugin<P: Protocol> {
    marker: std::marker::PhantomData<P>,
}

impl<P: Protocol> Default for ClientNetworkingPlugin<P> {
    fn default() -> Self {
        Self {
            marker: std::marker::PhantomData,
        }
    }
}

impl<P: Protocol> Plugin for ClientNetworkingPlugin<P> {
    fn build(&self, app: &mut App) {
        let config = app.world.resource::<ClientConfig>();
        // in host server mode, we don't send/receive packets at all
        if config.shared.mode == Mode::HostServer {
            // we just update the connection/disconnection events
            app.add_systems(
                PreUpdate,
                update_connect_events::<P>.in_set(InternalMainSet::<ClientMarker>::Receive),
            );
            return;
        }

        app
            // SYSTEM SETS
            .configure_sets(
                PreUpdate,
                InternalMainSet::<ClientMarker>::Receive.run_if(is_io_connected),
            )
            .configure_sets(
                PostUpdate,
                // run sync before send because some send systems need to know if the client is synced
                // we don't send packets every frame, but on a timer instead
                (
                    SyncSet,
                    InternalMainSet::<ClientMarker>::Send.run_if(is_client_ready_to_send),
                )
                    .run_if(is_io_connected)
                    .chain(),
            )
            // SYSTEMS
            .add_systems(
                PreUpdate,
                receive::<P>.in_set(InternalMainSet::<ClientMarker>::Receive),
            )
            .add_systems(
                PostUpdate,
                send::<P>.in_set(InternalMainSet::<ClientMarker>::SendPackets),
            );

        // TODO: update virtual time with Time<Real> so we have more accurate time at Send time.
        app.add_systems(
            PostUpdate,
            sync_update::<P>.in_set(SyncSet).run_if(is_client_connected),
        );
    }
}

pub(crate) fn is_io_connected(netcode: Res<ClientConnection>) -> bool {
    netcode.io().map_or(false, |io| io.is_connected())
}

/// In host server mode, we don't send/receive packets at all
///
/// Just update the Connect/Disconnect events for both client and server
pub(crate) fn update_connect_events<P: Protocol>(
    netcode: Res<ClientConnection>,
    mut connection: ResMut<ConnectionManager<P>>,
    // client connect events
    mut connect_event_writer: EventWriter<ConnectEvent>,
    mut disconnect_event_writer: EventWriter<DisconnectEvent>,
    // server connect events
    mut server_connect_event_writer: EventWriter<crate::server::events::ConnectEvent>,
    mut server_disconnect_event_writer: EventWriter<crate::server::events::DisconnectEvent>,
) {
    if netcode.is_connected() {
        // push an event indicating that we just connected
        if !connection.is_connected {
            debug!("Client connected event");
            connect_event_writer.send(ConnectEvent::new(netcode.id()));
            server_connect_event_writer
                .send(crate::server::events::ConnectEvent::new(netcode.id()));
            connection.is_connected = true;
        }
    } else {
        // push an event indicating that we just disconnected
        if connection.is_connected {
            debug!("Client disconnected event");
            disconnect_event_writer.send(DisconnectEvent::new(()));
            server_disconnect_event_writer
                .send(crate::server::events::DisconnectEvent::new(netcode.id()));
            connection.is_connected = false;
        }
    }
}

pub(crate) fn receive<P: Protocol>(world: &mut World) {
    trace!("Receive server packets");
    // TODO: here we can control time elapsed from the client's perspective?

    // TODO: THE CLIENT COULD DO PHYSICS UPDATES INSIDE FIXED-UPDATE SYSTEMS
    //  WE SHOULD BE CALLING UPDATE INSIDE THOSE AS WELL SO THAT WE CAN SEND UPDATES
    //  IN THE MIDDLE OF THE FIXED UPDATE LOOPS
    //  WE JUST KEEP AN INTERNAL TIMER TO KNOW IF WE REACHED OUR TICK AND SHOULD RECEIVE/SEND OUT PACKETS?
    //  FIXED-UPDATE.expend() updates the clock zR the fixed update interval
    //  THE NETWORK TICK INTERVAL COULD BE IN BETWEEN FIXED UPDATE INTERVALS
    world.resource_scope(
        |world: &mut World, mut connection: Mut<ConnectionManager<P>>| {
            world.resource_scope(
                |world: &mut World, mut netcode: Mut<ClientConnection>| {
                        world.resource_scope(
                            |world: &mut World, mut time_manager: Mut<TimeManager>| {
                                world.resource_scope(
                                    |world: &mut World, tick_manager: Mut<TickManager>| {
                                        let delta = world.resource::<Time<Virtual>>().delta();

                                        // UPDATE: update client state, send keep-alives, receive packets from io, update connection sync state
                                        time_manager.update(delta);
                                        trace!(time = ?time_manager.current_time(), tick = ?tick_manager.tick(), "receive");
                                        let _ = netcode
                                            .try_update(delta.as_secs_f64())
                                            .map_err(|e| {
                                                error!("Error updating netcode: {}", e);
                                            });

                                        // only start the connection (sending messages, sending pings, starting sync, etc.)
                                        // once we are connected
                                        if netcode.is_connected() {
                                            // push an event indicating that we just connected
                                            if !connection.is_connected {
                                                let mut connect_event_writer =
                                                    world.get_resource_mut::<Events<ConnectEvent>>().unwrap();
                                                debug!("Client connected event");
                                                connect_event_writer.send(ConnectEvent::new(netcode.id()));
                                                connection.is_connected = true;
                                            }
                                            // TODO: handle disconnection event
                                            connection.update(
                                                time_manager.as_ref(),
                                                tick_manager.as_ref(),
                                            );
                                        } else {
                                            // push an event indicating that we just disconnected
                                            if connection.is_connected {
                                                let mut disconnect_event_writer =
                                                    world.get_resource_mut::<Events<DisconnectEvent>>().unwrap();
                                                debug!("Client disconnected event");
                                                disconnect_event_writer.send(DisconnectEvent::new(()));
                                                connection.is_connected = false;
                                            }
                                        }

                                        // RECV PACKETS: buffer packets into message managers
                                        while let Some(packet) = netcode.recv() {
                                            connection
                                                .recv_packet(packet, tick_manager.as_ref())
                                                .unwrap();
                                        }

                                        // RECEIVE: receive packets from message managers
                                        let mut events = connection.receive(
                                            world,
                                            time_manager.as_ref(),
                                            tick_manager.as_ref(),
                                        );

                                        // TODO: run these in EventsPlugin!
                                        // HANDLE EVENTS
                                        if !events.is_empty() {
                                            // Message Events
                                            P::Message::push_message_events(world, &mut events);

                                            // SpawnEntity event
                                            if events.has_entity_spawn() {
                                                let mut entity_spawn_event_writer = world
                                                    .get_resource_mut::<Events<EntitySpawnEvent>>()
                                                    .unwrap();
                                                for (entity, _) in events.into_iter_entity_spawn() {
                                                    entity_spawn_event_writer
                                                        .send(EntitySpawnEvent::new(entity, ()));
                                                }
                                            }
                                            // DespawnEntity event
                                            if events.has_entity_despawn() {
                                                let mut entity_despawn_event_writer = world
                                                    .get_resource_mut::<Events<EntityDespawnEvent>>()
                                                    .unwrap();
                                                for (entity, _) in events.into_iter_entity_despawn()
                                                {
                                                    entity_despawn_event_writer
                                                        .send(EntityDespawnEvent::new(entity, ()));
                                                }
                                            }

                                            // Update component events (updates, inserts, removes)
                                            P::Components::push_component_events(
                                                world,
                                                &mut events,
                                            );
                                        }
                                    },
                                )
                            }
                    );
                }
            );
            trace!("finished recv");
        }
    );
}

pub(crate) fn send<P: Protocol>(
    mut netcode: ResMut<ClientConnection>,
    system_change_tick: SystemChangeTick,
    tick_manager: Res<TickManager>,
    time_manager: Res<TimeManager>,
    mut connection: ResMut<ConnectionManager<P>>,
) {
    trace!("Send packets to server");
    // finalize any packets that are needed for replication
    connection
        .buffer_replication_messages(tick_manager.tick(), system_change_tick.this_run())
        .unwrap_or_else(|e| {
            error!("Error preparing replicate send: {}", e);
        });
    // SEND_PACKETS: send buffered packets to io
    let packet_bytes = connection
        .send_packets(time_manager.as_ref(), tick_manager.as_ref())
        .unwrap();
    for packet_byte in packet_bytes {
        let _ = netcode.send(packet_byte.as_slice()).map_err(|e| {
            error!("Error sending packet: {}", e);
        });
    }

    // no need to clear the connection, because we already std::mem::take it
    // client.connection.clear();
}

/// Update the sync manager.
/// We run this at PostUpdate because:
/// - client prediction time is computed from ticks, which haven't been updated yet at PreUpdate
/// - server prediction time is computed from time, which has been updated via delta
/// Also server sends the tick after FixedUpdate, so it makes sense that we would compare to the client tick after FixedUpdate
/// So instead we update the sync manager at PostUpdate, after both ticks/time have been updated
pub(crate) fn sync_update<P: Protocol>(
    config: Res<ClientConfig>,
    netclient: Res<ClientConnection>,
    connection: ResMut<ConnectionManager<P>>,
    mut time_manager: ResMut<TimeManager>,
    mut tick_manager: ResMut<TickManager>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut tick_events: EventWriter<TickEvent>,
) {
    let connection = connection.into_inner();
    // NOTE: this triggers change detection
    // Handle pongs, update RTT estimates, update client prediction time
    if let Some(tick_event) = connection.sync_manager.update(
        time_manager.deref_mut(),
        tick_manager.deref_mut(),
        &connection.ping_manager,
        &config.interpolation.delay,
        config.shared.server_send_interval,
    ) {
        tick_events.send(tick_event);
    }

    if connection.sync_manager.is_synced() {
        if let Some(tick_event) = connection.sync_manager.update_prediction_time(
            time_manager.deref_mut(),
            tick_manager.deref_mut(),
            &connection.ping_manager,
        ) {
            tick_events.send(tick_event);
        }
        let relative_speed = time_manager.get_relative_speed();
        virtual_time.set_relative_speed(relative_speed);
    }
}

/// Run Condition that returns true if the client is connected
pub fn is_client_connected(netclient: Res<ClientConnection>) -> bool {
    netclient.is_connected()
}
