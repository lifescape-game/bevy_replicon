use std::io::Cursor;

use bevy::{
    ecs::world::EntityMut,
    prelude::*,
    utils::{Entry, HashMap},
};
use bevy_renet::{renet::Bytes, transport::client_connected};
use bevy_renet::{renet::RenetClient, transport::NetcodeClientPlugin, RenetClientPlugin};
use bincode::{DefaultOptions, Options};
use varint_rs::VarintReader;

use crate::replicon_core::{
    replication_rules::{Mapper, Replication, ReplicationRules},
    replicon_tick::RepliconTick,
    REPLICATION_CHANNEL_ID,
};

pub struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((RenetClientPlugin, NetcodeClientPlugin))
            .init_resource::<LastRepliconTick>()
            .init_resource::<NetworkEntityMap>()
            .configure_set(
                PreUpdate,
                ClientSet::Receive.after(NetcodeClientPlugin::update_system),
            )
            .configure_set(
                PostUpdate,
                ClientSet::Send.before(NetcodeClientPlugin::send_packets),
            )
            .add_systems(
                PreUpdate,
                Self::diff_receiving_system
                    .pipe(unwrap)
                    .in_set(ClientSet::Receive)
                    .run_if(client_connected()),
            )
            .add_systems(
                PostUpdate,
                (
                    Self::ack_sending_system
                        .in_set(ClientSet::Send)
                        .run_if(client_connected()),
                    Self::reset_system.run_if(resource_removed::<RenetClient>()),
                ),
            );
    }
}

impl ClientPlugin {
    fn diff_receiving_system(world: &mut World) -> Result<(), bincode::Error> {
        world.resource_scope(|world, mut client: Mut<RenetClient>| {
            world.resource_scope(|world, mut entity_map: Mut<NetworkEntityMap>| {
                world.resource_scope(|world, replication_rules: Mut<ReplicationRules>| {
                    while let Some(message) = client.receive_message(REPLICATION_CHANNEL_ID) {
                        let end_pos: u64 = message.len().try_into().unwrap();
                        let mut cursor = Cursor::new(message);

                        let Some(tick) = deserialize_tick(&mut cursor, world)? else {
                            continue;
                        };
                        if cursor.position() == end_pos {
                            continue;
                        }

                        deserialize_component_diffs(
                            &mut cursor,
                            world,
                            &mut entity_map,
                            &replication_rules,
                            DiffKind::Change,
                            tick,
                        )?;
                        if cursor.position() == end_pos {
                            continue;
                        }

                        deserialize_component_diffs(
                            &mut cursor,
                            world,
                            &mut entity_map,
                            &replication_rules,
                            DiffKind::Removal,
                            tick,
                        )?;
                        if cursor.position() == end_pos {
                            continue;
                        }

                        deserialize_despawns(
                            &mut cursor,
                            world,
                            &mut entity_map,
                            &replication_rules,
                            tick,
                        )?;
                    }

                    Ok(())
                })
            })
        })
    }

    fn ack_sending_system(last_tick: Res<LastRepliconTick>, mut client: ResMut<RenetClient>) {
        let message = bincode::serialize(&last_tick.0)
            .unwrap_or_else(|e| panic!("client ack should be serialized: {e}"));
        client.send_message(REPLICATION_CHANNEL_ID, message);
    }

    fn reset_system(
        mut last_tick: ResMut<LastRepliconTick>,
        mut entity_map: ResMut<NetworkEntityMap>,
    ) {
        last_tick.0 = Default::default();
        entity_map.clear();
    }
}

/// Deserializes server tick and applies it to [`LastTick`] if it is newer.
///
/// Returns the tick if [`LastTick`] has been updated.
fn deserialize_tick(
    cursor: &mut Cursor<Bytes>,
    world: &mut World,
) -> Result<Option<RepliconTick>, bincode::Error> {
    let tick = bincode::deserialize_from(cursor)?;

    let mut last_tick = world.resource_mut::<LastRepliconTick>();
    if last_tick.0 < tick {
        last_tick.0 = tick;
        Ok(Some(tick))
    } else {
        Ok(None)
    }
}

