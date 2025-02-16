use std::cmp::Reverse;

use bevy::{
    ecs::{archetype::Archetype, component::ComponentId, entity::MapEntities},
    prelude::*,
    utils::HashSet,
};
use serde::{de::DeserializeOwned, Serialize};

use super::replication_registry::{rule_fns::RuleFns, FnsId, ReplicationRegistry};

/// Replication functions for [`App`].
pub trait AppRuleExt {
    /// Creates a replication rule for a single component.
    ///
    /// The component will be replicated if its entity contains the [`Replicated`](super::Replicated)
    /// marker component.
    ///
    /// Component will be serialized and deserialized as-is using postcard.
    /// To customize it, use [`Self::replicate_group`].
    ///
    /// If your component contains any [`Entity`] inside, use [`Self::replicate_mapped`].
    ///
    /// See also [`Self::replicate_with`] and the section on [`components`](../../index.html#components)
    /// from the quick start guide.
    fn replicate<C>(&mut self) -> &mut Self
    where
        C: Component + Serialize + DeserializeOwned,
    {
        self.replicate_with::<C>(RuleFns::default())
    }

    /**
    Same as [`Self::replicate`], but additionally maps server entities to client inside the component after receiving.

    Always use it for components that contain entities.

    See also [`Self::replicate`].

    # Examples

    ```
    # use bevy::{prelude::*, ecs::entity::{EntityMapper, MapEntities}};
    # use bevy_replicon::prelude::*;
    # use serde::{Deserialize, Serialize};
    # let mut app = App::new();
    # app.add_plugins(RepliconPlugins);
    app.replicate_mapped::<MappedComponent>();

    #[derive(Component, Deserialize, Serialize)]
    struct MappedComponent(Entity);

    impl MapEntities for MappedComponent {
        fn map_entities<T: EntityMapper>(&mut self, mapper: &mut T) {
            self.0 = mapper.map_entity(self.0);
        }
    }
    ```
    **/
    fn replicate_mapped<C>(&mut self) -> &mut Self
    where
        C: Component + Serialize + DeserializeOwned + MapEntities,
    {
        self.replicate_with::<C>(RuleFns::default_mapped())
    }

