pub mod client_entity_map;
pub(super) mod despawn_buffer;
pub mod event;
pub(super) mod removal_buffer;
pub(super) mod replication_messages;
mod replication_read_world;
pub mod server_tick;

use std::{ops::Range, time::Duration};

use bevy::{
    ecs::{component::StorageType, system::SystemChangeTick},
    prelude::*,
    ptr::Ptr,
    time::common_conditions::on_timer,
};
use bytes::Buf;

use crate::core::{
    channels::{ReplicationChannel, RepliconChannels},
    common_conditions::{server_just_stopped, server_running},
    connected_clients::ConnectedClients,
    event::server_event::BufferedServerEvents,
    postcard_utils,
    replication::{
        replicated_clients::{
            client_visibility::Visibility, ClientBuffers, ReplicatedClients, VisibilityPolicy,
        },
        replication_registry::{
            component_fns::ComponentFns, ctx::SerializeCtx, rule_fns::UntypedRuleFns,
            ReplicationRegistry,
        },
        track_mutate_messages::TrackMutateMessages,
    },
    replicon_server::RepliconServer,
    replicon_tick::RepliconTick,
    ClientId, DisconnectReason,
};
use client_entity_map::ClientEntityMap;
use despawn_buffer::{DespawnBuffer, DespawnBufferPlugin};
use removal_buffer::{RemovalBuffer, RemovalBufferPlugin};
use replication_messages::{serialized_data::SerializedData, ReplicationMessages};
use replication_read_world::{ReplicatedComponent, ReplicationReadWorld};
use server_tick::ServerTick;

pub struct ServerPlugin {
    /// Tick configuration.
    pub tick_policy: TickPolicy,

    /// Visibility configuration.
    pub visibility_policy: VisibilityPolicy,

    /// The time after which mutations will be considered lost if an acknowledgment is not received for them.
    ///
    /// In practice mutations will live at least `mutations_timeout`, and at most `2*mutations_timeout`.
    pub mutations_timeout: Duration,

    /// If enabled, replication will be started automatically after connection.
    ///
    /// If disabled, replication should be started manually by sending the [`StartReplication`] event.
    /// Until replication has started, the client and server can still exchange network events.
    ///
    /// All events from server will be buffered on client until replication starts, except the ones marked as independent.
    /// See also [`ServerEventAppExt::make_independent`](crate::core::event::server_event::ServerEventAppExt::make_independent).
    pub replicate_after_connect: bool,
}

impl Default for ServerPlugin {
    fn default() -> Self {
        Self {
            tick_policy: TickPolicy::MaxTickRate(30),
            visibility_policy: Default::default(),
            mutations_timeout: Duration::from_secs(10),
            replicate_after_connect: true,
        }
    }
}

/// Server functionality and replication sending.
///
/// Can be disabled for client-only apps.
impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((DespawnBufferPlugin, RemovalBufferPlugin))
            .init_resource::<RepliconServer>()
            .init_resource::<ServerTick>()
            .init_resource::<ClientBuffers>()
            .init_resource::<ClientEntityMap>()
            .init_resource::<ConnectedClients>()
            .insert_resource(ReplicatedClients::new(
                self.visibility_policy,
                self.replicate_after_connect,
            ))
            .init_resource::<BufferedServerEvents>()
            .configure_sets(
                PreUpdate,
                (
                    ServerSet::ReceivePackets,
                    ServerSet::TriggerConnectionEvents,
                    ServerSet::Receive,
                )
                    .chain(),
            )
            .configure_sets(
                PostUpdate,
                (
                    ServerSet::StoreHierarchy,
                    ServerSet::Send,
                    ServerSet::SendPackets,
                )
                    .chain(),
            )
            .add_observer(handle_connects)
            .add_observer(handle_disconnects)
            .add_observer(enable_replication)
            .add_systems(Startup, setup_channels)
            .add_systems(
                PreUpdate,
                (
                    receive_acks,
                    cleanup_acks(self.mutations_timeout).run_if(on_timer(self.mutations_timeout)),
                )
                    .chain()
                    .in_set(ServerSet::Receive)
                    .run_if(server_running),
            )
            .add_systems(
                PostUpdate,
                (
                    send_replication
                        .map(Result::unwrap)
                        .in_set(ServerSet::Send)
                        .run_if(server_running)
                        .run_if(resource_changed::<ServerTick>),
                    reset.run_if(server_just_stopped),
                ),
            );

        match self.tick_policy {
            TickPolicy::MaxTickRate(max_tick_rate) => {
                let tick_time = Duration::from_millis(1000 / max_tick_rate as u64);
                app.add_systems(
                    PostUpdate,
                    increment_tick
                        .before(send_replication)
                        .run_if(server_running)
                        .run_if(on_timer(tick_time)),
                );
            }
            TickPolicy::EveryFrame => {
                app.add_systems(
                    PostUpdate,
                    increment_tick
                        .before(send_replication)
                        .run_if(server_running),
                );
            }
            TickPolicy::Manual => (),
        }
    }
}