/// Deserializes component diffs of `diff_kind` and applies them to the `world`.
fn deserialize_component_diffs(
    cursor: &mut Cursor<Bytes>,
    world: &mut World,
    entity_map: &mut NetworkEntityMap,
    replication_rules: &ReplicationRules,
    diff_kind: DiffKind,
    tick: RepliconTick,
) -> Result<(), bincode::Error> {
    let entities_count: u16 = bincode::deserialize_from(&mut *cursor)?;
    for _ in 0..entities_count {
        let (entity, predicted_entity) = deserialize_entity_combo(&mut *cursor)?;
        let mut entity = entity_map.get_by_server_or_spawn(world, entity, predicted_entity);
        let components_count: u8 = bincode::deserialize_from(&mut *cursor)?;
        for _ in 0..components_count {
            let replication_id = DefaultOptions::new().deserialize_from(&mut *cursor)?;
            // SAFETY: server and client have identical `ReplicationRules` and server always sends valid IDs.
            let replication_info = unsafe { replication_rules.get_info_unchecked(replication_id) };
            match diff_kind {
                DiffKind::Change => {
                    (replication_info.deserialize)(&mut entity, entity_map, cursor, tick)?
                }
                DiffKind::Removal => (replication_info.remove)(&mut entity, tick),
            }
        }
    }

    Ok(())
}

/// Deserializes despawns and applies them to the `world`.
fn deserialize_despawns(
    cursor: &mut Cursor<Bytes>,
    world: &mut World,
    entity_map: &mut NetworkEntityMap,
    replication_rules: &ReplicationRules,
    tick: RepliconTick,
) -> Result<(), bincode::Error> {
    let entities_count: u16 = bincode::deserialize_from(&mut *cursor)?;
    for _ in 0..entities_count {
        // The entity might have already been despawned because of hierarchy or
        // with the last diff, but the server might not yet have received confirmation
        // from the client and could include the deletion in the latest diff.
        let server_entity = deserialize_entity(&mut *cursor)?;
        if let Some(client_entity) = entity_map
            .remove_by_server(server_entity)
            .and_then(|entity| world.get_entity_mut(entity))
        {
            (replication_rules.despawn_fn)(client_entity, tick);
        }
    }

    Ok(())
}

/// Deserializes `entity` from compressed index and generation, for details see [`ReplicationBuffer::write_entity()`].
fn deserialize_entity(cursor: &mut Cursor<Bytes>) -> Result<Entity, bincode::Error> {
    let flagged_index: u64 = cursor.read_u64_varint()?;
    let has_generation = (flagged_index & 1) > 0;
    let generation = if has_generation {
        cursor.read_u32_varint()?
    } else {
        0u32
    };

    let bits = (generation as u64) << 32 | (flagged_index >> 1);

    Ok(Entity::from_bits(bits))
}

/// Deserializes an Entity then an optional second Entity from the cursor.
/// see [`ReplicationBuffer::write_entity_combo()`].
fn deserialize_entity_combo(
    cursor: &mut Cursor<Bytes>,
) -> Result<(Entity, Option<Entity>), bincode::Error> {
    let flagged_index: u64 = DefaultOptions::new().deserialize_from(&mut *cursor)?;
    let has_generation = (flagged_index & 1) > 0;
    let has_prediction = (flagged_index & 2) > 0;
    let has_prediction_generation = (flagged_index & 4) > 0;

    let generation = if has_generation {
        DefaultOptions::new().deserialize_from(&mut *cursor)?
    } else {
        0u32
    };

    let bits = (generation as u64) << 32 | (flagged_index >> 3);
    let entity = Entity::from_bits(bits);

    let predicted_entity: Option<Entity> = if has_prediction {
        let predicted_index: u32 = DefaultOptions::new().deserialize_from(&mut *cursor)?;
        let predicted_generation = if has_prediction_generation {
            DefaultOptions::new().deserialize_from(&mut *cursor)?
        } else {
            0u32
        };
        let bits = (predicted_generation as u64) << 32 | predicted_index as u64;
        Some(Entity::from_bits(bits))
    } else {
        None
    };

    Ok((entity, predicted_entity))
}

/// Type of component change.
///
/// Parameter for [`deserialize_component_diffs`].
enum DiffKind {
    Change,
    Removal,
}

