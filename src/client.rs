pub mod confirm_history;
#[cfg(feature = "client_diagnostics")]
pub mod diagnostics;
pub mod event;
pub mod server_mutate_ticks;

use bevy::{ecs::world::CommandQueue, prelude::*};
use bytes::{Buf, Bytes};
use postcard::experimental::max_size::MaxSize;

use crate::core::{
    channels::{ReplicationChannel, RepliconChannels},
    common_conditions::{client_connected, client_just_connected, client_just_disconnected},
    entity_serde, postcard_utils,
    replication::{
        command_markers::{CommandMarkers, EntityMarkers},
        deferred_entity::DeferredEntity,
        mutate_index::MutateIndex,
        replication_registry::{
            ctx::{DespawnCtx, RemoveCtx, WriteCtx},
            ReplicationRegistry,
        },
        track_mutate_messages::TrackMutateMessages,
        update_message_flags::UpdateMessageFlags,
        Replicated,
    },
    replicon_client::RepliconClient,
    replicon_tick::RepliconTick,
    server_entity_map::ServerEntityMap,
};
use confirm_history::{ConfirmHistory, EntityReplicated};
use server_mutate_ticks::{MutateTickReceived, ServerMutateTicks};

/// Client functionality and replication receiving.
///
/// Can be disabled for server-only apps.
pub struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RepliconClient>()
            .init_resource::<ServerEntityMap>()
            .init_resource::<ServerUpdateTick>()
            .init_resource::<BufferedMutations>()
            .add_event::<EntityReplicated>()
            .add_event::<MutateTickReceived>()
            .configure_sets(
                PreUpdate,
                (
                    ClientSet::ReceivePackets,
                    (
                        ClientSet::ResetEvents.run_if(client_just_connected),
                        ClientSet::Reset.run_if(client_just_disconnected),
                    ),
                    ClientSet::Receive,
                    (ClientSet::Diagnostics, ClientSet::SyncHierarchy),
                )
                    .chain(),
            )
            .configure_sets(
                PostUpdate,
                (ClientSet::Send, ClientSet::SendPackets).chain(),
            )
            .add_systems(Startup, setup_channels)
            .add_systems(
                PreUpdate,
                receive_replication
                    .map(Result::unwrap)
                    .in_set(ClientSet::Receive)
                    .run_if(client_connected),
            )
            .add_systems(PreUpdate, reset.in_set(ClientSet::Reset));
    }

    fn finish(&self, app: &mut App) {
        if **app.world().resource::<TrackMutateMessages>() {
            app.init_resource::<ServerMutateTicks>();
        }
    }
}

fn setup_channels(mut client: ResMut<RepliconClient>, channels: Res<RepliconChannels>) {
    client.setup_server_channels(channels.server_channels().len());
}

/// Receives and applies replication messages from the server.
///
/// Update messages are sent over the [`ReplicationChannel::Updates`] and are applied first to ensure valid state
/// for component mutations.
///
/// Mutate messages are sent over [`ReplicationChannel::Mutations`], which means they may appear
/// ahead-of or behind update messages from the same server tick. A mutation will only be applied if its
/// update tick has already appeared in an update message, otherwise it will be buffered while waiting.
/// Since component mutations can arrive in any order, they will only be applied if they correspond to a more
/// recent server tick than the last acked server tick for each entity.
///
/// Buffered mutate messages are processed last.
///
/// Acknowledgments for received mutate messages are sent back to the server.
///
/// See also [`ReplicationMessages`](crate::server::replication_messages::ReplicationMessages).
pub(super) fn receive_replication(
    world: &mut World,
    mut queue: Local<CommandQueue>,
    mut entity_markers: Local<EntityMarkers>,
) -> postcard::Result<()> {
    world.resource_scope(|world, mut client: Mut<RepliconClient>| {
        world.resource_scope(|world, mut entity_map: Mut<ServerEntityMap>| {
            world.resource_scope(|world, mut buffered_mutations: Mut<BufferedMutations>| {
                world.resource_scope(|world, command_markers: Mut<CommandMarkers>| {
                    world.resource_scope(|world, registry: Mut<ReplicationRegistry>| {
                        world.resource_scope(
                            |world, mut replicated_events: Mut<Events<EntityReplicated>>| {
                                let mut stats = world.remove_resource::<ClientReplicationStats>();
                                let mut mutate_ticks = world.remove_resource::<ServerMutateTicks>();
                                let mut params = ReceiveParams {
                                    queue: &mut queue,
                                    entity_markers: &mut entity_markers,
                                    entity_map: &mut entity_map,
                                    replicated_events: &mut replicated_events,
                                    mutate_ticks: mutate_ticks.as_mut(),
                                    stats: stats.as_mut(),
                                    command_markers: &command_markers,
                                    registry: &registry,
                                };

                                apply_replication(
                                    world,
                                    &mut params,
                                    &mut client,
                                    &mut buffered_mutations,
                                )?;

                                if let Some(stats) = stats {
                                    world.insert_resource(stats);
                                }
                                if let Some(mutate_ticks) = mutate_ticks {
                                    world.insert_resource(mutate_ticks);
                                }

                                Ok(())
                            },
                        )
                    })
                })
            })
        })
    })
}