    /**
    Same as [`Self::replicate`], but uses the specified functions for serialization and deserialization.

    Can be used to customize how the component will be passed over the network or
    for components that don't implement [`Serialize`] or [`DeserializeOwned`].

    You can also override how the component will be written,
    see [`AppMarkerExt`](super::command_markers::AppMarkerExt).

    See also [`postcard_utils`](crate::core::postcard_utils).

    # Examples

    Ser/de only [`Transform::translation`]:

    ```
    use bevy::prelude::*;
    use bevy_replicon::{
        bytes::Bytes,
        core::{
            postcard_utils,
            replication::replication_registry::{
                ctx::{SerializeCtx, WriteCtx},
                rule_fns::{RuleFns, DeserializeFn},
            },
        },
        prelude::*,
    };

    # let mut app = App::new();
    # app.add_plugins(RepliconPlugins);
    // We override in-place as well to apply only translation when the component is already inserted.
    app.replicate_with(
        RuleFns::new(serialize_translation, deserialize_translation)
            .with_in_place(deserialize_transform_in_place),
    );

    /// Serializes only `translation` from [`Transform`].
    fn serialize_translation(
        _ctx: &SerializeCtx,
        transform: &Transform,
        message: &mut Vec<u8>,
    ) -> postcard::Result<()> {
        postcard_utils::to_extend_mut(&transform.translation, message)
    }

    /// Deserializes `translation` and creates [`Transform`] from it.
    ///
    /// Called by Replicon on component insertions.
    fn deserialize_translation(
        _ctx: &mut WriteCtx,
        message: &mut Bytes,
    ) -> postcard::Result<Transform> {
        let translation: Vec3 = postcard_utils::from_buf(message)?;
        Ok(Transform::from_translation(translation))
    }

    /// Applies the assigned deserialization function and assigns only translation.
    ///
    /// Called by Replicon on component mutations.
    pub fn deserialize_transform_in_place(
        deserialize: DeserializeFn<Transform>,
        ctx: &mut WriteCtx,
        component: &mut Transform,
        message: &mut Bytes,
    ) -> postcard::Result<()> {
        let transform = (deserialize)(ctx, message)?;
        component.translation = transform.translation;
        Ok(())
    }
    ```

    Ser/de with compression:

    ```
    use bevy::prelude::*;
    use bevy_replicon::{
        bytes::Bytes,
        core::{
            postcard_utils,
            replication::replication_registry::{
                ctx::{SerializeCtx, WriteCtx},
                rule_fns::RuleFns,
            },
        },
        postcard,
        prelude::*,
    };
    use bytes::Buf;
    use serde::{Deserialize, Serialize};

    # let mut app = App::new();
    # app.add_plugins(RepliconPlugins);
    app.replicate_with(RuleFns::new(
        serialize_big_component,
        deserialize_big_component,
    ));

    /// Serializes [`BigComponent`] with compression.
    pub fn serialize_big_component(
        _ctx: &SerializeCtx,
        component: &BigComponent,
        message: &mut Vec<u8>,
    ) -> postcard::Result<()> {
        // Serialize as usual, but track size.
        let start = message.len();
        postcard_utils::to_extend_mut(component, message)?;
        let end = message.len();

        // Compress serialized slice.
        // Could be `zstd`, for example.
        let compressed = compress(&mut message[start..end]);

        // Replace serialized slice with compressed data prepended by its size.
        message.truncate(start);
        postcard_utils::to_extend_mut(&compressed.len(), message)?;
        message.extend(compressed);

        Ok(())
    }

    /// Deserializes [`BigComponent`] with decompression.
    pub fn deserialize_big_component(
        _ctx: &mut WriteCtx,
        message: &mut Bytes,
    ) -> postcard::Result<BigComponent> {
        // Read size first to know how much data is encoded.
        let size = postcard_utils::from_buf(message)?;

        // Apply decompression and advance the reading cursor.
        let decompressed = decompress(&message[..size]);
        message.advance(size);

        let component = postcard::from_bytes(&decompressed)?;
        Ok(component)
    }

    #[derive(Component, Deserialize, Serialize)]
    struct BigComponent(Vec<u64>);
    # fn compress(data: &[u8]) -> Vec<u8> { unimplemented!() }
    # fn decompress(data: &[u8]) -> Vec<u8> { unimplemented!() }
    ```

    Custom ser/de with entity mapping:

    ```
    use bevy::{ecs::entity::MapEntities, prelude::*};
    use bevy_replicon::{
        bytes::Bytes,
        core::{
            postcard_utils,
            replication::replication_registry::{
                ctx::{SerializeCtx, WriteCtx},
                rule_fns::RuleFns,
            },
        },
        postcard,
        prelude::*,
    };
    use serde::{Deserialize, Serialize};

    let mut app = App::new();
    app.add_plugins(RepliconPlugins);
    app.replicate_with(RuleFns::new(
        serialize_big_component,
        deserialize_big_component,
    ));

    /// Serializes [`MappedComponent`], but skips [`MappedComponent::unused_field`].
    pub fn serialize_big_component(
        _ctx: &SerializeCtx,
        component: &MappedComponent,
        message: &mut Vec<u8>,
    ) -> postcard::Result<()> {
        postcard_utils::to_extend_mut(&component.entity, message)
    }

    /// Deserializes an entity and creates [`MappedComponent`] from it.
    pub fn deserialize_big_component(
        ctx: &mut WriteCtx,
        message: &mut Bytes,
    ) -> postcard::Result<MappedComponent> {
        let entity = postcard_utils::from_buf(message)?;
        let mut component = MappedComponent {
            entity,
            unused_field: Default::default(),
        };
        component.map_entities(ctx);
        Ok(component)
    }

    #[derive(Component, Deserialize, Serialize)]
    struct MappedComponent {
        entity: Entity,
        unused_field: Vec<bool>,
    }

    impl MapEntities for MappedComponent {
        fn map_entities<T: EntityMapper>(&mut self, mapper: &mut T) {
            self.entity = mapper.map_entity(self.entity);
        }
    }
    ```
    */
    fn replicate_with<C>(&mut self, rule_fns: RuleFns<C>) -> &mut Self
    where
        C: Component;