/// Last received tick from server.
///
/// Used only on clients, sent to the server in last replicon ack message.
#[derive(Default, Resource, Deref)]
pub struct LastRepliconTick(pub(super) RepliconTick);

/// Set with replication and event systems related to client.
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum ClientSet {
    /// Systems that receive data.
    ///
    /// Runs in `PreUpdate`.
    Receive,
    /// Systems that send data.
    ///
    /// Runs in `PostUpdate`.
    Send,
}

/// Signature for callback, when an entity matched to a predicted client entity
/// typically you want to remove any prediction component in here.
pub type PredictionHitFn = fn(&mut EntityMut);

/// Maps server entities to client entities and vice versa.
///
/// Used only on client.
#[derive(Default, Resource)]
pub struct NetworkEntityMap {
    server_to_client: HashMap<Entity, Entity>,
    client_to_server: HashMap<Entity, Entity>,
    prediction_hit_fn: Option<PredictionHitFn>,
}

impl NetworkEntityMap {
    pub fn set_prediction_hit_callback(&mut self, predition_hit_fn: PredictionHitFn) {
        self.prediction_hit_fn = Some(predition_hit_fn);
    }

    #[inline]
    pub fn insert(&mut self, server_entity: Entity, client_entity: Entity) {
        self.server_to_client.insert(server_entity, client_entity);
        self.client_to_server.insert(client_entity, server_entity);
    }

    pub(super) fn get_by_server_or_spawn<'a>(
        &mut self,
        world: &'a mut World,
        server_entity: Entity,
        predicted_entity: Option<Entity>,
    ) -> EntityMut<'a> {
        match self.server_to_client.entry(server_entity) {
            Entry::Occupied(entry) => world.entity_mut(*entry.get()),
            Entry::Vacant(entry) => {
                // test if a predicted entity exists
                let client_entity = match predicted_entity {
                    Some(e) => {
                        if world.get_entity(e).is_some() {
                            // hit. call prediction_hit callback if specified
                            let mut ent_mut = world.get_entity_mut(e).unwrap();
                            if let Some(pred_fn) = self.prediction_hit_fn {
                                (pred_fn)(&mut ent_mut)
                            }
                            ent_mut.insert(Replication);
                            ent_mut
                        } else {
                            // miss, spawn a new entity.
                            world.spawn(Replication)
                        }
                    }
                    None => world.spawn(Replication),
                };

                entry.insert(client_entity.id());
                self.client_to_server
                    .insert(client_entity.id(), server_entity);
                client_entity
            }
        }
    }

    pub(super) fn remove_by_server(&mut self, server_entity: Entity) -> Option<Entity> {
        let client_entity = self.server_to_client.remove(&server_entity);
        if let Some(client_entity) = client_entity {
            self.client_to_server.remove(&client_entity);
        }
        client_entity
    }

    #[inline]
    pub fn to_client(&self) -> &HashMap<Entity, Entity> {
        &self.server_to_client
    }

    #[inline]
    pub fn to_server(&self) -> &HashMap<Entity, Entity> {
        &self.client_to_server
    }

    fn clear(&mut self) {
        self.client_to_server.clear();
        self.server_to_client.clear();
    }
}

/// Maps server entities into client entities inside components.
///
/// Spawns new client entity if a mapping doesn't exists.
pub struct ClientMapper<'a> {
    world: &'a mut World,
    server_to_client: &'a mut HashMap<Entity, Entity>,
    client_to_server: &'a mut HashMap<Entity, Entity>,
}

impl<'a> ClientMapper<'a> {
    #[inline]
    pub fn new(world: &'a mut World, entity_map: &'a mut NetworkEntityMap) -> Self {
        Self {
            world,
            server_to_client: &mut entity_map.server_to_client,
            client_to_server: &mut entity_map.client_to_server,
        }
    }
}

impl Mapper for ClientMapper<'_> {
    fn map(&mut self, entity: Entity) -> Entity {
        *self.server_to_client.entry(entity).or_insert_with(|| {
            let client_entity = self.world.spawn(Replication).id();
            self.client_to_server.insert(client_entity, entity);
            client_entity
        })
    }
}
