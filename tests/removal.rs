use bevy::prelude::*;
use bevy_replicon::{
    client::confirm_history::{ConfirmHistory, EntityReplicated},
    core::replication::{
        deferred_entity::DeferredEntity,
        replication_registry::{command_fns, ctx::WriteCtx, rule_fns::RuleFns},
    },
    prelude::*,
    server::server_tick::ServerTick,
    test_app::ServerTestAppExt,
};
use bytes::Bytes;
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
        .replicate::<DummyComponent>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, With<DummyComponent>>()
        .single(client_app.world());

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<DummyComponent>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app.world().entity(client_entity);
    assert!(!client_entity.contains::<DummyComponent>());
}

#[test]
fn command_fns() {
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
        .replicate::<OriginalComponent>()
        .set_command_fns(replace, command_fns::default_remove::<ReplacedComponent>)
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, OriginalComponent))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, With<ReplacedComponent>>()
        .single(client_app.world());

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<OriginalComponent>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app.world().entity(client_entity);
    assert!(!client_entity.contains::<ReplacedComponent>());
}

#[test]
fn marker() {
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
        .register_marker::<ReplaceMarker>()
        .replicate::<OriginalComponent>()
        .set_marker_fns::<ReplaceMarker, _>(
            replace,
            command_fns::default_remove::<ReplacedComponent>,
        )
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, OriginalComponent))
        .id();

    let client_entity = client_app.world_mut().spawn(ReplaceMarker).id();

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
    server_app.exchange_with_client(&mut client_app);

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<OriginalComponent>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app.world().entity(client_entity);
    assert!(!client_entity.contains::<ReplacedComponent>());
}

#[test]
fn group() {
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
        .replicate_group::<(GroupComponentA, GroupComponentB)>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, (GroupComponentA, GroupComponentB)))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, (With<GroupComponentA>, With<GroupComponentB>)>()
        .single(client_app.world());

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<(GroupComponentA, GroupComponentB)>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app.world().entity(client_entity);
    assert!(!client_entity.contains::<GroupComponentA>());
    assert!(!client_entity.contains::<GroupComponentB>());
}

#[test]
fn not_replicated() {
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

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, NotReplicatedComponent))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, (With<Replicated>, Without<NotReplicatedComponent>)>()
        .single(client_app.world());

    client_app
        .world_mut()
        .entity_mut(client_entity)
        .insert(NotReplicatedComponent);

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<NotReplicatedComponent>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app.world().entity(client_entity);
    assert!(client_entity.contains::<NotReplicatedComponent>());
}

#[test]
fn after_insertion() {
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

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, With<DummyComponent>>()
        .single(client_app.world());

    // Insert and remove at the same time.
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .insert(DummyComponent)
        .remove::<DummyComponent>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let client_entity = client_app.world().entity(client_entity);
    assert!(!client_entity.contains::<DummyComponent>());
}

#[test]
fn with_spawn() {
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

    server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .remove::<DummyComponent>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    client_app
        .world_mut()
        .query_filtered::<Entity, (With<Replicated>, Without<DummyComponent>)>()
        .single(client_app.world());
}

#[test]
fn with_despawn() {
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

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    client_app
        .world_mut()
        .query::<&Replicated>()
        .single(client_app.world());

    // Un-replicate and remove at the same time.
    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<DummyComponent>()
        .remove::<Replicated>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let mut replicated = client_app.world_mut().query::<&Replicated>();
    assert!(replicated.iter(client_app.world()).next().is_none());
}

#[test]
fn confirm_history() {
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

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    let client_entity = client_app
        .world_mut()
        .query_filtered::<Entity, With<DummyComponent>>()
        .single(client_app.world());

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<DummyComponent>();

    // Clear previous events.
    client_app
        .world_mut()
        .resource_mut::<Events<EntityReplicated>>()
        .clear();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let tick = **server_app.world().resource::<ServerTick>();

    let confirm_history = client_app
        .world_mut()
        .get::<ConfirmHistory>(client_entity)
        .unwrap();
    assert!(confirm_history.contains(tick));

    let mut replicated_events = client_app
        .world_mut()
        .resource_mut::<Events<EntityReplicated>>();
    let [event] = replicated_events
        .drain()
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    assert_eq!(event.entity, client_entity);
    assert_eq!(event.tick, tick);
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
        .replicate::<DummyComponent>()
        .finish();
    }

    server_app.connect_client(&mut client_app);

    let server_entity = server_app
        .world_mut()
        .spawn((Replicated, DummyComponent))
        .id();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();
    server_app.exchange_with_client(&mut client_app);

    server_app
        .world_mut()
        .entity_mut(server_entity)
        .remove::<DummyComponent>();

    server_app.update();
    server_app.exchange_with_client(&mut client_app);
    client_app.update();

    let mut replicated = client_app.world_mut().query::<&Replicated>();
    assert!(
        replicated.iter(client_app.world()).next().is_none(),
        "client shouldn't know about hidden entity"
    );
}

#[derive(Component, Deserialize, Serialize)]
struct DummyComponent;

#[derive(Component, Deserialize, Serialize)]
struct GroupComponentA;

#[derive(Component, Deserialize, Serialize)]
struct GroupComponentB;

#[derive(Component, Deserialize, Serialize)]
struct NotReplicatedComponent;

#[derive(Component)]
struct ReplaceMarker;

#[derive(Component, Deserialize, Serialize)]
struct OriginalComponent;

#[derive(Component, Deserialize, Serialize)]
struct ReplacedComponent;

/// Deserializes [`OriginalComponent`], but ignores it and inserts [`ReplacedComponent`].
fn replace(
    ctx: &mut WriteCtx,
    rule_fns: &RuleFns<OriginalComponent>,
    entity: &mut DeferredEntity,
    message: &mut Bytes,
) -> postcard::Result<()> {
    rule_fns.deserialize(ctx, message)?;
    ctx.commands.entity(entity.id()).insert(ReplacedComponent);

    Ok(())
}
