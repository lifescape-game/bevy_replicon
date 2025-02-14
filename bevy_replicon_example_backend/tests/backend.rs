use std::io;

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_example_backend::{ExampleClient, ExampleServer, RepliconExampleBackendPlugins};
use serde::{Deserialize, Serialize};

#[test]
fn connect_disconnect() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconExampleBackendPlugins,
        ))
        .finish();
    }

    setup(&mut server_app, &mut client_app).unwrap();

    assert!(server_app.world().resource::<RepliconServer>().is_running());

    let renet_server = server_app.world().resource::<ExampleServer>();
    assert_eq!(renet_server.connected_clients(), 1);

    let connected_clients = server_app.world().resource::<ConnectedClients>();
    assert_eq!(connected_clients.len(), 1);

    let replicon_client = client_app.world().resource::<RepliconClient>();
    assert!(replicon_client.is_connected());

    let renet_client = client_app.world().resource::<ExampleClient>();
    assert!(renet_client.is_connected());

    client_app.world_mut().remove_resource::<ExampleClient>();

    client_app.update();
    server_app.update();

    let connected_clients = server_app.world().resource::<ConnectedClients>();
    assert_eq!(connected_clients.len(), 0);

    let renet_server = server_app.world().resource::<ExampleServer>();
    assert_eq!(renet_server.connected_clients(), 0);

    let replicon_client = client_app.world().resource::<RepliconClient>();
    assert!(replicon_client.is_disconnected());

    server_app.world_mut().remove_resource::<ExampleServer>();

    server_app.update();

    assert!(!server_app.world().resource::<RepliconServer>().is_running());
}

#[test]
fn replication() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconExampleBackendPlugins,
        ))
        .finish();
    }

    setup(&mut server_app, &mut client_app).unwrap();

    server_app.world_mut().spawn(Replicated);

    server_app.update();
    client_app.update();

    client_app
        .world_mut()
        .query::<&Replicated>()
        .single(client_app.world());
}

#[test]
fn server_event() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconExampleBackendPlugins,
        ))
        .add_server_event::<DummyEvent>(ChannelKind::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app).unwrap();

    server_app.world_mut().send_event(ToClients {
        mode: SendMode::Broadcast,
        event: DummyEvent,
    });

    server_app.update();
    client_app.update();

    let dummy_events = client_app.world().resource::<Events<DummyEvent>>();
    assert_eq!(dummy_events.len(), 1);
}

#[test]
fn client_event() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconExampleBackendPlugins,
        ))
        .add_client_event::<DummyEvent>(ChannelKind::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app).unwrap();

    client_app.world_mut().send_event(DummyEvent);

    client_app.update();
    server_app.update();

    let client_events = server_app
        .world()
        .resource::<Events<FromClient<DummyEvent>>>();
    assert_eq!(client_events.len(), 1);
}

fn setup(server_app: &mut App, client_app: &mut App) -> io::Result<()> {
    let server_socket = ExampleServer::new(0)?;
    let server_addr = server_socket.local_addr()?;
    let client_socket = ExampleClient::new(server_addr.port())?;

    server_app.insert_resource(server_socket);
    client_app.insert_resource(client_socket);

    server_app.update();
    client_app.update();

    Ok(())
}

#[derive(Deserialize, Event, Serialize)]
struct DummyEvent;
