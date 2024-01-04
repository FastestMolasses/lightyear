//! General struct handling replication
use std::collections::BTreeMap;
use std::iter::Extend;

use anyhow::Context;
use bevy::ecs::component::Tick as BevyTick;
use bevy::prelude::{Entity, World};
use bevy::utils::petgraph::data::ElementIterator;
use bevy::utils::{EntityHashMap, EntityHashSet, HashMap, HashSet};
use crossbeam_channel::Receiver;
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_subscriber::filter::FilterExt;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::_reexport::{EntityActionsChannel, EntityUpdatesChannel};
use crate::connection::events::ConnectionEvents;
use crate::packet::message::MessageId;
use crate::prelude::client::Confirmed;
use crate::prelude::{MapEntities, Tick};
use crate::protocol::channel::ChannelKind;
use crate::protocol::component::ComponentProtocol;
use crate::protocol::component::{ComponentBehaviour, ComponentKindBehaviour};
use crate::protocol::Protocol;
use crate::shared::replication::components::ReplicationGroupId;

use super::entity_map::RemoteEntityMap;
use super::{
    EntityActionMessage, EntityActions, EntityUpdatesMessage, ReplicationMessage,
    ReplicationMessageData,
};

// TODO: maybe separate send/receive side for clarity?
pub(crate) struct ReplicationReceiver<P: Protocol> {
    /// Map between local and remote entities. (used mostly on client because it's when we receive entity updates)
    pub remote_entity_map: RemoteEntityMap,

    /// Map from remote entity to the replication group-id
    pub remote_entity_to_group: EntityHashMap<Entity, ReplicationGroupId>,

    // BOTH
    /// Buffer to so that we have an ordered receiver per group
    pub group_channels: EntityHashMap<ReplicationGroupId, GroupChannel<P>>,
}

impl<P: Protocol> ReplicationReceiver<P> {
    pub(crate) fn new() -> Self {
        Self {
            // RECEIVE
            remote_entity_map: RemoteEntityMap::default(),
            remote_entity_to_group: Default::default(),
            // BOTH
            group_channels: Default::default(),
        }
    }

    /// Recv a new replication message and buffer it
    pub(crate) fn recv_message(
        &mut self,
        message: ReplicationMessage<P::Components, P::ComponentKinds>,
        remote_tick: Tick,
    ) {
        trace!(?message, ?remote_tick, "Received replication message");
        let channel = self.group_channels.entry(message.group_id).or_default();
        match message.data {
            ReplicationMessageData::Actions(m) => {
                // if the message is too old, ignore it
                if m.sequence_id < channel.actions_pending_recv_message_id {
                    return;
                }

                // add the message to the buffer
                // TODO: I guess this handles potential duplicates?
                channel
                    .actions_recv_message_buffer
                    .insert(m.sequence_id, (remote_tick, m));
            }
            ReplicationMessageData::Updates(m) => {
                // if we have already applied a more recent update for this group, no need to keep this one
                if remote_tick <= channel.latest_tick {
                    return;
                }

                // TODO: include somewhere in the update message the m.last_ack_tick since when we compute changes?
                //  (if we want to do diff compression?
                // otherwise buffer the update
                channel
                    .buffered_updates
                    .entry(m.last_action_tick)
                    .or_default()
                    .entry(remote_tick)
                    .or_insert(m);
            }
        }
        trace!(?channel, "group channel after buffering");
    }

    /// Return the list of replication messages that are ready to be applied to the World
    /// Also include the server_tick when that replication message was emitted
    ///
    /// Updates the `latest_tick` for this group
    pub(crate) fn read_messages(
        &mut self,
    ) -> Vec<(
        ReplicationGroupId,
        Vec<(
            Tick,
            ReplicationMessageData<P::Components, P::ComponentKinds>,
        )>,
    )> {
        self.group_channels
            .iter_mut()
            .filter_map(|(group_id, channel)| {
                channel
                    .read_messages()
                    .map(|messages| (*group_id, messages))
            })
            .collect()
    }

    /// Gets the tick at which the provided confirmed entity currently is
    /// (i.e. the latest server tick at which we received an update for that entity)
    pub(crate) fn get_confirmed_tick(&self, confirmed_entity: Entity) -> Option<Tick> {
        self.channel_by_local(confirmed_entity)
            .map(|channel| channel.latest_tick)
    }

