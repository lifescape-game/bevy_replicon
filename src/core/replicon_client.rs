use bevy::prelude::*;
use bytes::Bytes;

use crate::core::ClientId;

/// Stores information about a client independent from the messaging backend.
///
/// The messaging backend is responsible for updating this resource:
/// - When the messaging client changes its status (connected, connecting and disconnected),
///   [`Self::set_status`] should be used to reflect this.
/// - For receiving messages, [`Self::insert_received`] should be to used.
///   A system to forward backend messages to Replicon should run in
///   [`ClientSet::ReceivePackets`](crate::client::ClientSet::ReceivePackets).
/// - For sending messages, [`Self::drain_sent`] should be used to drain all sent messages.
///   A system to forward Replicon messages to the backend should run in
///   [`ClientSet::SendPackets`](crate::client::ClientSet::SendPackets).
///
/// Inserted as resource by [`ClientPlugin`](crate::client::ClientPlugin).
#[derive(Resource, Default)]
pub struct RepliconClient {
    /// Client connection status.
    status: RepliconClientStatus,

    /// List of received messages for each channel.
    ///
    /// Top index is channel ID.
    /// Inner [`Vec`] stores received messages since the last tick.
    received_messages: Vec<Vec<Bytes>>,

    /// List of sent messages and their channels since the last tick.
    sent_messages: Vec<(u8, Bytes)>,

    rtt: f64,
    packet_loss: f64,
    sent_bps: f64,
    received_bps: f64,
}

impl RepliconClient {
    /// Changes the size of the receive messages storage according to the number of server channels.
    pub(crate) fn setup_server_channels(&mut self, channels_count: usize) {
        self.received_messages.resize(channels_count, Vec::new());
    }

    /// Returns number of received messages for a channel.
    ///
    /// See also [`Self::receive`].
    pub(crate) fn received_count<I: Into<u8>>(&self, channel_id: I) -> usize {
        let channel_id = channel_id.into();
        let channel_messages = self
            .received_messages
            .get(channel_id as usize)
            .unwrap_or_else(|| panic!("client should have a receive channel with id {channel_id}"));

        channel_messages.len()
    }

    /// Receives all available messages from the server over a channel.
    ///
    /// All messages will be drained.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn receive<I: Into<u8>>(&mut self, channel_id: I) -> impl Iterator<Item = Bytes> + '_ {
        if !self.is_connected() {
            // We can't return here because we need to return an empty iterator.
            warn!("trying to receive a message when the client is not connected");
        }

        let channel_id = channel_id.into();
        let channel_messages = self
            .received_messages
            .get_mut(channel_id as usize)
            .unwrap_or_else(|| panic!("client should have a receive channel with id {channel_id}"));

        trace!(
            "received {} message(s) totaling {} bytes from channel {channel_id}",
            channel_messages.len(),
            channel_messages
                .iter()
                .map(|bytes| bytes.len())
                .sum::<usize>()
        );

