use std::mem;

use bevy::{
    ecs::{
        archetype::{ArchetypeGeneration, ArchetypeId, Archetypes},
        component::{ComponentId, Components},
    },
    log::Level,
    prelude::*,
    utils::tracing::enabled,
};

use crate::core::replication::{
    replication_registry::FnsId, replication_rules::ReplicationRules, Replicated,
};

/// Cached information about all replicated archetypes.
#[derive(Deref)]
pub(crate) struct ReplicatedArchetypes {
    /// ID of [`Replicated`] component.
    marker_id: ComponentId,

    /// Highest processed archetype ID.
    generation: ArchetypeGeneration,

    /// Archetypes marked as replicated.
    #[deref]
    archetypes: Vec<ReplicatedArchetype>,
}

impl ReplicatedArchetypes {
    /// ID of the [`Replicated`] component.
    pub(crate) fn marker_id(&self) -> ComponentId {
        self.marker_id
    }

    /// Updates the internal view of the [`World`]'s replicated archetypes.
    ///
    /// If this is not called before querying data, the results may not accurately reflect what is in the world.
    pub(super) fn update(
        &mut self,
        archetypes: &Archetypes,
        components: &Components,
        rules: &ReplicationRules,
    ) {
        let old_generation = mem::replace(&mut self.generation, archetypes.generation());

        // Archetypes are never removed, iterate over newly added since the last update.
        for archetype in archetypes[old_generation..]
            .iter()
            .filter(|archetype| archetype.contains(self.marker_id))
        {
            let mut replicated_archetype = ReplicatedArchetype::new(archetype.id());
            for rule in rules.iter().filter(|rule| rule.matches(archetype)) {
                for &(component_id, fns_id) in &rule.components {
                    // Since rules are sorted by priority,
                    // we are inserting only new components that aren't present.
                    if replicated_archetype
                        .components
                        .iter()
                        .any(|component| component.component_id == component_id)
                    {
                        if enabled!(Level::DEBUG) {
                            let component_name = components
                                .get_name(component_id)
                                .expect("rules should be registered with valid component");

                            let component_names: Vec<_> = replicated_archetype
                                .components
                                .iter()
                                .flat_map(|component| components.get_name(component.component_id))
                                .collect();

                            debug!("ignoring component `{component_name}` with priority {} for archetype with `{component_names:?}`", rule.priority);
                        }

                        continue;
                    }

                    replicated_archetype.components.push(ReplicatedComponent {
                        component_id,
                        fns_id,
                    });
                }
            }
            self.archetypes.push(replicated_archetype);
        }
    }
}

impl FromWorld for ReplicatedArchetypes {
    fn from_world(world: &mut World) -> Self {
        Self {
            marker_id: world.register_component::<Replicated>(),
            generation: ArchetypeGeneration::initial(),
            archetypes: Default::default(),
        }
    }
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
    pub(super) fns_id: FnsId,
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::{core::replication::replication_registry::ReplicationRegistry, AppRuleExt};

    #[test]
    fn empty() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>();

        app.world_mut().spawn_empty();

        let archetypes = match_archetypes(app.world_mut());
        assert!(archetypes.is_empty());
    }

    #[test]
    fn no_components() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>();

        app.world_mut().spawn(Replicated);

        let archetypes = match_archetypes(app.world_mut());
        assert_eq!(archetypes.len(), 1);

        let archetype = archetypes.first().unwrap();
        assert!(archetype.components.is_empty());
    }

    #[test]
    fn not_replicated() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>();

        app.world_mut().spawn((Replicated, ComponentB));

        let archetypes = match_archetypes(app.world_mut());
        let archetype = archetypes.first().unwrap();
        assert!(archetype.components.is_empty());
    }

    #[test]
    fn component() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>();

        app.world_mut().spawn((Replicated, ComponentA));

        let archetypes = match_archetypes(app.world_mut());
        let archetype = archetypes.first().unwrap();
        assert_eq!(archetype.components.len(), 1);
    }

    #[test]
    fn group() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate_group::<(ComponentA, ComponentB)>();

        app.world_mut().spawn((Replicated, ComponentA, ComponentB));

        let archetypes = match_archetypes(app.world_mut());
        let archetype = archetypes.first().unwrap();
        assert_eq!(archetype.components.len(), 2);
    }

    #[test]
    fn part_of_group() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate_group::<(ComponentA, ComponentB)>();

        app.world_mut().spawn((Replicated, ComponentA));

        let archetypes = match_archetypes(app.world_mut());
        let archetype = archetypes.first().unwrap();
        assert!(archetype.components.is_empty());
    }

    #[test]
    fn group_with_subset() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>()
            .replicate_group::<(ComponentA, ComponentB)>();

        app.world_mut().spawn((Replicated, ComponentA, ComponentB));

        let archetypes = match_archetypes(app.world_mut());
        let archetype = archetypes.first().unwrap();
        assert_eq!(archetype.components.len(), 2);
    }

    #[test]
    fn group_with_multiple_subsets() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate::<ComponentA>()
            .replicate::<ComponentB>()
            .replicate_group::<(ComponentA, ComponentB)>();

        app.world_mut().spawn((Replicated, ComponentA, ComponentB));

        let archetypes = match_archetypes(app.world_mut());
        let archetype = archetypes.first().unwrap();
        assert_eq!(archetype.components.len(), 2);
    }

    #[test]
    fn groups_with_overlap() {
        let mut app = App::new();
        app.init_resource::<ReplicationRules>()
            .init_resource::<ReplicationRegistry>()
            .replicate_group::<(ComponentA, ComponentC)>()
            .replicate_group::<(ComponentA, ComponentB)>();

        app.world_mut()
            .spawn((Replicated, ComponentA, ComponentB, ComponentC));

        let archetypes = match_archetypes(app.world_mut());
        let archetype = archetypes.first().unwrap();
        assert_eq!(archetype.components.len(), 3);
    }

    fn match_archetypes(world: &mut World) -> ReplicatedArchetypes {
        let mut archetypes = ReplicatedArchetypes::from_world(world);
        archetypes.update(
            world.archetypes(),
            world.components(),
            world.resource::<ReplicationRules>(),
        );

        archetypes
    }

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentA;

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentB;

    #[derive(Serialize, Deserialize, Component)]
    struct ComponentC;
}
