use bevy::prelude::*;

use crate::core::ClientId;

/// Contains all connected clients.
///
/// Inserted as resource by [`ServerPlugin`](crate::server::ServerPlugin).
///
/// See also [ReplicatedClients](super::replicated_clients::ReplicatedClients).
#[derive(Resource, Default, Deref)]
pub struct ConnectedClients {
    #[deref]
    clients: Vec<ClientId>,
    replicate_after_connect: bool,
}

impl ConnectedClients {
    pub(crate) fn new(replicate_after_connect: bool) -> Self {
        Self {
            clients: Default::default(),
            replicate_after_connect,
        }
    }

    /// Returns if clients will automatically have replication enabled for them after they connect.
    pub fn replicate_after_connect(&self) -> bool {
        self.replicate_after_connect
    }

    pub(crate) fn add(&mut self, client_id: ClientId) {
        debug!("adding connected `{client_id:?}`");

        self.clients.push(client_id);
    }

    pub(crate) fn remove(&mut self, client_id: ClientId) {
        debug!("removing disconnected `{client_id:?}`");

        let index = self
            .clients
            .iter()
            .position(|test_id| *test_id == client_id)
            .unwrap_or_else(|| panic!("{client_id:?} should be added before removal"));
        self.clients.remove(index);
    }
}