fn setup_channels(mut server: ResMut<RepliconServer>, channels: Res<RepliconChannels>) {
    server.setup_client_channels(channels.client_channels().len());
}

/// Increments current server tick which causes the server to replicate this frame.
pub fn increment_tick(mut server_tick: ResMut<ServerTick>) {
    server_tick.increment();
    trace!("incremented {server_tick:?}");
}

fn handle_connects(
    trigger: Trigger<ClientConnected>,
    mut connected_clients: ResMut<ConnectedClients>,
    mut replicated_clients: ResMut<ReplicatedClients>,
    mut buffered_events: ResMut<BufferedServerEvents>,
    mut client_buffers: ResMut<ClientBuffers>,
) {
    debug!("`{:?}` connected", trigger.client_id);
    connected_clients.add(trigger.client_id);
    if replicated_clients.replicate_after_connect() {
        replicated_clients.add(&mut client_buffers, trigger.client_id);
    }
    buffered_events.exclude_client(trigger.client_id);
}

fn handle_disconnects(
    trigger: Trigger<ClientDisconnected>,
    mut entity_map: ResMut<ClientEntityMap>,
    mut connected_clients: ResMut<ConnectedClients>,
    mut replicated_clients: ResMut<ReplicatedClients>,
    mut server: ResMut<RepliconServer>,
    mut client_buffers: ResMut<ClientBuffers>,
) {
    debug!("`{:?}` disconnected: {}", trigger.client_id, trigger.reason);
    entity_map.0.remove(&trigger.client_id);
    connected_clients.remove(trigger.client_id);
    replicated_clients.remove(&mut client_buffers, trigger.client_id);
    server.remove_client(trigger.client_id);
}

fn enable_replication(
    trigger: Trigger<StartReplication>,
    mut replicated_clients: ResMut<ReplicatedClients>,
    mut client_buffers: ResMut<ClientBuffers>,
) {
    replicated_clients.add(&mut client_buffers, **trigger);
}

fn cleanup_acks(
    mutations_timeout: Duration,
) -> impl FnMut(ResMut<ReplicatedClients>, ResMut<ClientBuffers>, Res<Time>) {
    move |mut replicated_clients: ResMut<ReplicatedClients>,
          mut client_buffers: ResMut<ClientBuffers>,
          time: Res<Time>| {
        let min_timestamp = time.elapsed().saturating_sub(mutations_timeout);
        for client in replicated_clients.iter_mut() {
            client.cleanup_older_mutations(&mut client_buffers, min_timestamp);
        }
    }
}

