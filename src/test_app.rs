use bevy::prelude::*;

use crate::{
    core::{
        replication::replicated_clients::ReplicatedClients,
        replicon_client::{RepliconClient, RepliconClientStatus},
        replicon_server::RepliconServer,
        ClientId, DisconnectReason,
    },
    server::{ClientConnected, ClientDisconnected},
};

/**
Extension for [`App`] to communicate with other instances like it's a server.

# Example

```
use bevy::prelude::*;
use bevy_replicon::{prelude::*, test_app::ServerTestAppExt};

let mut server_app = App::new();
let mut client_app = App::new();
for app in [&mut server_app, &mut client_app] {
    app.add_plugins((
        MinimalPlugins,
        // No messaging library plugin required.
        RepliconPlugins.set(ServerPlugin {
            tick_policy: TickPolicy::EveryFrame, // To tick each app update.
            ..Default::default()
        }),
    ))
    .finish();
}

// Simulate connection between two apps:
// - server app will register a connected client,
// - client app will be in connected state.
server_app.connect_client(&mut client_app);

server_app.world_mut().spawn(Replicated);

// Run tick for each app and trigger message exchange.
server_app.update();
server_app.exchange_with_client(&mut client_app);
client_app.update();

let mut relicated = client_app.world_mut().query::<&Replicated>();
assert_eq!(
    relicated.iter(client_app.world()).count(),
    1,
    "client should replicate spawned entity"
);

// You can optionally simulate a disconnect.
server_app.disconnect_client(&mut client_app);
```
**/
pub trait ServerTestAppExt {
    /// Starts server in [`self`] and connects a client app.
    ///
    /// Can be called multiple times on different client apps.
    /// Internally updates both apps one time.
    ///
    /// # Panics
    ///
    /// Panics if a client app has been connected before.
    fn connect_client(&mut self, client_app: &mut App);

    /// Disconnects a client app from [`self`].
    ///
    /// Can be called multiple times on different client apps.
    /// Internally updates both apps once.
    ///
    /// # Panics
    ///
    /// Panics if a client app hasn't been connected before.
    fn disconnect_client(&mut self, client_app: &mut App);

    /// Exchanges messages between client and server.
    ///
    /// Internally updates [`self`] before sending and updates the client app after receiving.
    ///
    /// # Panics
    ///
    /// Panics if a client app hasn't been connected before.
    fn exchange_with_client(&mut self, client_app: &mut App);
}

impl ServerTestAppExt for App {
    fn connect_client(&mut self, client_app: &mut App) {
        let mut client = client_app.world_mut().resource_mut::<RepliconClient>();
        assert!(
            client.is_disconnected(),
            "client can't be connected multiple times"
        );

        // Use client number as ID.
        // Server ID (0) will always be skipped.
        let max_id = self
            .world_mut()
            .resource_mut::<ReplicatedClients>()
            .iter_client_ids()
            .max()
            .unwrap_or(ClientId::SERVER);
        let client_id = ClientId::new(max_id.get() + 1);
        client.set_status(RepliconClientStatus::Connected {
            client_id: Some(client_id),
        });

        let mut server = self.world_mut().resource_mut::<RepliconServer>();
        server.set_running(true);

        self.world_mut().trigger(ClientConnected { client_id });

        self.update();
        client_app.update();
    }

    fn disconnect_client(&mut self, client_app: &mut App) {
        let mut client = client_app.world_mut().resource_mut::<RepliconClient>();
        let client_id = client
            .id()
            .expect("client should have an assigned ID for disconnect");

        client.set_status(RepliconClientStatus::Disconnected);

        self.world_mut().trigger(ClientDisconnected {
            client_id,
            reason: DisconnectReason::DisconnectedByServer,
        });

        self.update();
        client_app.update();
    }

    fn exchange_with_client(&mut self, client_app: &mut App) {
        let mut client = client_app.world_mut().resource_mut::<RepliconClient>();
        let client_id = client
            .id()
            .expect("client should have an assigned ID for exchanging messages");

        let mut server = self.world_mut().resource_mut::<RepliconServer>();
        for (channel_id, message) in client.drain_sent() {
            server.insert_received(client_id, channel_id, message)
        }

        server.retain_sent(|(sender_id, channel_id, message)| {
            if *sender_id == client_id {
                client.insert_received(*channel_id, message.clone());
                false
            } else {
                true
            }
        })
    }
}
