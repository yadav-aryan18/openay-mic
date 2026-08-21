//! RFCOMM SPP-style server (feature `bluetooth`).
//!
//! Registers a Bluetooth Serial Port Profile (SPP) service with BlueZ and
//! yields accepted RFCOMM connections as generic byte streams. The streams
//! implement `tokio::io::AsyncRead + AsyncWrite`, so callers wrap them in
//! [`crate::tcp::TcpPacketStream`] to apply the same byte-stream framing and
//! bad-magic resync rules used by TCP.
//!
//! # Hardware testing
//!
//! This module compiles without any Bluetooth hardware, but *running* it
//! requires:
//!
//! - Linux with a system BlueZ D-Bus daemon (`bluetoothd`) and a `org.bluez`
//!   service (either a real adapter or BlueZ's `emulator` plugin)
//! - A paired peer device (e.g. an Android phone) that connects to the
//!   published SPP service
//!
//! The `rfcomm_adapter_presence` integration test (ignored by default) checks
//! adapter presence and prints `SKIP` when no adapter is available.

use std::pin::Pin;
use std::task::Context;

use bluer::rfcomm::{Profile, ProfileHandle, Role, Stream};
use bluer::{Error, ErrorKind, Session, Uuid};
// Trait import only (name kept private to avoid clashing with rfcomm::Stream).
use futures_core::Stream as _;

use crate::tcp::TcpPacketStream;

/// Serial Port Profile (SPP) service class UUID:
/// `00001101-0000-1000-8000-00805f9b34fb`.
pub const SPP_UUID: Uuid = Uuid::from_u128(0x0000_1101_0000_1000_8000_0080_5f9b_34fb);

/// Optional RFCOMM channel hint. `None` lets BlueZ pick one and publish it in
/// the SDP record (most discoverable). Fix a channel `1..=30` to allow
/// out-of-band connection attempts.
pub const CHANNEL: Option<u16> = None;

/// A registered SPP service on a specific adapter.
///
/// Keeps the [`Session`] and [`ProfileHandle`] alive; dropping the server
/// unregisters the profile with BlueZ.
pub struct RfcommServer {
    session: Session,
    handle: ProfileHandle,
}

impl RfcommServer {
    /// Register the SPP profile on the default adapter.
    pub async fn register_default() -> bluer::Result<Self> {
        let session = Session::new().await?;
        let adapter_name = session.default_adapter().await?.name().to_string();
        Self::register_on(session, &adapter_name).await
    }

    /// Register the SPP profile on a named adapter (e.g. `"hci0"`).
    pub async fn register_on_name(adapter_name: &str) -> bluer::Result<Self> {
        let session = Session::new().await?;
        Self::register_on(session, adapter_name).await
    }

    async fn register_on(session: Session, adapter_name: &str) -> bluer::Result<Self> {
        let adapter = session.adapter(adapter_name)?;
        if !adapter.is_powered().await.unwrap_or(false) {
            log::warn!("Bluetooth adapter {adapter_name} is not powered");
        }

        let profile = Profile {
            uuid: SPP_UUID,
            name: Some("OpenAY Mic SPP".to_string()),
            service: None,
            role: Some(Role::Server),
            channel: CHANNEL,
            psm: None,
            require_authentication: Some(false),
            require_authorization: Some(false),
            auto_connect: Some(false),
            service_record: None,
            version: Some(0x0102),
            features: None,
            _non_exhaustive: (),
        };
        let handle = session.register_profile(profile).await?;
        Ok(RfcommServer { session, handle })
    }

    /// Wait for the next incoming RFCOMM connection and accept it.
    ///
    /// Returns the accepted byte stream (`AsyncRead + AsyncWrite`). Wrap it
    /// in [`TcpPacketStream`] for OpenAY packet framing:
    ///
    /// ```ignore
    /// let stream = server.accept().await?;
    /// let mut frames = TcpPacketStream::new(stream);
    /// let pkt = frames.next_packet().await?;
    /// ```
    ///
    /// Errors when the profile has been unregistered or the D-Bus session
    /// died.
    pub async fn accept(&mut self) -> bluer::Result<Stream> {
        let req = self.next_request().await.ok_or_else(|| Error {
            kind: ErrorKind::NotRegistered,
            message: "OpenAY SPP profile is no longer registered".to_string(),
        })?;
        let device = req.device();
        let stream = req.accept()?;
        log::info!("Accepted RFCOMM connection from {device}");
        Ok(stream)
    }

    /// Convenience: accept one connection and immediately apply packet
    /// framing.
    pub async fn accept_packet_stream(&mut self) -> bluer::Result<TcpPacketStream<Stream>> {
        Ok(TcpPacketStream::new(self.accept().await?))
    }

    /// The D-Bus session (useful for adapter introspection).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Poll the `ProfileHandle` stream for the next connection request.
    async fn next_request(&mut self) -> Option<bluer::rfcomm::ConnectRequest> {
        let mut handle = Pin::new(&mut self.handle);
        std::future::poll_fn(move |cx: &mut Context<'_>| handle.as_mut().poll_next(cx)).await
    }
}