    /**
    Creates a replication rule for a group of components.

    A group will only be replicated if all its components are present on the entity.

    If a group contains a single component, it will work the same as [`Self::replicate`].

    If an entity matches multiple groups, functions from a group with higher priority
    will take precedence for overlapping components. For example, a rule with [`Transform`]
    and a `Player` marker will take precedence over a single [`Transform`] rule.

    If you remove a single component from a group, only a single removal will be sent to clients.
    Other group components will continue to be present on both server and clients.
    Replication for them will be stopped, unless they match other rules.

    We provide blanket impls for tuples to replicate them as-is, but a user could manually implement the trait
    to customize how components will be serialized and deserialized. For details see [`GroupReplication`].

    # Panics

    Panics if `debug_assertions` are enabled and any rule is a subset of another.

    # Examples

    Replicate [`Transform`] and user's `Player` marker only if both of them are present on an entity:

    ```
    use bevy::prelude::*;
    use bevy_replicon::prelude::*;
    use serde::{Deserialize, Serialize};

    # let mut app = App::new();
    # app.add_plugins(RepliconPlugins);
    app.replicate_group::<(Transform, Player)>();

    #[derive(Component, Deserialize, Serialize)]
    struct Player;
    ```
    **/
    fn replicate_group<C: GroupReplication>(&mut self) -> &mut Self;
}

impl AppRuleExt for App {
    fn replicate_with<C>(&mut self, rule_fns: RuleFns<C>) -> &mut Self
    where
        C: Component,
    {
        let rule =
            self.world_mut()
                .resource_scope(|world, mut registry: Mut<ReplicationRegistry>| {
                    let fns_info = registry.register_rule_fns(world, rule_fns);
                    ReplicationRule::new(vec![fns_info])
                });

        self.world_mut()
            .resource_mut::<ReplicationRules>()
            .insert(rule);

        self
    }

    fn replicate_group<C: GroupReplication>(&mut self) -> &mut Self {
        let rule =
            self.world_mut()
                .resource_scope(|world, mut registry: Mut<ReplicationRegistry>| {
                    C::register(world, &mut registry)
                });

        self.world_mut()
            .resource_mut::<ReplicationRules>()
            .insert(rule);

        self
    }
}

/// All registered rules for components replication.
#[derive(Default, Deref, Resource, Clone)]
pub struct ReplicationRules(Vec<ReplicationRule>);

impl ReplicationRules {
    /// Inserts a new rule, maintaining sorting by their priority in descending order.
    fn insert(&mut self, rule: ReplicationRule) {
        let index = self
            .binary_search_by_key(&Reverse(rule.priority), |rule| Reverse(rule.priority))
            .unwrap_or_else(|index| index);

        self.0.insert(index, rule);
    }
}

/// Describes a replicated component or a group of components.
#[derive(Clone)]
pub struct ReplicationRule {
    /// Priority for this rule.
    ///
    /// Usually equal to the number of serialized components,
    /// but can be adjusted by the user.
    pub priority: usize,

    /// Rule components and their serialization/deserialization/removal functions.
    pub components: Vec<(ComponentId, FnsId)>,
}

impl ReplicationRule {
    /// Creates a new rule with priority equal to the number of serializable components.
    pub fn new(components: Vec<(ComponentId, FnsId)>) -> Self {
        Self {
            priority: components.len(),
            components,
        }
    }

    /// Determines whether an archetype contains all components required by the rule.
    pub(crate) fn matches(&self, archetype: &Archetype) -> bool {
        self.components
            .iter()
            .all(|&(component_id, _)| archetype.contains(component_id))
    }