fn reset(
    mut update_tick: ResMut<ServerUpdateTick>,
    mut entity_map: ResMut<ServerEntityMap>,
    mut buffered_mutations: ResMut<BufferedMutations>,
    stats: Option<ResMut<ClientReplicationStats>>,
) {
    *update_tick = Default::default();
    entity_map.clear();
    buffered_mutations.clear();
    if let Some(mut stats) = stats {
        *stats = Default::default();
    }
}

/// Reads all received messages and applies them.
///
/// Sends acknowledgments for mutate messages back.
fn apply_replication(
    world: &mut World,
    params: &mut ReceiveParams,
    client: &mut RepliconClient,
    buffered_mutations: &mut BufferedMutations,
) -> postcard::Result<()> {
    for mut message in client.receive(ReplicationChannel::Updates) {
        apply_update_message(world, params, &mut message)?;
    }

    // Unlike update messages, we read all mutate messages first, sort them by tick
    // in descending order to ensure that the last mutation will be applied first.
    // Since mutate messages manually split by packet size, we apply all messages,
    // but skip outdated data per-entity by checking last received tick for it
    // (unless user requested history via marker).
    let update_tick = *world.resource::<ServerUpdateTick>();
    let acks_size =
        MutateIndex::POSTCARD_MAX_SIZE * client.received_count(ReplicationChannel::Mutations);
    if acks_size != 0 {
        let mut acks = Vec::with_capacity(acks_size);
        for message in client.receive(ReplicationChannel::Mutations) {
            let mutate_index = buffer_mutate_message(params, buffered_mutations, message)?;
            postcard_utils::to_extend_mut(&mutate_index, &mut acks)?;
        }
        client.send(ReplicationChannel::Updates, acks);
    }

    apply_mutate_messages(world, params, buffered_mutations, update_tick)
}

/// Reads and applies an update message.
///
/// For details see [`replication_messages`](crate::server::replication_messages).
fn apply_update_message(
    world: &mut World,
    params: &mut ReceiveParams,
    message: &mut Bytes,
) -> postcard::Result<()> {
    if let Some(stats) = &mut params.stats {
        stats.messages += 1;
        stats.bytes += message.len();
    }

    let flags: UpdateMessageFlags = postcard_utils::from_buf(message)?;
    debug_assert!(!flags.is_empty(), "message can't be empty");

    let message_tick = postcard_utils::from_buf(message)?;
    trace!("applying update message for {message_tick:?}");
    world.resource_mut::<ServerUpdateTick>().0 = message_tick;

    let last_flag = flags.last();
    for (_, flag) in flags.iter_names() {
        let array_kind = if flag != last_flag {
            ArrayKind::Sized
        } else {
            ArrayKind::Dynamic
        };

        match flag {
            UpdateMessageFlags::MAPPINGS => {
                debug_assert_eq!(array_kind, ArrayKind::Sized);
                let len = apply_array(array_kind, message, |message| {
                    apply_entity_mapping(world, params, message)
                })?;
                if let Some(stats) = &mut params.stats {
                    stats.mappings += len;
                }
            }
            UpdateMessageFlags::DESPAWNS => {
                let len = apply_array(array_kind, message, |message| {
                    apply_despawn(world, params, message, message_tick)
                })?;
                if let Some(stats) = &mut params.stats {
                    stats.despawns += len;
                }
            }
            UpdateMessageFlags::REMOVALS => {
                let len = apply_array(array_kind, message, |message| {
                    apply_removals(world, params, message, message_tick)
                })?;
                if let Some(stats) = &mut params.stats {
                    stats.entities_changed += len;
                }
            }
            UpdateMessageFlags::CHANGES => {
                debug_assert_eq!(array_kind, ArrayKind::Dynamic);
                let len = apply_array(array_kind, message, |message| {
                    apply_changes(world, params, message, message_tick)
                })?;
                if let Some(stats) = &mut params.stats {
                    stats.entities_changed += len;
                }
            }
            _ => unreachable!("iteration should yield only named flags"),
        }
    }

    Ok(())
}