    // USED BY RECEIVE SIDE (SEND SIZE CAN GET THE GROUP_ID EASILY)
    /// Get the group channel associated with a given entity
    fn channel_by_local(&self, local_entity: Entity) -> Option<&GroupChannel<P>> {
        self.remote_entity_map
            .get_remote(local_entity)
            .and_then(|remote_entity| self.channel_by_remote(*remote_entity))
    }

    // USED BY RECEIVE SIDE (SEND SIZE CAN GET THE GROUP_ID EASILY)
    /// Get the group channel associated with a given entity
    fn channel_by_remote(&self, remote_entity: Entity) -> Option<&GroupChannel<P>> {
        self.remote_entity_to_group
            .get(&remote_entity)
            .and_then(|group_id| self.group_channels.get(group_id))
    }
}

/// We want:
/// - entity actions to be done reliably
/// - entity updates (component updates) to be done unreliably
///
/// - all component inserts/removes/updates for an entity to be grouped together in a single message
impl<P: Protocol> ReplicationReceiver<P> {
    // TODO: how can I emit metrics here that contain the channel kind?
    //  use a OnceCell that gets set with the channel name mapping when the protocol is finalized?
    //  the other option is to have wrappers in Connection, but that's pretty ugly

    /// Apply any replication messages to the world, and emit an event
    /// I think we don't need to emit a tick with the event anymore, because
    /// we can access the tick via the replication manager
    pub(crate) fn apply_world(
        &mut self,
        world: &mut World,
        replication: ReplicationMessageData<P::Components, P::ComponentKinds>,
        group_id: ReplicationGroupId,
        events: &mut ConnectionEvents<P>,
    ) {
        let _span = trace_span!("Apply received replication message to world").entered();
        match replication {
            ReplicationMessageData::Actions(m) => {
                debug!(?m, "Received replication actions");
                // NOTE: order matters here, because some components can depend on other entities.
                // These components could even form a cycle, for example A.HasWeapon(B) and B.HasHolder(A)
                // Our solution is to first handle spawn for all entities separately.
                for (entity, actions) in m.actions.iter() {
                    debug!(remote_entity = ?entity, "Received entity actions");
                    assert!(!(actions.spawn && actions.despawn));
                    // spawn
                    if actions.spawn {
                        self.remote_entity_to_group.insert(*entity, group_id);
                        if let Some(local_entity) = self.remote_entity_map.get_local(*entity) {
                            if world.get_entity(*local_entity).is_some() {
                                warn!("Received spawn for an entity that already exists");
                                continue;
                            }
                            warn!("Received spawn for an entity that is already in our entity mapping! Not spawning");
                            continue;
                        }
                        // TODO: optimization: spawn the bundle of insert components
                        let local_entity = world.spawn_empty();
                        self.remote_entity_map.insert(*entity, local_entity.id());

                        debug!(remote_entity = ?entity, "Received entity spawn");
                        events.push_spawn(local_entity.id());
                    }
                }

                for (entity, actions) in m.actions.into_iter() {
                    debug!(remote_entity = ?entity, "Received entity actions");

                    // despawn
                    if actions.despawn {
                        debug!(remote_entity = ?entity, "Received entity despawn");
                        if let Some(local_entity) = self.remote_entity_map.remove_by_remote(entity)
                        {
                            world.despawn(local_entity);
                            self.remote_entity_to_group.remove(&entity);
                            events.push_despawn(local_entity);
                        } else {
                            error!("Received despawn for an entity that does not exist")
                        }
                        continue;
                    }

                    // safety: we know by this point that the entity exists
                    let Ok(mut local_entity_mut) =
                        self.remote_entity_map.get_by_remote(world, entity)
                    else {
                        error!("cannot find entity");
                        continue;
                    };

                    // inserts
                    let kinds = actions
                        .insert
                        .iter()
                        .map(|c| c.into())
                        .collect::<HashSet<P::ComponentKinds>>();
                    debug!(remote_entity = ?entity, ?kinds, "Received InsertComponent");
                    for mut component in actions.insert {
                        // map any entities inside the component
                        component.map_entities(Box::new(&self.remote_entity_map));
                        // TODO: figure out what to do with tick here
                        events.push_insert_component(
                            local_entity_mut.id(),
                            (&component).into(),
                            Tick(0),
                        );
                        component.insert(&mut local_entity_mut);

                        // TODO: special-case for pre-spawned entities: we receive them from a client, but then we
                        // we should immediately take ownership of it, so we won't receive a despawn for it
                        // thus, we should remove it from the entity map right after receiving it!
                        // Actually, we should figure out a way to cleanup every received entity where the sender
                        // stopped replicating or didn't replicate the Spawn, as this could just cause memory to accumulate
                    }

                    // removals
                    debug!(remote_entity = ?entity, ?actions.remove, "Received RemoveComponent");
                    for kind in actions.remove {
                        events.push_remove_component(local_entity_mut.id(), kind, Tick(0));
                        kind.remove(&mut local_entity_mut);
                    }

                    // (no need to run apply_deferred after applying actions, that is only for Commands)

                    // updates
                    let kinds = actions
                        .updates
                        .iter()
                        .map(|c| c.into())
                        .collect::<Vec<P::ComponentKinds>>();
                    debug!(remote_entity = ?entity, ?kinds, "Received UpdateComponent");
                    for mut component in actions.updates {
                        // map any entities inside the component
                        component.map_entities(Box::new(&self.remote_entity_map));
                        events.push_update_component(
                            local_entity_mut.id(),
                            (&component).into(),
                            Tick(0),
                        );
                        component.update(&mut local_entity_mut);
                    }
                }
            }
            ReplicationMessageData::Updates(m) => {
                for (entity, components) in m.updates.into_iter() {
                    debug!(?components, remote_entity = ?entity, "Received UpdateComponent");
                    // update the entity only if it exists
                    if let Ok(mut local_entity) =
                        self.remote_entity_map.get_by_remote(world, entity)
                    {
                        for component in components {
                            events.push_update_component(
                                local_entity.id(),
                                (&component).into(),
                                Tick(0),
                            );
                            component.update(&mut local_entity);
                        }
                    } else {
                        // we can get a few buffered updates after the entity has been despawned
                        // those are the updates that we received before the despawn action message, but with a tick
                        // later than the despawn action message
                        debug!("update for entity that doesn't exist?");
                    }
                }
            }
        }
    }
}

