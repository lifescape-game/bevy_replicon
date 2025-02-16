use bevy::{
    ecs::{
        archetype::{Archetype, ArchetypeEntity, ArchetypeId},
        component::{ComponentId, ComponentTicks, StorageType, Tick},
        query::{Access, FilteredAccess},
        storage::TableId,
        system::{ReadOnlySystemParam, SystemMeta, SystemParam},
        world::unsafe_world_cell::UnsafeWorldCell,
    },
    prelude::*,
    ptr::Ptr,
};

use crate::core::replication::{
    replication_registry::FnsId, replication_rules::ReplicationRules, Replicated,
};

/// A [`SystemParam`] that wraps [`World`], but provides access only for replicated components.
///
/// We don't use [`FilteredEntityRef`](bevy::ecs::world::FilteredEntityRef) to avoid access checks
/// and [`StorageType`] fetch (we cache this information on replicated archetypes).
pub(crate) struct ReplicationReadWorld<'w, 's> {
    world: UnsafeWorldCell<'w>,
    state: &'s ReplicationReadState,
}

impl<'w> ReplicationReadWorld<'w, '_> {
    /// Extracts a component as [`Ptr`] and its ticks from a table or sparse set, depending on its storage type.
    ///
    /// # Safety
    ///
    /// The component must be present in this archetype, have the specified storage type, and be previously marked for replication.
    pub(super) unsafe fn get_component_unchecked(
        &self,
        entity: &ArchetypeEntity,
        table_id: TableId,
        storage_type: StorageType,
        component_id: ComponentId,
    ) -> (Ptr<'w>, ComponentTicks) {
        debug_assert!(self.state.access.has_component_read(component_id));

        let storages = self.world.storages();
        match storage_type {
            StorageType::Table => {
                let table = storages.tables.get(table_id).unwrap_unchecked();
                // TODO: re-use column lookup, asked in https://github.com/bevyengine/bevy/issues/16593.
                let component: Ptr<'w> = table
                    .get_component(component_id, entity.table_row())
                    .unwrap_unchecked();
                let ticks = table
                    .get_ticks_unchecked(component_id, entity.table_row())
                    .unwrap_unchecked();

                (component, ticks)
            }
            StorageType::SparseSet => {
                let sparse_set = storages.sparse_sets.get(component_id).unwrap_unchecked();
                let component = sparse_set.get(entity.id()).unwrap_unchecked();
                let ticks = sparse_set.get_ticks(entity.id()).unwrap_unchecked();

                (component, ticks)
            }
        }
    }

    /// ID of the [`Replicated`] component.
    pub(super) fn marker_id(&self) -> ComponentId {
        self.state.marker_id
    }

    /// Return iterator over replicated archetypes.
    pub(super) fn iter_archetypes(
        &self,
    ) -> impl Iterator<Item = (&Archetype, &ReplicatedArchetype)> {
        self.state.archetypes.iter().map(|replicated_archetype| {
            // SAFETY: all IDs from replicated archetypes obtained from real archetypes.
            let archetype = unsafe {
                self.world
                    .archetypes()
                    .get(replicated_archetype.id)
                    .unwrap_unchecked()
            };

            (archetype, replicated_archetype)
        })
    }
}

unsafe impl SystemParam for ReplicationReadWorld<'_, '_> {
    type State = ReplicationReadState;
    type Item<'world, 'state> = ReplicationReadWorld<'world, 'state>;

    fn init_state(world: &mut World, system_meta: &mut SystemMeta) -> Self::State {
        let mut filtered_access = FilteredAccess::default();

        let marker_id = world.register_component::<Replicated>();
        filtered_access.add_component_read(marker_id);

        let rules = world.resource::<ReplicationRules>();
        let combined_access = system_meta.component_access_set().combined_access();
        for rule in rules.iter() {
            for &(component_id, _) in &rule.components {
                filtered_access.add_component_read(component_id);
                assert!(
                    !combined_access.has_component_write(component_id),
                    "replicated component `{}` in system `{}` shouldn't be in conflict with other system parameters",
                    world.components().get_name(component_id).unwrap(),
                    system_meta.name(),
                );
            }
        }

        let access = filtered_access.access().clone();

        // SAFETY: used only to extend access.
        unsafe {
            system_meta.component_access_set_mut().add(filtered_access);
        }

        ReplicationReadState {
            access,
            marker_id,
            archetypes: Default::default(),
            // Needs to be cloned because `new_archetype` only accepts the state.
            rules: world.resource::<ReplicationRules>().clone(),
        }
    }

    unsafe fn new_archetype(
        state: &mut Self::State,
        archetype: &Archetype,
        system_meta: &mut SystemMeta,
    ) {
        if !archetype.contains(state.marker_id) {
            return;
        }

        let mut replicated_archetype = ReplicatedArchetype::new(archetype.id());
        for rule in state.rules.iter().filter(|rule| rule.matches(archetype)) {
            for &(component_id, fns_id) in &rule.components {
                // Since rules are sorted by priority,
                // we are inserting only new components that aren't present.
                if replicated_archetype
                    .components
                    .iter()
                    .any(|component| component.component_id == component_id)
                {
                    continue;
                }

                let storage_type = archetype.get_storage_type(component_id).unwrap_unchecked();
                replicated_archetype.components.push(ReplicatedComponent {
                    component_id,
                    storage_type,
                    fns_id,
                });
            }
        }

        // Update system access for proper parallelization.
        for component in &replicated_archetype.components {
            let archetype_id = archetype
                .get_archetype_component_id(component.component_id)
                .unwrap_unchecked();
            system_meta
                .archetype_component_access_mut()
                .add_component_read(archetype_id)
        }

        // Store for future iteration.
        state.archetypes.push(replicated_archetype);
    }