/// Reads and buffers mutate message.
///
/// For details see [`replication_messages`](crate::server::replication_messages).
///
/// Returns mutate index to be used for acknowledgment.
fn buffer_mutate_message(
    params: &mut ReceiveParams,
    buffered_mutations: &mut BufferedMutations,
    mut message: Bytes,
) -> postcard::Result<MutateIndex> {
    if let Some(stats) = &mut params.stats {
        stats.messages += 1;
        stats.bytes += message.len();
    }

    let update_tick = postcard_utils::from_buf(&mut message)?;
    let message_tick = postcard_utils::from_buf(&mut message)?;
    let messages_count = if params.mutate_ticks.is_some() {
        postcard_utils::from_buf(&mut message)?
    } else {
        1
    };
    let mutate_index = postcard_utils::from_buf(&mut message)?;
    trace!("received mutate message for {message_tick:?}");
    buffered_mutations.insert(BufferedMutate {
        update_tick,
        message_tick,
        messages_count,
        message,
    });

    Ok(mutate_index)
}

/// Applies mutations from [`BufferedMutations`].
///
/// If the mutate message can't be applied yet (because the update message with the
/// corresponding tick hasn't arrived), it will be kept in the buffer.
fn apply_mutate_messages(
    world: &mut World,
    params: &mut ReceiveParams,
    buffered_mutations: &mut BufferedMutations,
    update_tick: ServerUpdateTick,
) -> postcard::Result<()> {
    let mut result = Ok(());
    buffered_mutations.0.retain_mut(|mutate| {
        if mutate.update_tick > *update_tick {
            return true;
        }

        trace!("applying mutate message for {:?}", mutate.message_tick);
        let len = apply_array(ArrayKind::Dynamic, &mut mutate.message, |message| {
            apply_mutations(world, params, message, mutate.message_tick)
        });

        match len {
            Ok(len) => {
                if let Some(stats) = &mut params.stats {
                    stats.entities_changed += len;
                }
            }
            Err(e) => result = Err(e),
        }

        if let Some(mutate_ticks) = &mut params.mutate_ticks {
            if mutate_ticks.confirm(mutate.message_tick, mutate.messages_count) {
                world.send_event(MutateTickReceived {
                    tick: mutate.message_tick,
                });
            }
        }

        false
    });

    result
}

/// Deserializes and applies server mapping from client's pre-spawned entities.
fn apply_entity_mapping(
    world: &mut World,
    params: &mut ReceiveParams,
    message: &mut Bytes,
) -> postcard::Result<()> {
    let server_entity = entity_serde::deserialize_entity(message)?;
    let client_entity = entity_serde::deserialize_entity(message)?;

    if let Ok(mut entity) = world.get_entity_mut(client_entity) {
        debug!("received mapping from {server_entity:?} to {client_entity:?}");
        entity.insert(Replicated);
        params.entity_map.insert(server_entity, client_entity);
    } else {
        // Entity could be despawned on client already.
        debug!("received mapping from {server_entity:?} to {client_entity:?}, but the entity doesn't exists");
    }

    Ok(())
}