/// Channel to keep track of receiving/sending replication messages for a given Group
#[derive(Debug)]
pub struct GroupChannel<P: Protocol> {
    // actions
    pub actions_pending_recv_message_id: MessageId,
    pub actions_recv_message_buffer:
        BTreeMap<MessageId, (Tick, EntityActionMessage<P::Components, P::ComponentKinds>)>,
    // updates
    // map from necessary_last_action_tick to the buffered message
    // the first tick is the last_action_tick
    // the second tick is the update's server tick when it was sent
    pub buffered_updates: BTreeMap<Tick, BTreeMap<Tick, EntityUpdatesMessage<P::Components>>>,
    /// remote tick of the latest update/action that we applied to the local group
    pub latest_tick: Tick,
}

impl<P: Protocol> Default for GroupChannel<P> {
    fn default() -> Self {
        Self {
            actions_pending_recv_message_id: MessageId(0),
            actions_recv_message_buffer: BTreeMap::new(),
            buffered_updates: Default::default(),
            latest_tick: Tick(0),
        }
    }
}

impl<P: Protocol> GroupChannel<P> {
    /// Reads a message from the internal buffer to get its content
    /// Since we are receiving messages in order, we don't return from the buffer
    /// until we have received the message we are waiting for (the next expected MessageId)
    /// This assumes that the sender sends all message ids sequentially.
    ///
    /// If had received updates that were waiting on a given action, we also return them
    fn read_action(
        &mut self,
    ) -> Option<(Tick, EntityActionMessage<P::Components, P::ComponentKinds>)> {
        // Check if we have received the message we are waiting for
        let Some(message) = self
            .actions_recv_message_buffer
            .remove(&self.actions_pending_recv_message_id)
        else {
            return None;
        };

        self.actions_pending_recv_message_id += 1;
        // Update the latest server tick that we have processed
        self.latest_tick = message.0;
        Some(message)
    }

    fn read_buffered_updates(&mut self) -> Vec<(Tick, EntityUpdatesMessage<P::Components>)> {
        // go through all the buffered updates whose last_action_tick has been reached
        // (the update's last_action_tick <= latest_tick)
        let not_ready = self.buffered_updates.split_off(&(self.latest_tick + 1));

        let mut res = vec![];
        let buffered_updates_to_consider = std::mem::take(&mut self.buffered_updates);
        for (necessary_action_tick, updates) in buffered_updates_to_consider.into_iter() {
            for (tick, update) in updates {
                // only push the update if the update's tick is more recent than the entity's current latest_tick
                if self.latest_tick < tick {
                    self.latest_tick = tick;
                    res.push((tick, update));
                }
            }
        }
        self.buffered_updates = not_ready;
        res
    }

