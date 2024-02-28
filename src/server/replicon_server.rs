use bevy::{prelude::*, utils::HashMap};
use bytes::Bytes;

use crate::core::PeerId;

/// Stores information about server independent from messaging library.
///
/// Messaging library responsible for updating this resource:
/// - When server is started or stopped, [`Self::set_running`] should be used to reflect this.
/// - For sending messages [`Self::iter_sent_mut`] should be used to drain all sent messages.
/// Corresponding system should run in [`ServerSet::SendPackets`](super::ServerSet::SendPackets).
/// - For receiving messages [`Self::insert_received`] should be to used.
/// Corresponding system should run in [`ServerSet::ReceivePackets`](super::ServerSet::ReceivePackets).
#[derive(Resource, Default)]
pub struct RepliconServer {
    /// `true` if server is open for connections.
    ///
    /// By default set to `false`.
    running: bool,

    /// List of sent messages for each channel where top index is ID.
    ///
    /// Top index is channel ID.
    /// Inner [`Vec`] stores sent messages since the last tick.
    sent_messages: Vec<Vec<(PeerId, Bytes)>>,

    /// List of received messages for each channel where top index is ID.
    ///
    /// Top index is channel ID.
    /// Inner hash map stores sent messages since the last tick.
    ///
    /// Unlike in `sent_messages`, we use a hash map here for quick access
    /// to messages for a client from other system.
    received_messages: Vec<HashMap<PeerId, Vec<Bytes>>>,

    /// [`Vec`]'s from disconnected clients.
    ///
    /// All data is cleared before the insertion.
    /// Stored to reuse allocated capacity.
    client_buffer: Vec<Vec<Bytes>>,
}

impl RepliconServer {
    /// Changes the size of the message storage according to the number of channels.
    pub(super) fn setup_channels(
        &mut self,
        server_channels_count: usize,
        client_channels_count: usize,
    ) {
        self.sent_messages.resize(server_channels_count, Vec::new());
        self.received_messages
            .resize(client_channels_count, HashMap::new());
    }

    /// Initializes a message storage for a client.
    ///
    /// Reuses the memory from previously disconnected clients if available.
    pub(super) fn add_client(&mut self, peer_id: PeerId) {
        for channel_messages in &mut self.received_messages {
            let client_messages = self.client_buffer.pop().unwrap_or_default();
            channel_messages.insert(peer_id, client_messages);
        }
    }

    /// Removes a disconnected client.
    ///
    /// Keeps allocated memory for reuse.
    pub(super) fn remove_client(&mut self, peer_id: PeerId) {
        for channel_messages in &mut self.sent_messages {
            channel_messages.retain(|&(sender_id, _)| sender_id != peer_id);
        }

        for channel_messages in &mut self.received_messages {
            let mut client_messages = channel_messages
                .remove(&peer_id)
                .unwrap_or_else(|| panic!("{peer_id:?} should be added before removal"));
            client_messages.clear();
            self.client_buffer.push(client_messages);
        }
    }

    /// Receives a message from a client over a channel.
    pub fn receive<I: Into<u8>>(&mut self, peer_id: PeerId, channel_id: I) -> Option<Bytes> {
        let channel_id = channel_id.into();
        let channel_messages = self
            .received_messages
            .get_mut(channel_id as usize)
            .unwrap_or_else(|| panic!("server should have a receive channel with id {channel_id}"));

        channel_messages.get_mut(&peer_id)?.pop()
    }

    /// Sends a message to a client over a channel.
    pub fn send<I: Into<u8>, B: Into<Bytes>>(
        &mut self,
        peer_id: PeerId,
        channel_id: I,
        message: B,
    ) {
        let channel_id = channel_id.into();
        let channel_messages = self
            .sent_messages
            .get_mut(channel_id as usize)
            .unwrap_or_else(|| panic!("server should have a send channel with id {channel_id}"));

        channel_messages.push((peer_id, message.into()));
    }

    /// Marks the server as running or stopped.
    ///
    /// Should be called only from messaging library when the server changes its state.
    pub fn set_running(&mut self, running: bool) {
        if !running {
            for channel_messages in &mut self.sent_messages {
                channel_messages.clear();
            }
            for channel_messages in &mut self.received_messages {
                self.client_buffer
                    .extend(channel_messages.drain().map(|(_, messages)| messages));
            }
        }

        self.running = running;
    }

    /// Returns `true` if the server is running.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Returns iterator over all messages for each channel.
    ///
    /// Should be called only from messaging library.
    pub fn iter_sent_mut(&mut self) -> impl Iterator<Item = (u8, &mut Vec<(PeerId, Bytes)>)> + '_ {
        self.sent_messages
            .iter_mut()
            .enumerate()
            .map(|(channel_id, messages)| (channel_id as u8, messages))
    }

    /// Adds the message from the client to the list of received.
    ///
    /// Should be called only from messaging library.
    pub fn insert_received<I: Into<u8>, B: Into<Bytes>>(
        &mut self,
        peer_id: PeerId,
        message: B,
        channel_id: I,
    ) {
        let channel_id = channel_id.into();
        let channel_messages = self
            .received_messages
            .get_mut(channel_id as usize)
            .unwrap_or_else(|| panic!("server should have a receive channel with id {channel_id}"));

        let client_messages = channel_messages
            .get_mut(&peer_id)
            .unwrap_or_else(|| panic!("{peer_id:?} should be connected to send messages"));
        client_messages.push(message.into());
    }
}