/// Deserializes and applies entity despawn from update message.
fn apply_despawn(
    world: &mut World,
    params: &mut ReceiveParams,
    message: &mut Bytes,
    message_tick: RepliconTick,
) -> postcard::Result<()> {
    // The entity might have already been despawned because of hierarchy or
    // with the last replication message, but the server might not yet have received confirmation
    // from the client and could include the deletion in the this message.
    let server_entity = entity_serde::deserialize_entity(message)?;
    if let Some(client_entity) = params
        .entity_map
        .remove_by_server(server_entity)
        .and_then(|entity| world.get_entity_mut(entity).ok())
    {
        let ctx = DespawnCtx { message_tick };
        (params.registry.despawn)(&ctx, client_entity);
    }

    Ok(())
}

/// Deserializes and applies component removals for an entity.
fn apply_removals(
    world: &mut World,
    params: &mut ReceiveParams,
    message: &mut Bytes,
    message_tick: RepliconTick,
) -> postcard::Result<()> {
    let server_entity = entity_serde::deserialize_entity(message)?;

    let client_entity = params
        .entity_map
        .get_by_server_or_insert(server_entity, || world.spawn(Replicated).id());

    let mut client_entity = DeferredEntity::new(world, client_entity);
    let mut commands = client_entity.commands(params.queue);
    params
        .entity_markers
        .read(params.command_markers, &*client_entity);

    confirm_tick(
        &mut commands,
        &mut client_entity,
        params.replicated_events,
        message_tick,
    );

    let len = apply_array(ArrayKind::Sized, message, |message| {
        let fns_id = postcard_utils::from_buf(message)?;
        let (component_id, component_fns, _) = params.registry.get(fns_id);
        let mut ctx = RemoveCtx {
            commands: &mut commands,
            message_tick,
            component_id,
        };
        component_fns.remove(&mut ctx, params.entity_markers, &mut client_entity);

        Ok(())
    })?;

    if let Some(stats) = &mut params.stats {
        stats.components_changed += len;
    }

    params.queue.apply(world);

    Ok(())
}

/// Deserializes and applies component insertions and/or mutations for an entity.
fn apply_changes(
    world: &mut World,
    params: &mut ReceiveParams,
    message: &mut Bytes,
    message_tick: RepliconTick,
) -> postcard::Result<()> {
    let server_entity = entity_serde::deserialize_entity(message)?;

    let client_entity = params
        .entity_map
        .get_by_server_or_insert(server_entity, || world.spawn(Replicated).id());

    let mut client_entity = DeferredEntity::new(world, client_entity);
    let mut commands = client_entity.commands(params.queue);
    params
        .entity_markers
        .read(params.command_markers, &*client_entity);

    confirm_tick(
        &mut commands,
        &mut client_entity,
        params.replicated_events,
        message_tick,
    );

    let len = apply_array(ArrayKind::Sized, message, |message| {
        let fns_id = postcard_utils::from_buf(message)?;
        let (component_id, component_fns, rule_fns) = params.registry.get(fns_id);
        let mut ctx = WriteCtx::new(&mut commands, params.entity_map, component_id, message_tick);

        // SAFETY: `rule_fns` and `component_fns` were created for the same type.
        unsafe {
            component_fns.write(
                &mut ctx,
                rule_fns,
                params.entity_markers,
                &mut client_entity,
                message,
            )?;
        }

        Ok(())
    })?;

    if let Some(stats) = &mut params.stats {
        stats.components_changed += len;
    }

    params.queue.apply(world);

    Ok(())
}

fn apply_array(
    kind: ArrayKind,
    message: &mut Bytes,
    mut f: impl FnMut(&mut Bytes) -> postcard::Result<()>,
) -> postcard::Result<usize> {
    match kind {
        ArrayKind::Sized => {
            let len = postcard_utils::from_buf(message)?;
            for _ in 0..len {
                (f)(message)?;
            }

            Ok(len)
        }
        ArrayKind::Dynamic => {
            let mut len = 0;
            while message.has_remaining() {
                (f)(message)?;
                len += 1;
            }

            Ok(len)
        }
    }
}

/// Type of serialized array.
#[derive(PartialEq, Eq, Debug)]
enum ArrayKind {
    /// Size is serialized before the array.
    Sized,
    /// Size is unknown, means that all bytes needs to be consumed.
    Dynamic,
}