        channel_messages.drain(..)
    }

    /// Sends a message to the server over a channel.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn send<I: Into<u8>, B: Into<Bytes>>(&mut self, channel_id: I, message: B) {
        if !self.is_connected() {
            warn!("trying to send a message when the client is not connected");
            return;
        }

        let channel_id: u8 = channel_id.into();
        let message: Bytes = message.into();

        trace!("sending {} bytes over channel {channel_id}", message.len());

        self.sent_messages.push((channel_id, message));
    }

    /// Sets the client connection status.
    ///
    /// Discards all messages if the state changes from [`RepliconClientStatus::Connected`].
    /// See also [`Self::status`].
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend when the client status changes.
    ///
    /// </div>
    pub fn set_status(&mut self, status: RepliconClientStatus) {
        debug!("changing `RepliconClient` status to `{status:?}`");

        if self.is_connected() && !matches!(status, RepliconClientStatus::Connected { .. }) {
            for channel_messages in &mut self.received_messages {
                channel_messages.clear();
            }
            self.sent_messages.clear();

            self.rtt = 0.0;
            self.packet_loss = 0.0;
            self.sent_bps = 0.0;
            self.received_bps = 0.0;
        }

        self.status = status;
    }

    /// Returns the current client status.
    ///
    /// See also [`Self::set_status`].
    #[inline]
    pub fn status(&self) -> RepliconClientStatus {
        self.status
    }

    /// Returns `true` if the client is disconnected.
    ///
    /// See also [`Self::status`].
    #[inline]
    pub fn is_disconnected(&self) -> bool {
        matches!(self.status, RepliconClientStatus::Disconnected)
    }

    /// Returns `true` if the client is connecting.
    ///
    /// See also [`Self::status`].
    #[inline]
    pub fn is_connecting(&self) -> bool {
        matches!(self.status, RepliconClientStatus::Connecting)
    }

    /// Returns `true` if the client is connected.
    ///
    /// See also [`Self::status`].
    #[inline]
    pub fn is_connected(&self) -> bool {
        matches!(self.status, RepliconClientStatus::Connected { .. })
    }

    /// Returns the client's ID.
    ///
    /// The client ID is available only if the client state is [`RepliconClientStatus::Connected`].
    /// See also [`Self::status`].
    #[inline]
    pub fn id(&self) -> Option<ClientId> {
        if let RepliconClientStatus::Connected { client_id } = self.status {
            client_id
        } else {
            None
        }
    }

    /// Removes all sent messages, returning them as an iterator with channel.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn drain_sent(&mut self) -> impl Iterator<Item = (u8, Bytes)> + '_ {
        self.sent_messages.drain(..)
    }

    /// Adds a message from the server to the list of received messages.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn insert_received<I: Into<u8>, B: Into<Bytes>>(&mut self, channel_id: I, message: B) {
        if !self.is_connected() {
            warn!("trying to insert a received message when the client is not connected");
            return;
        }

        let channel_id = channel_id.into();
        let channel_messages = self
            .received_messages
            .get_mut(channel_id as usize)
            .unwrap_or_else(|| panic!("client should have a channel with id {channel_id}"));

        channel_messages.push(message.into());
    }

    /// Returns the round-time trip in seconds for the connection.
    ///
    /// Returns zero if not provided by the backend.
    pub fn rtt(&self) -> f64 {
        self.rtt
    }

    /// Sets the round-time trip in seconds for the connection.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn set_rtt(&mut self, rtt: f64) {
        self.rtt = rtt;
    }

    /// Returns the packet loss % for the connection.
    ///
    /// Returns zero if not provided by the backend.
    pub fn packet_loss(&self) -> f64 {
        self.packet_loss
    }

    /// Sets the packet loss % for the connection.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn set_packet_loss(&mut self, packet_loss: f64) {
        self.packet_loss = packet_loss;
    }

    /// Returns the bytes sent per second for the connection.
    ///
    /// Returns zero if not provided by the backend.
    pub fn sent_bps(&self) -> f64 {
        self.sent_bps
    }

    /// Sets the bytes sent per second for the connection.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn set_sent_bps(&mut self, sent_bps: f64) {
        self.sent_bps = sent_bps;
    }

    /// Returns the bytes received per second for the connection.
    ///
    /// Returns zero if not provided by the backend.
    pub fn received_bps(&self) -> f64 {
        self.received_bps
    }

    /// Sets the bytes received per second for the connection.
    ///
    /// <div class="warning">
    ///
    /// Should only be called from the messaging backend.
    ///
    /// </div>
    pub fn set_received_bps(&mut self, received_bps: f64) {
        self.received_bps = received_bps;
    }
}

/// Connection status of the [`RepliconClient`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum RepliconClientStatus {
    /// Not connected or trying to connect.
    #[default]
    Disconnected,
    /// Trying to connect to the server.
    Connecting,
    /// Connected to the server.
    ///
    /// Stores the assigned ID if one was assigned by the server.
    /// Needed only for users to access ID independent from messaging library.
    Connected { client_id: Option<ClientId> },
}
