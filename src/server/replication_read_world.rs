use bevy::{
    ecs::{
        archetype::{ArchetypeEntity, Archetypes},
        component::{ComponentId, ComponentTicks, Components, StorageType, Tick},
        query::{Access, FilteredAccess},
        storage::TableId,
        system::{ReadOnlySystemParam, SystemMeta, SystemParam},
        world::unsafe_world_cell::UnsafeWorldCell,
    },
    prelude::*,
    ptr::Ptr,
};

use crate::core::replication::{replication_rules::ReplicationRules, Replicated};

/// A [`SystemParam`] that wraps [`World`], but provides access only for replicated components.
///
/// We don't use [`FilteredEntityRef`](bevy::ecs::world::FilteredEntityRef) to avoid access checks
/// and [`StorageType`] fetch (we cache this information on replicated archetypes).
pub(crate) struct ReplicationReadWorld<'w, 's> {
    world: UnsafeWorldCell<'w>,
    state: &'s Access<ComponentId>,
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
        debug_assert!(self.state.has_component_read(component_id));

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

    pub(super) fn archetypes(&self) -> &Archetypes {
        self.world.archetypes()
    }

    pub(super) fn components(&self) -> &Components {
        self.world.components()
    }
}

unsafe impl SystemParam for ReplicationReadWorld<'_, '_> {
    type State = Access<ComponentId>;
    type Item<'world, 'state> = ReplicationReadWorld<'world, 'state>;

    fn init_state(world: &mut World, system_meta: &mut SystemMeta) -> Self::State {
        let mut access = Access::new();
        let mut filtered_access = FilteredAccess::default();

        let marker_id = world.register_component::<Replicated>();
        access.add_component_read(marker_id);
        filtered_access.add_component_read(marker_id);

        let rules = world.resource::<ReplicationRules>();
        let combined_access = system_meta.component_access_set().combined_access();
        for rule in rules.iter() {
            for &(component_id, _) in &rule.components {
                access.add_component_read(component_id);
                filtered_access.add_component_read(component_id);
                assert!(
                    !combined_access.has_component_write(component_id),
                    "replicated component `{}` in system `{}` shouldn't be in conflict with other system parameters",
                    world.components().get_name(component_id).unwrap(),
                    system_meta.name(),
                );
            }
        }

        // SAFETY: used only to extend access.
        unsafe {
            system_meta.component_access_set_mut().add(filtered_access);
        }

        access
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

#[cfg(test)]
mod tests {
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
}
