use bevy::prelude::*;
use bevy_replicon::{
    core::server_entity_map::ServerEntityMap, prelude::*, test_app::ServerTestAppExt,
};
use serde::{Deserialize, Serialize};

#[test]
fn single() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
        ))
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app.world_mut().spawn(Replicated).id();
    let client_entity = client_app.world_mut().spawn(Replicated).id();

    client_app
        .world_mut()
        .resource_mut::<ServerEntityMap>()
        .insert(server_entity, client_entity);

    server_app.world_mut().despawn(server_entity);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert!(client_app.world().get_entity(client_entity).is_err());

    let entity_map = client_app.world().resource::<ServerEntityMap>();
    assert!(entity_map.to_client().is_empty());
    assert!(entity_map.to_server().is_empty());
}

#[test]
fn with_hierarchy() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
        ))
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_child_entity = server_app.world_mut().spawn(Replicated).id();
    let server_entity = server_app
        .world_mut()
        .spawn(Replicated)
        .add_children(&[server_child_entity])
        .id();

    let client_child_entity = client_app.world_mut().spawn(Replicated).id();
    let client_entity = client_app
        .world_mut()
        .spawn(Replicated)
        .add_children(&[client_child_entity])
        .id();

    let mut entity_map = client_app.world_mut().resource_mut::<ServerEntityMap>();
    entity_map.insert(server_entity, client_entity);
    entity_map.insert(server_child_entity, client_child_entity);

    server_app.world_mut().despawn(server_entity);
    server_app.world_mut().despawn(server_child_entity);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let mut replicated = client_app.world_mut().query::<&Replicated>();
    assert!(replicated.iter(client_app.world()).next().is_none());
}

#[test]
fn after_spawn() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
        ))
        .replicate::<DummyComponent>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    // Insert and remove `Replicated` to trigger spawn and despawn for client at the same time.
    server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .remove::<Replicated>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let mut replicated = client_app.world_mut().query::<&Replicated>();
    assert!(replicated.iter(client_app.world()).next().is_none());
}

#[test]
fn hidden() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                visibility_policy: VisibilityPolicy::Whitelist, // Hide all spawned entities by default.
                ..Default::default()
            }),
        ))
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app.world_mut().spawn(Replicated).id();
    let allocated_entities = client_app.world().entities().total_count();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app.world_mut().despawn(server_entity);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    assert_eq!(
        client_app.world().entities().total_count(),
        allocated_entities,
        "client shouldn't spawn or despawn hidden entity"
    );
}

#[derive(Component, Deserialize, Serialize)]
struct DummyComponent;
