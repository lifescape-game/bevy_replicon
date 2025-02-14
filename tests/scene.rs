use bevy::prelude::*;
use bevy_replicon::{prelude::*, scene};
use serde::{Deserialize, Serialize};

#[test]
fn replicated_entity() {
    let mut app = App::new();
    app.add_plugins(RepliconPlugins)
        .register_type::<DummyComponent>()
        .register_type::<NonReflectedComponent>()
        .replicate::<DummyComponent>()
        .replicate::<OtherReflectedComponent>() // Reflected, but the type is not registered.
        .replicate::<NonReflectedComponent>()
        .finish();

    let entity = app
        .world_mut()
        .spawn((
            Replicated,
            DummyComponent,
            OtherReflectedComponent,
            NonReflectedComponent,
        ))
        .id();

    let mut scene = DynamicScene::default();
    scene::replicate_into(&mut scene, app.world());

    assert!(scene.resources.is_empty());
    assert_eq!(scene.entities.len(), 1);

    let dyn_entity = &scene.entities[0];
    assert_eq!(dyn_entity.entity, entity);
    assert_eq!(
        dyn_entity.components.len(),
        1,
        "entity should have only registered components with `#[reflect(Component)]`"
    );
}

#[test]
fn empty_entity() {
    let mut app = App::new();
    app.add_plugins(RepliconPlugins).finish();

    let entity = app.world_mut().spawn(Replicated).id();

    // Extend with replicated components.
    let mut scene = DynamicScene::default();
    scene::replicate_into(&mut scene, app.world());

    assert!(scene.resources.is_empty());
    assert_eq!(scene.entities.len(), 1);

    let dyn_entity = &scene.entities[0];
    assert_eq!(dyn_entity.entity, entity);
    assert!(dyn_entity.components.is_empty());
}

#[test]
fn not_replicated_entity() {
    let mut app = App::new();
    app.add_plugins(RepliconPlugins)
        .register_type::<DummyComponent>()
        .replicate::<DummyComponent>()
        .finish();

    app.world_mut().spawn(DummyComponent);

    let mut scene = DynamicScene::default();
    scene::replicate_into(&mut scene, app.world());

    assert!(scene.resources.is_empty());
    assert!(scene.entities.is_empty());
}

#[test]
fn entity_update() {
    let mut app = App::new();
    app.add_plugins(RepliconPlugins)
        .register_type::<DummyComponent>()
        .replicate::<DummyComponent>()
        .register_type::<OtherReflectedComponent>()
        .finish();

    let entity = app
        .world_mut()
        .spawn((Replicated, DummyComponent, OtherReflectedComponent))
        .id();

    // Populate scene only with a single non-replicated component.
    let mut scene = DynamicSceneBuilder::from_world(app.world())
        .allow_component::<OtherReflectedComponent>()
        .extract_entity(entity)
        .build();

    // Update already extracted entity with replicated components.
    scene::replicate_into(&mut scene, app.world());

    assert!(scene.resources.is_empty());
    assert_eq!(scene.entities.len(), 1);

    let dyn_entity = &scene.entities[0];
    assert_eq!(dyn_entity.entity, entity);
    assert_eq!(dyn_entity.components.len(), 2);
}

#[derive(Component, Default, Deserialize, Reflect, Serialize)]
#[reflect(Component)]
struct DummyComponent;

#[derive(Component, Default, Deserialize, Reflect, Serialize)]
#[reflect(Component)]
struct OtherReflectedComponent;

/// Component that have `Reflect` derive, but without `#[reflect(Component)]`
#[derive(Component, Default, Deserialize, Reflect, Serialize)]
struct NonReflectedComponent;