fn confirm_tick(
    commands: &mut Commands,
    entity: &mut DeferredEntity,
    replicated_events: &mut Events<EntityReplicated>,
    tick: RepliconTick,
) {
    if let Some(mut history) = entity.get_mut::<ConfirmHistory>() {
        history.set_last_tick(tick);
    } else {
        commands
            .entity(entity.id())
            .insert(ConfirmHistory::new(tick));
    }
    replicated_events.send(EntityReplicated {
        entity: entity.id(),
        tick,
    });
}

/// Deserializes and applies component mutations for all entities.
///
/// Consumes all remaining bytes in the given message.
fn apply_mutations(
    world: &mut World,
    params: &mut ReceiveParams,
    message: &mut Bytes,
    message_tick: RepliconTick,
) -> postcard::Result<()> {
    let server_entity = entity_serde::deserialize_entity(message)?;
    let data_size: usize = postcard_utils::from_buf(message)?;

    let Some(client_entity) = params.entity_map.get_by_server(server_entity) else {
        // Mutation could arrive after a despawn from update message.
        debug!("ignoring mutations received for unknown server's {server_entity:?}");
        message.advance(data_size);
        return Ok(());
    };

    let mut client_entity = DeferredEntity::new(world, client_entity);
    let mut commands = client_entity.commands(params.queue);
    params
        .entity_markers
        .read(params.command_markers, &*client_entity);

    let Some(mut history) = client_entity.get_mut::<ConfirmHistory>() else {
        error!(
            "ignoring mutations that arrived earlier that insertion for {:?}",
            client_entity.id()
        );
        commands.entity(client_entity.id()).log_components();
        cursor.set_position(cursor.position() + data_size as u64);
        return Ok(());
    };
    let new_tick = message_tick > history.last_tick();
    if new_tick {
        history.set_last_tick(message_tick);
    } else {
        if !params.entity_markers.need_history() {
            trace!(
                "ignoring outdated mutations for client's {:?}",
                client_entity.id()
            );
            message.advance(data_size);
            return Ok(());
        }

        let ago = history.last_tick().get().wrapping_sub(message_tick.get());
        if ago >= u64::BITS {
            trace!(
                "discarding {ago} ticks old mutations for client's {:?}",
                client_entity.id()
            );
            message.advance(data_size);
            return Ok(());
        }

        history.set(ago);
    }
    params.replicated_events.send(EntityReplicated {
        entity: client_entity.id(),
        tick: message_tick,
    });

    let mut data = message.split_to(data_size);
    let mut components_count = 0;
    while data.has_remaining() {
        let fns_id = postcard_utils::from_buf(&mut data)?;
        let (component_id, component_fns, rule_fns) = params.registry.get(fns_id);
        let mut ctx = WriteCtx::new(&mut commands, params.entity_map, component_id, message_tick);

        // SAFETY: `rule_fns` and `component_fns` were created for the same type.
        unsafe {
            if new_tick {
                component_fns.write(
                    &mut ctx,
                    rule_fns,
                    params.entity_markers,
                    &mut client_entity,
                    &mut data,
                )?;
            } else {
                component_fns.consume_or_write(
                    &mut ctx,
                    rule_fns,
                    params.entity_markers,
                    params.command_markers,
                    &mut client_entity,
                    &mut data,
                )?;
            }
        }

        components_count += 1;
    }

    if let Some(stats) = &mut params.stats {
        stats.components_changed += components_count;
    }

    params.queue.apply(world);

    Ok(())
}

/// Borrowed resources from the world and locals.
///
/// To avoid passing a lot of arguments into all receive functions.
struct ReceiveParams<'a> {
    queue: &'a mut CommandQueue,
    entity_markers: &'a mut EntityMarkers,
    entity_map: &'a mut ServerEntityMap,
    replicated_events: &'a mut Events<EntityReplicated>,
    mutate_ticks: Option<&'a mut ServerMutateTicks>,
    stats: Option<&'a mut ClientReplicationStats>,
    command_markers: &'a CommandMarkers,
    registry: &'a ReplicationRegistry,
}