    unsafe fn get_param<'world, 'state>(
        state: &'state mut Self::State,
        _system_meta: &SystemMeta,
        world: UnsafeWorldCell<'world>,
        _change_tick: Tick,
    ) -> Self::Item<'world, 'state> {
        ReplicationReadWorld { world, state }
    }
}

unsafe impl ReadOnlySystemParam for ReplicationReadWorld<'_, '_> {}

pub(crate) struct ReplicationReadState {
    /// All replicated components.
    ///
    /// Used only in debug to check component access.
    access: Access<ComponentId>,

    /// ID of [`Replicated`] component.
    marker_id: ComponentId,

    /// Archetypes marked as replicated.
    archetypes: Vec<ReplicatedArchetype>,

    rules: ReplicationRules,
}

/// An archetype that can be stored in [`ReplicatedArchetypes`].
pub(crate) struct ReplicatedArchetype {
    /// Associated archetype ID.
    pub(super) id: ArchetypeId,

    /// Components marked as replicated.
    pub(super) components: Vec<ReplicatedComponent>,
}

impl ReplicatedArchetype {
    fn new(id: ArchetypeId) -> Self {
        Self {
            id,
            components: Default::default(),
        }
    }
}

/// Stores information about a replicated component.
pub(super) struct ReplicatedComponent {
    component_id: ComponentId,
    pub(super) storage_type: StorageType,
    pub(super) fns_id: FnsId,
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::core::replication::{
        replication_registry::ReplicationRegistry, replication_rules::AppRuleExt,
    };

    #[test]
    #[should_panic]
    fn world_then_query() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<Transform>()
            .add_systems(
                Update,
                |_: ReplicationReadWorld, _: Query<&mut Transform>| {},
            );

        app.update();
    }

    #[test]
    #[should_panic]
    fn query_then_world() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<Transform>()
            .add_systems(
                Update,
                |_: Query<&mut Transform>, _: ReplicationReadWorld| {},
            );

        app.update();
    }

    #[test]
    fn world_then_readonly_query() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<Transform>()
            .add_systems(Update, |_: ReplicationReadWorld, _: Query<&Transform>| {});

        app.update();
    }

    #[test]
    fn replicate_after_system() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .add_systems(Update, |_: ReplicationReadWorld, _: Query<&Transform>| {})
            .replicate::<Transform>();

        app.update();
    }

    #[test]
    fn empty() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert!(world.state.archetypes.is_empty());
            });

        app.world_mut().spawn_empty();
        app.update();
    }

    #[test]
    fn no_components() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert!(archetype.components.is_empty());
            });

        app.world_mut().spawn(Replicated);
        app.update();
    }

    #[test]
    fn not_replicated() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert!(archetype.components.is_empty());
            });

        app.world_mut().spawn((Replicated, ComponentB));
        app.update();
    }

    #[test]
    fn component() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert_eq!(archetype.components.len(), 1);
            });

        app.world_mut().spawn((Replicated, ComponentA));
        app.update();
    }

    #[test]
    fn group() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate_group::<(ComponentA, ComponentB)>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert_eq!(archetype.components.len(), 2);
            });

        app.world_mut().spawn((Replicated, ComponentA, ComponentB));
        app.update();
    }

    #[test]
    fn part_of_group() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate_group::<(ComponentA, ComponentB)>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert!(archetype.components.is_empty());
            });

        app.world_mut().spawn((Replicated, ComponentA));
        app.update();
    }

    #[test]
    fn group_with_subset() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>()
            .replicate_group::<(ComponentA, ComponentB)>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert_eq!(archetype.components.len(), 2);
            });

        app.world_mut().spawn((Replicated, ComponentA, ComponentB));
        app.update();
    }

    #[test]
    fn group_with_multiple_subsets() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>()
            .replicate::<ComponentB>()
            .replicate_group::<(ComponentA, ComponentB)>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert_eq!(archetype.components.len(), 2);
            });

        app.world_mut().spawn((Replicated, ComponentA, ComponentB));
        app.update();
    }

    #[test]
    fn groups_with_overlap() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate_group::<(ComponentA, ComponentC)>()
            .replicate_group::<(ComponentA, ComponentB)>()
            .add_systems(Update, |world: ReplicationReadWorld| {
                assert_eq!(world.state.archetypes.len(), 1);
                let archetype = world.state.archetypes.first().unwrap();
                assert_eq!(archetype.components.len(), 3);
            });

        app.world_mut()
            .spawn((Replicated, ComponentA, ComponentB, ComponentC));
        app.update();
    }

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentA;

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentB;

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentC;
}