fn receive_acks(
    change_tick: SystemChangeTick,
    mut server: ResMut<RepliconServer>,
    mut replicated_clients: ResMut<ReplicatedClients>,
    mut client_buffers: ResMut<ClientBuffers>,
) {
    for (client_id, mut message) in server.receive(ReplicationChannel::Updates) {
        while message.has_remaining() {
            match postcard_utils::from_buf(&mut message) {
                Ok(mutate_index) => {
                    let client = replicated_clients.client_mut(client_id);
                    client.ack_mutate_message(
                        &mut client_buffers,
                        change_tick.this_run(),
                        mutate_index,
                    );
                }
                Err(e) => debug!("unable to deserialize mutate index from {client_id:?}: {e}"),
            }
        }
    }
}

/// Collects [`ReplicationMessages`] and sends them.
pub(super) fn send_replication(
    mut serialized: Local<SerializedData>,
    mut messages: Local<ReplicationMessages>,
    change_tick: SystemChangeTick,
    world: ReplicationReadWorld,
    mut replicated_clients: ResMut<ReplicatedClients>,
    mut removal_buffer: ResMut<RemovalBuffer>,
    mut client_buffers: ResMut<ClientBuffers>,
    mut entity_map: ResMut<ClientEntityMap>,
    mut despawn_buffer: ResMut<DespawnBuffer>,
    mut server: ResMut<RepliconServer>,
    track_mutate_messages: Res<TrackMutateMessages>,
    registry: Res<ReplicationRegistry>,
    server_tick: Res<ServerTick>,
    time: Res<Time>,
) -> postcard::Result<()> {
    messages.reset(replicated_clients.len());

    collect_mappings(
        &mut messages,
        &mut serialized,
        &replicated_clients,
        &mut entity_map,
    )?;
    collect_despawns(
        &mut messages,
        &mut serialized,
        &mut replicated_clients,
        &mut despawn_buffer,
    )?;
    collect_removals(
        &mut messages,
        &mut serialized,
        &replicated_clients,
        &removal_buffer,
    )?;
    collect_changes(
        &mut messages,
        &mut serialized,
        &mut replicated_clients,
        &registry,
        &removal_buffer,
        &world,
        &change_tick,
        **server_tick,
    )?;
    removal_buffer.clear();

    send_messages(
        &mut messages,
        &mut replicated_clients,
        &mut server,
        **server_tick,
        **track_mutate_messages,
        &mut serialized,
        &mut client_buffers,
        change_tick,
        &time,
    )?;
    serialized.clear();

    Ok(())
}

fn reset(
    mut server_tick: ResMut<ServerTick>,
    mut entity_map: ResMut<ClientEntityMap>,
    mut replicated_clients: ResMut<ReplicatedClients>,
    mut client_buffers: ResMut<ClientBuffers>,
    mut buffered_events: ResMut<BufferedServerEvents>,
) {
    *server_tick = Default::default();
    entity_map.0.clear();
    replicated_clients.clear(&mut client_buffers);
    buffered_events.clear();
}

fn send_messages(
    messages: &mut ReplicationMessages,
    replicated_clients: &mut ReplicatedClients,
    server: &mut RepliconServer,
    server_tick: RepliconTick,
    track_mutate_messages: bool,
    serialized: &mut SerializedData,
    client_buffers: &mut ClientBuffers,
    change_tick: SystemChangeTick,
    time: &Time,
) -> postcard::Result<()> {
    let mut server_tick_range = None;
    for ((update_message, mutate_message), client) in
        messages.iter_mut().zip(replicated_clients.iter_mut())
    {
        if !update_message.is_empty() {
            client.set_update_tick(server_tick);
            let server_tick = write_tick_cached(&mut server_tick_range, serialized, server_tick)?;

            trace!("sending update message to {:?}", client.id());
            update_message.send(server, client, serialized, server_tick)?;
        } else {
            trace!("no updates to send for {:?}", client.id());
        }

        if !mutate_message.is_empty() || track_mutate_messages {
            let server_tick = write_tick_cached(&mut server_tick_range, serialized, server_tick)?;

            let messages_count = mutate_message.send(
                server,
                client,
                client_buffers,
                serialized,
                track_mutate_messages,
                server_tick,
                change_tick.this_run(),
                time.elapsed(),
            )?;
            trace!(
                "sending {messages_count} mutate message(s) to {:?}",
                client.id()
            );
        } else {
            trace!("no mutations to send for {:?}", client.id());
        }

        client.visibility_mut().update();
    }

    Ok(())
}