/// Set with replication and event systems related to client.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum ClientSet {
    /// Systems that receive packets from the messaging backend.
    ///
    /// Used by messaging backend implementations.
    ///
    /// Runs in [`PreUpdate`].
    ReceivePackets,
    /// Systems that receive data from [`RepliconClient`].
    ///
    /// Used by `bevy_replicon`.
    ///
    /// Runs in [`PreUpdate`].
    Receive,
    /// Systems that populate Bevy's [`Diagnostics`](bevy::diagnostic::Diagnostics).
    ///
    /// Used by `bevy_replicon`.
    ///
    /// Runs in [`PreUpdate`].
    Diagnostics,
    /// Systems that synchronize hierarchy changes in [`ParentSync`](super::parent_sync::ParentSync).
    ///
    /// Used by `bevy_replicon`.
    ///
    /// Runs in [`PreUpdate`].
    SyncHierarchy,
    /// Systems that send data to [`RepliconClient`].
    ///
    /// Used by `bevy_replicon`.
    ///
    /// Runs in [`PostUpdate`].
    Send,
    /// Systems that send packets to the messaging backend.
    ///
    /// Used by messaging backend implementations.
    ///
    /// Runs in [`PostUpdate`].
    SendPackets,
    /// Systems that reset queued server events.
    ///
    /// Runs in [`PreUpdate`] immediately after the client connects to ensure client sessions have a fresh start.
    ///
    /// This is a separate set from [`ClientSet::Reset`] because the reset requirements for events are different
    /// from the replicon client internals.
    /// It is best practice to discard client-sent and server-received events while the client is not connected
    /// in order to guarantee clean separation between connection sessions.
    ResetEvents,
    /// Systems that reset the client.
    ///
    /// Runs in [`PreUpdate`] when the client just disconnected.
    ///
    /// You may want to disable this set if you want to preserve client replication state across reconnects.
    /// In that case, you need to manually repair the client state (or use something like
    /// [`bevy_replicon_repair`](https://docs.rs/bevy_replicon_repair)).
    ///
    /// If this set is disabled and you don't want to repair client state, then you need to manually clean up
    /// the client after a disconnect or when reconnecting.
    Reset,
}

/// Last received tick for update messages from the server.
///
/// In other words, the last [`RepliconTick`] with a removal, insertion, spawn or despawn.
/// This value is not updated when mutation messages are received from the server.
///
/// See also [`ServerMutateTicks`].
#[derive(Clone, Copy, Debug, Default, Deref, Resource)]
pub struct ServerUpdateTick(RepliconTick);

/// Cached buffered mutate messages, used to synchronize mutations with update messages.
///
/// If [`ClientSet::Reset`] is disabled, then this needs to be cleaned up manually with [`Self::clear`].
#[derive(Default, Resource)]
pub struct BufferedMutations(Vec<BufferedMutate>);

impl BufferedMutations {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Inserts a new buffered message, maintaining sorting by their message tick in descending order.
    fn insert(&mut self, mutation: BufferedMutate) {
        let index = self
            .0
            .partition_point(|other_mutation| mutation.message_tick < other_mutation.message_tick);
        self.0.insert(index, mutation);
    }
}

/// Partially-deserialized mutate message that is waiting for its tick to appear in an update message.
///
/// See also [`crate::server::replication_messages`].
pub(super) struct BufferedMutate {
    /// Required tick to wait for.
    update_tick: RepliconTick,

    /// The tick this mutations corresponds to.
    message_tick: RepliconTick,

    /// Total number of mutate messages sent by the server for this tick.
    ///
    /// May not be equal to the number of received messages.
    messages_count: usize,

    /// Mutations data.
    message: Bytes,
}

/// Replication stats during message processing.
///
/// Statistic will be collected only if the resource is present.
/// The resource is not added by default.
///
/// See also [`ClientDiagnosticsPlugin`](diagnostics::ClientDiagnosticsPlugin)
/// for automatic integration with Bevy diagnostics.
#[derive(Clone, Copy, Default, Resource, Debug)]
pub struct ClientReplicationStats {
    /// Incremented per entity that changes.
    pub entities_changed: usize,
    /// Incremented for every component that changes.
    pub components_changed: usize,
    /// Incremented per client mapping added.
    pub mappings: usize,
    /// Incremented per entity despawn.
    pub despawns: usize,
    /// Replication messages received.
    pub messages: usize,
    /// Replication bytes received in message payloads (without internal messaging plugin data).
    pub bytes: usize,
}