    /// Determines whether the rule is applicable to an archetype with removals included and contains at least one removal.
    ///
    /// Returns `true` if all components in this rule are found in either `removed_components` or the
    /// `post_removal_archetype`, and at least one component is found in `removed_components`.
    /// Returning true means the entity with this archetype satisfied this
    /// rule in the previous tick, but then a component within this rule was removed from the entity.
    pub(crate) fn matches_removals(
        &self,
        post_removal_archetype: &Archetype,
        removed_components: &HashSet<ComponentId>,
    ) -> bool {
        let mut matches = false;
        for &(component_id, _) in &self.components {
            if removed_components.contains(&component_id) {
                matches = true;
            } else if !post_removal_archetype.contains(component_id) {
                return false;
            }
        }

        matches
    }
}

/**
Describes how a component group should be serialized, deserialized, written, and removed.

Can be implemented on any struct to create a custom replication group.

# Examples

```
# use bevy::prelude::*;
# use bevy_replicon::{
#     bytes::Bytes,
#     core::replication::{
#         replication_registry::{
#             ctx::{SerializeCtx, WriteCtx},
#             rule_fns::RuleFns,
#             ReplicationRegistry,
#         },
#         replication_rules::{GroupReplication, ReplicationRule},
#     },
#     prelude::*,
# };
# use serde::{Deserialize, Serialize};
# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
app.replicate_group::<PlayerBundle>();

#[derive(Bundle)]
struct PlayerBundle {
    transform: Transform,
    player: Player,
    replicated: Replicated,
}

#[derive(Component, Deserialize, Serialize)]
struct Player;

impl GroupReplication for PlayerBundle {
    fn register(world: &mut World, registry: &mut ReplicationRegistry) -> ReplicationRule {
        // Customize serlialization to serialize only `translation`.
        let transform_info = registry.register_rule_fns(
            world,
            RuleFns::new(serialize_translation, deserialize_translation),
        );

        // Serialize `player` as usual.
        let player_info = registry.register_rule_fns(world, RuleFns::<Player>::default());

        // We skip `replication` registration since it's a special component.
        // It's automatically inserted on clients after replication and
        // deserialization from scenes.

        ReplicationRule::new(vec![transform_info, player_info])
    }
}

# fn serialize_translation(_: &SerializeCtx, _: &Transform, _: &mut Vec<u8>) -> postcard::Result<()> { unimplemented!() }
# fn deserialize_translation(_: &mut WriteCtx, _: &mut Bytes) -> postcard::Result<Transform> { unimplemented!() }
```
**/
pub trait GroupReplication {
    /// Creates the associated replication rules and registers its functions in [`ReplicationRegistry`].
    fn register(world: &mut World, registry: &mut ReplicationRegistry) -> ReplicationRule;
}

macro_rules! impl_registrations {
    ($($type:ident),*) => {
        impl<$($type: Component + Serialize + DeserializeOwned),*> GroupReplication for ($($type,)*) {
            fn register(world: &mut World, registry: &mut ReplicationRegistry) -> ReplicationRule {
                // TODO: initialize with capacity after stabilization: https://github.com/rust-lang/rust/pull/122808
                let mut components = Vec::new();
                $(
                    let fns_info = registry.register_rule_fns(world, RuleFns::<$type>::default());
                    components.push(fns_info);
                )*

                ReplicationRule::new(components)
            }
        }
    }
}

bevy::utils::all_tuples!(impl_registrations, 1, 15, B);

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::AppRuleExt;

    #[test]
    fn sorting() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>()
            .replicate::<ComponentB>()
            .replicate_group::<(ComponentA, ComponentB)>()
            .replicate_group::<(ComponentB, ComponentC)>()
            .replicate::<ComponentC>()
            .replicate::<ComponentD>();

        let replication_rules = app.world().resource::<ReplicationRules>();
        let priorities: Vec<_> = replication_rules.iter().map(|rule| rule.priority).collect();
        assert_eq!(priorities, [2, 2, 1, 1, 1, 1]);
    }

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentA;

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentB;

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentC;

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentD;
}