/// Collects and writes any new entity mappings that happened in this tick.
fn collect_mappings(
    messages: &mut ReplicationMessages,
    serialized: &mut SerializedData,
    replicated_clients: &ReplicatedClients,
    entity_map: &mut ClientEntityMap,
) -> postcard::Result<()> {
    for ((message, _), client) in messages.iter_mut().zip(replicated_clients.iter()) {
        if let Some(mappings) = entity_map.0.get_mut(&client.id()) {
            let len = mappings.len();
            let mappings = serialized.write_mappings(mappings.drain(..))?;
            message.set_mappings(mappings, len);
        }
    }

    Ok(())
}

/// Collect entity despawns from this tick into update messages.
fn collect_despawns(
    messages: &mut ReplicationMessages,
    serialized: &mut SerializedData,
    replicated_clients: &mut ReplicatedClients,
    despawn_buffer: &mut DespawnBuffer,
) -> postcard::Result<()> {
    for entity in despawn_buffer.drain(..) {
        let entity_range = serialized.write_entity(entity)?;
        for ((message, _), client) in messages.iter_mut().zip(replicated_clients.iter_mut()) {
            if client.visibility().is_visible(entity) {
                message.add_despawn(entity_range.clone());
            }
            client.remove_despawned(entity);
        }
    }

    for ((message, _), client) in messages.iter_mut().zip(replicated_clients.iter_mut()) {
        for entity in client.drain_lost_visibility() {
            let entity_range = serialized.write_entity(entity)?;
            message.add_despawn(entity_range);
        }
    }

    Ok(())
}

/// Collects component removals from this tick into update messages.
fn collect_removals(
    messages: &mut ReplicationMessages,
    serialized: &mut SerializedData,
    replicated_clients: &ReplicatedClients,
    removal_buffer: &RemovalBuffer,
) -> postcard::Result<()> {
    for (&entity, remove_ids) in removal_buffer.iter() {
        let entity_range = serialized.write_entity(entity)?;
        let ids_len = remove_ids.len();
        let fn_ids = serialized.write_fn_ids(remove_ids.iter().map(|&(_, fns_id)| fns_id))?;
        for ((message, _), client) in messages.iter_mut().zip(replicated_clients.iter()) {
            if client.visibility().is_visible(entity) {
                message.add_removals(entity_range.clone(), ids_len, fn_ids.clone());
            }
        }
    }

    Ok(())
}

