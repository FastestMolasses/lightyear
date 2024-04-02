//! Bevy [`Event`] that are emitted when certain network events occur on the client
//!
//! You can use this to react to network events in your game systems.
//! ```rust,ignore
//! use bevy::ecs::event::EventId;
//! fn handle_message(mut messages: EventReader<MessageEvent<MyMessage>>) {
//!   for event in messages.read() {
//!     // the event has two functions `message()` and `context()`
//!     // `context()` is currently unused but is reserved for future uses (e.g. to get the sender of the message, or the tick it was sent on)
//!     let message = event.message();
//!     // do something with the message
//!   }
//! }
//! ```

use crate::prelude::{ClientId, Protocol};
use crate::shared::events::connection::ConnectionEvents;
use crate::shared::events::plugin::EventsPlugin;
use bevy::app::{App, Plugin, PostUpdate};
use bevy::prelude::{Event, Events};

/// Plugin that handles generating bevy [`Events`] related to networking and replication
pub struct ClientEventsPlugin<P: Protocol> {
    marker: std::marker::PhantomData<P>,
}

impl<P: Protocol> Default for ClientEventsPlugin<P> {
    fn default() -> Self {
        Self {
            marker: std::marker::PhantomData,
        }
    }
}

impl<P: Protocol> Plugin for ClientEventsPlugin<P> {
    fn build(&self, app: &mut App) {
        app
            // EVENTS
            .add_event::<ConnectEvent>()
            // PLUGIN
            // TODO: it's annoying to have to keep that () around...
            //  revisit this.. maybe the into_iter_messages returns directly an object that
            //  can be created from Ctx and Message
            //  For Server it's the MessageEvent<M, ClientId>
            //  For Client it's MessageEvent<M> directly
            .add_plugins(EventsPlugin::<P, ()>::default());
    }
}

/// Bevy [`Event`] emitted on the client on the frame where the connection is established
///
/// We keep this separate from the server's ConnectEvent so that we have different events emitted on the client
/// and the server when running in HostServer mode
#[derive(Event)]
pub struct ConnectEvent(ClientId);

impl ConnectEvent {
    pub fn new(client_id: ClientId) -> Self {
        Self(client_id)
    }
    pub fn client_id(&self) -> ClientId {
        self.0
    }
}

/// Bevy [`Event`] emitted on the client on the frame where the connection is disconnected
pub type DisconnectEvent = crate::shared::events::components::DisconnectEvent<()>;
/// Bevy [`Event`] emitted on the client to indicate the user input for the tick
pub type InputEvent<I> = crate::shared::events::components::InputEvent<I, ()>;
/// Bevy [`Event`] emitted on the client when a EntitySpawn replication message is received
pub type EntitySpawnEvent = crate::shared::events::components::EntitySpawnEvent<()>;
/// Bevy [`Event`] emitted on the client when a EntityDespawn replication message is received
pub type EntityDespawnEvent = crate::shared::events::components::EntityDespawnEvent<()>;
/// Bevy [`Event`] emitted on the client when a ComponentUpdate replication message is received
pub type ComponentUpdateEvent<C> = crate::shared::events::components::ComponentUpdateEvent<C, ()>;
/// Bevy [`Event`] emitted on the client when a ComponentInsert replication message is received
pub type ComponentInsertEvent<C> = crate::shared::events::components::ComponentInsertEvent<C, ()>;
/// Bevy [`Event`] emitted on the client when a ComponentRemove replication message is received
pub type ComponentRemoveEvent<C> = crate::shared::events::components::ComponentRemoveEvent<C, ()>;
/// Bevy [`Event`] emitted on the client when a (non-replication) message is received
pub type MessageEvent<M> = crate::shared::events::components::MessageEvent<M, ()>;
