use bevy::prelude::*;
use bevy_replicon::{
    client::confirm_history::ConfirmHistory, core::server_entity_map::ServerEntityMap, prelude::*,
    test_app::ServerTestAppExt,
};
use serde::{Deserialize, Serialize};

#[test]
fn empty() {
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

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, With<Replicated>>()
        .single(client_app.world());

    let entity_map = client_app.world().resource::<ServerEntityMap>();
    assert_eq!(
        entity_map.to_client().get(&server_entity),
        Some(&client_entity),
        "server entity should be mapped to a replicated entity on client"
    );
    assert_eq!(
        entity_map.to_server().get(&client_entity),
        Some(&server_entity),
        "replicated entity on client should be mapped to a server entity"
    );
}

#[test]
fn with_component() {
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

    server_app.world_mut().spawn((Replicated, DummyComponent));

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    client_app
        .world_mut()
        .query::<(&Replicated, &DummyComponent)>()
        .single(client_app.world());
}

#[test]
fn with_old_component() {
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

    // Spawn an entity with replicated component, but without a marker.
    let server_entity = server_app.world_mut().spawn(DummyComponent).id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let mut replicated = client_app.world_mut().query::<&Replicated>();
    assert!(replicated.iter(client_app.world()).next().is_none());

    // Enable replication for previously spawned entity
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .insert(Replicated);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    client_app
        .world_mut()
        .query::<(&Replicated, &DummyComponent)>()
        .single(client_app.world());
}

#[test]
fn before_connection() {
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

    // Spawn an entity before client connected.
    server_app.world_mut().spawn((Replicated, DummyComponent));

    server_app.connect_client(&mut client_app);

    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    client_app
        .world_mut()
        .query::<(&Replicated, &DummyComponent)>()
        .single(client_app.world());
}

#[test]
fn pre_spawn() {
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

    // Make client and server have different entity IDs.
    server_app.world_mut().spawn_empty();

    let client_entity = client_app.world_mut().spawn_empty().id();
    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .id();

    let client = client_app.world().resource::<RepliconClient>();
    let client_id = client.id().unwrap();

    let mut entity_map = server_app.world_mut().resource_mut::<ClientEntityMap>();
    entity_map.insert(
        client_id,
        ClientMapping {
            server_entity,
            client_entity,
        },
    );

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let entity_map = client_app.world().resource::<ServerEntityMap>();
    assert_eq!(
        entity_map.to_client().get(&server_entity),
        Some(&client_entity),
        "server entity should be mapped to a replicated entity on client"
    );
    assert_eq!(
        entity_map.to_server().get(&client_entity),
        Some(&server_entity),
        "replicated entity on client should be mapped to a server entity"
    );

    let client_entity = client_app.world().entity(client_entity);
    assert!(
        client_entity.contains::<Replicated>(),
        "entity should start receive replication"
    );
    assert!(
        client_entity.contains::<ConfirmHistory>(),
        "server should confirm replication of client entity"
    );
    assert!(
        client_entity.contains::<DummyComponent>(),
        "component from server should be replicated"
    );

    let mut replicated = client_app.world_mut().query::<&Replicated>();
    assert_eq!(
        replicated.iter(client_app.world()).count(),
        1,
        "new entity shouldn't be spawned on client"
    );
}

#[test]
fn after_despawn() {
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

    // Remove and insert `Replicated` to trigger despawn and spawn for client at the same time.
    server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .remove::<Replicated>()
        .insert(Replicated);

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    client_app
        .world_mut()
        .query::<(&Replicated, &DummyComponent)>()
        .single(client_app.world());
}

#[derive(Component, Deserialize, Serialize)]
struct DummyComponent;