/// Collects component changes from this tick into update and mutate messages since the last entity tick.
fn collect_changes(
    messages: &mut ReplicationMessages,
    serialized: &mut SerializedData,
    replicated_clients: &mut ReplicatedClients,
    registry: &ReplicationRegistry,
    removal_buffer: &RemovalBuffer,
    world: &ReplicationReadWorld,
    change_tick: &SystemChangeTick,
    server_tick: RepliconTick,
) -> postcard::Result<()> {
    for (archetype, replicated_archetype) in world.iter_archetypes() {
        for entity in archetype.entities() {
            let mut entity_range = None;
            for ((update_message, mutate_message), client) in
                messages.iter_mut().zip(replicated_clients.iter())
            {
                let visibility = client.visibility().state(entity.id());
                update_message.start_entity_changes(visibility);
                mutate_message.start_entity_mutations();
            }

            // SAFETY: all replicated archetypes have marker component with table storage.
            let (_, marker_ticks) = unsafe {
                world.get_component_unchecked(
                    entity,
                    archetype.table_id(),
                    StorageType::Table,
                    world.marker_id(),
                )
            };
            // If the marker was added in this tick, the entity just started replicating.
            // It could be a newly spawned entity or an old entity with just-enabled replication,
            // so we need to include even old components that were registered for replication.
            let marker_added =
                marker_ticks.is_added(change_tick.last_run(), change_tick.this_run());

            for replicated_component in &replicated_archetype.components {
                let (component_id, component_fns, rule_fns) =
                    registry.get(replicated_component.fns_id);

                // SAFETY: component and storage were obtained from this archetype.
                let (component, ticks) = unsafe {
                    world.get_component_unchecked(
                        entity,
                        archetype.table_id(),
                        replicated_component.storage_type,
                        component_id,
                    )
                };

                let ctx = SerializeCtx {
                    server_tick,
                    component_id,
                };
                let mut component_range = None;
                for ((update_message, mutate_message), client) in
                    messages.iter_mut().zip(replicated_clients.iter())
                {
                    if update_message.entity_visibility() == Visibility::Hidden {
                        continue;
                    }

                    if let Some(tick) = client
                        .mutation_tick(entity.id())
                        .filter(|_| !marker_added)
                        .filter(|_| update_message.entity_visibility() != Visibility::Gained)
                        .filter(|_| !ticks.is_added(change_tick.last_run(), change_tick.this_run()))
                    {
                        if ticks.is_changed(tick, change_tick.this_run()) {
                            if !mutate_message.mutations_written() {
                                let entity_range = write_entity_cached(
                                    &mut entity_range,
                                    serialized,
                                    entity.id(),
                                )?;
                                mutate_message.add_mutated_entity(entity.id(), entity_range);
                            }
                            let component_range = write_component_cached(
                                &mut component_range,
                                serialized,
                                rule_fns,
                                component_fns,
                                &ctx,
                                replicated_component,
                                component,
                            )?;
                            mutate_message.add_mutated_component(component_range);
                        }
                    } else {
                        if !update_message.entity_written() {
                            let entity_range =
                                write_entity_cached(&mut entity_range, serialized, entity.id())?;
                            update_message.add_changed_entity(entity_range);
                        }
                        let component_range = write_component_cached(
                            &mut component_range,
                            serialized,
                            rule_fns,
                            component_fns,
                            &ctx,
                            replicated_component,
                            component,
                        )?;
                        update_message.add_inserted_component(component_range);
                    }
                }
            }

            for ((update_message, mutate_message), client) in
                messages.iter_mut().zip(replicated_clients.iter_mut())
            {
                let visibility = update_message.entity_visibility();
                if visibility == Visibility::Hidden {
                    continue;
                }

                let new_entity = marker_added || visibility == Visibility::Gained;
                if new_entity
                    || update_message.entity_written()
                    || removal_buffer.contains_key(&entity.id())
                {
                    // If there is any insertion, removal, or it's a new entity for a client, include all mutations
                    // into update message and bump the last acknowledged tick to keep entity updates atomic.
                    update_message.take_mutations(mutate_message);
                    client.set_mutation_tick(entity.id(), change_tick.this_run());
                }

                if new_entity && !update_message.entity_written() {
                    // Force-write new entity even if it doesn't have any components.
                    let entity_range =
                        write_entity_cached(&mut entity_range, serialized, entity.id())?;
                    update_message.add_changed_entity(entity_range);
                }
            }
        }
    }

    Ok(())
}

/// Writes an entity or re-uses previously written range if exists.
fn write_entity_cached(
    entity_range: &mut Option<Range<usize>>,
    serialized: &mut SerializedData,
    entity: Entity,
) -> postcard::Result<Range<usize>> {
    if let Some(range) = entity_range.clone() {
        return Ok(range);
    }

    let range = serialized.write_entity(entity)?;
    *entity_range = Some(range.clone());

    Ok(range)
}