    fn read_messages(
        &mut self,
    ) -> Option<
        Vec<(
            Tick,
            ReplicationMessageData<P::Components, P::ComponentKinds>,
        )>,
    > {
        let mut res = Vec::new();

        // check for any actions that are ready to be applied
        while let Some((tick, actions)) = self.read_action() {
            res.push((tick, ReplicationMessageData::Actions(actions)));
        }

        // TODO: (IMPORTANT): should we try to get the updates in order of tick?

        // check for any buffered updates that are ready to be applied now that we have applied more actions/updates
        res.extend(
            self.read_buffered_updates()
                .into_iter()
                .map(|(tick, updates)| (tick, ReplicationMessageData::Updates(updates))),
        );

        (!res.is_empty()).then_some(res)
    }
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;

    use crate::tests::protocol::*;

    use super::*;

    #[allow(clippy::get_first)]
    #[test]
    fn test_recv_replication_messages() {
        let mut manager = ReplicationReceiver::<MyProtocol>::new();

        let group_id = ReplicationGroupId(0);
        // recv an actions message that is too old: should be ignored
        manager.recv_message(
            ReplicationMessage {
                group_id,
                data: ReplicationMessageData::Actions(EntityActionMessage {
                    sequence_id: MessageId(0) - 1,
                    actions: Default::default(),
                }),
            },
            Tick(0),
        );
        assert_eq!(
            manager
                .group_channels
                .get(&group_id)
                .unwrap()
                .actions_pending_recv_message_id,
            MessageId(0)
        );
        assert!(manager
            .group_channels
            .get(&group_id)
            .unwrap()
            .actions_recv_message_buffer
            .is_empty());

        // recv an actions message: in order, should be buffered
        manager.recv_message(
            ReplicationMessage {
                group_id: ReplicationGroupId(0),
                data: ReplicationMessageData::Actions(EntityActionMessage {
                    sequence_id: MessageId(0),
                    actions: Default::default(),
                }),
            },
            Tick(0),
        );
        assert!(manager
            .group_channels
            .get(&group_id)
            .unwrap()
            .actions_recv_message_buffer
            .get(&MessageId(0))
            .is_some());

        // add an updates message
        manager.recv_message(
            ReplicationMessage {
                group_id: ReplicationGroupId(0),
                data: ReplicationMessageData::Updates(EntityUpdatesMessage {
                    last_action_tick: Tick(0),
                    updates: Default::default(),
                }),
            },
            Tick(1),
        );
        assert!(manager
            .group_channels
            .get(&group_id)
            .unwrap()
            .buffered_updates
            .get(&Tick(0))
            .unwrap()
            .get(&Tick(1))
            .is_some());

        // add updates before actions (last_action_tick is 2)
        manager.recv_message(
            ReplicationMessage {
                group_id: ReplicationGroupId(0),
                data: ReplicationMessageData::Updates(EntityUpdatesMessage {
                    last_action_tick: Tick(2),
                    updates: Default::default(),
                }),
            },
            Tick(4),
        );
        assert!(manager
            .group_channels
            .get(&group_id)
            .unwrap()
            .buffered_updates
            .get(&Tick(2))
            .unwrap()
            .get(&Tick(4))
            .is_some());

        // read messages: only read the first action and update
        let read_messages = manager.read_messages();
        let replication_data = &read_messages.first().unwrap().1;
        assert_eq!(replication_data.get(0).unwrap().0, Tick(0));
        assert_eq!(replication_data.get(1).unwrap().0, Tick(1));

        // recv actions-3: should be buffered, we are still waiting for actions-2
        manager.recv_message(
            ReplicationMessage {
                group_id: ReplicationGroupId(0),
                data: ReplicationMessageData::Actions(EntityActionMessage {
                    sequence_id: MessageId(2),
                    actions: Default::default(),
                }),
            },
            Tick(3),
        );
        assert!(manager.read_messages().is_empty());

        // recv actions-2: we should now be able to read actions-2, actions-3, updates-4
        manager.recv_message(
            ReplicationMessage {
                group_id: ReplicationGroupId(0),
                data: ReplicationMessageData::Actions(EntityActionMessage {
                    sequence_id: MessageId(1),
                    actions: Default::default(),
                }),
            },
            Tick(2),
        );
        let read_messages = manager.read_messages();
        let replication_data = &read_messages.first().unwrap().1;
        assert_eq!(replication_data.len(), 3);
        assert_eq!(replication_data.get(0).unwrap().0, Tick(2));
        assert_eq!(replication_data.get(1).unwrap().0, Tick(3));
        assert_eq!(replication_data.get(2).unwrap().0, Tick(4));
    }
}