/// Writes a component or re-uses previously written range if exists.
fn write_component_cached(
    component_range: &mut Option<Range<usize>>,
    serialized: &mut SerializedData,
    rule_fns: &UntypedRuleFns,
    component_fns: &ComponentFns,
    ctx: &SerializeCtx,
    replicated_component: &ReplicatedComponent,
    component: Ptr<'_>,
) -> postcard::Result<Range<usize>> {
    if let Some(component_range) = component_range.clone() {
        return Ok(component_range);
    }

    let range = serialized.write_component(
        rule_fns,
        component_fns,
        ctx,
        replicated_component.fns_id,
        component,
    )?;
    *component_range = Some(range.clone());

    Ok(range)
}

/// Writes an entity or re-uses previously written range if exists.
fn write_tick_cached(
    tick_range: &mut Option<Range<usize>>,
    serialized: &mut SerializedData,
    tick: RepliconTick,
) -> postcard::Result<Range<usize>> {
    if let Some(range) = tick_range.clone() {
        return Ok(range);
    }

    let range = serialized.write_tick(tick)?;
    *tick_range = Some(range.clone());

    Ok(range)
}

/// Set with replication and event systems related to server.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum ServerSet {
    /// Systems that receive packets from the messaging backend.
    ///
    /// Used by the messaging backend.
    ///
    /// Runs in [`PreUpdate`].
    ReceivePackets,
    /// Systems that trigger [`ClientConnected`] and [`ClientDisconnected`].
    ///
    /// The messaging backend should convert its own server events in this set.
    ///
    /// Needed only for backends that use batched events instead of triggers.
    ///
    /// Runs in [`PreUpdate`].
    TriggerConnectionEvents,
    /// Systems that receive data from [`RepliconServer`].
    ///
    /// Used by `bevy_replicon`.
    ///
    /// Runs in [`PreUpdate`].
    Receive,
    /// Systems that store hierarchy changes in [`ParentSync`](super::parent_sync::ParentSync).
    ///
    /// Runs in [`PostUpdate`].
    StoreHierarchy,
    /// Systems that send data to [`RepliconServer`].
    ///
    /// Used by `bevy_replicon`.
    ///
    /// Runs in [`PostUpdate`] on server tick, see [`TickPolicy`].
    Send,
    /// Systems that send packets to the messaging backend.
    ///
    /// Used by the messaging backend.
    ///
    /// Runs in [`PostUpdate`] on server tick, see [`TickPolicy`].
    SendPackets,
}

/// Controls how often [`RepliconTick`] is incremented on the server.
///
/// When [`RepliconTick`] is mutated, the server's replication
/// system will run. This means the tick policy controls how often server state is replicated.
///
/// Note that component mutations are replicated over the unreliable channel, so if a component mutate message is lost
/// then component mutations won't be resent until the server's replication system runs again.
#[derive(Debug, Copy, Clone)]
pub enum TickPolicy {
    /// The replicon tick is incremented at most max ticks per second. In practice the tick rate may be lower if the
    /// app's update cycle duration is too long.
    ///
    /// By default it's 30 ticks per second.
    MaxTickRate(u16),
    /// The replicon tick is incremented every frame.
    EveryFrame,
    /// The user should manually configure [`increment_tick`] or manually increment
    /// [`RepliconTick`].
    Manual,
}

/// Triggered on connection on the server.
///
/// The messaging backend is responsible for triggering.
///
/// See also [`ClientDisconnected`] and [`Trigger`].
#[derive(Event, Debug, Clone, Copy)]
pub struct ClientConnected {
    pub client_id: ClientId,
}

/// Triggered on disconnection on the server.
///
/// The messaging backend is responsible for triggering.
///
/// See also [`ClientConnected`] and [`Trigger`].
#[derive(Event, Debug)]
pub struct ClientDisconnected {
    pub client_id: ClientId,
    pub reason: DisconnectReason,
}

/// Triggers replication for a connected client.
///
/// This event needs to be triggered manually if [`ServerPlugin::replicate_after_connect`] is set to `false`.
///
/// See also [`Trigger`].
#[derive(Debug, Clone, Copy, Event, Deref)]
pub struct StartReplication(pub ClientId);
