use std::{
    io::Error as IoError,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket as StdUdpSocket},
    sync::Arc,
};

use bytes::Bytes;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::{
    net::UdpSocket,
    sync::{
        oneshot::{self, Sender},
        RwLock as AsyncRwLock,
    },
};
use tuic::Address;

use super::Connection;
use crate::{error::Error, CONFIG};

#[derive(Clone)]
pub struct UdpSession(Arc<UdpSessionInner>);

struct UdpSessionInner {
    assoc_id: u16,
    conn: Connection,
    socket_v4: UdpSocket,
    socket_v6: Option<UdpSocket>,
    close: AsyncRwLock<Option<Sender<()>>>,
}

impl UdpSession {
    pub fn new(conn: Connection, assoc_id: u16) -> Result<Self, Error> {
        let socket_v4 = {
            let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
                .map_err(|err| Error::Socket("failed to create UDP associate IPv4 socket", err))?;

            socket.set_nonblocking(true).map_err(|err| {
                Error::Socket(
                    "failed setting UDP associate IPv4 socket as non-blocking",
                    err,
                )
            })?;

            socket
                .bind(&SockAddr::from(SocketAddr::from((
                    Ipv4Addr::UNSPECIFIED,
                    0,
                ))))
                .map_err(|err| Error::Socket("failed to bind UDP associate IPv4 socket", err))?;

            UdpSocket::from_std(StdUdpSocket::from(socket))?
        };

        let socket_v6 = if CONFIG.udp_relay_ipv6 {
            let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))
                .map_err(|err| Error::Socket("failed to create UDP associate IPv6 socket", err))?;

            socket.set_nonblocking(true).map_err(|err| {
                Error::Socket(
                    "failed setting UDP associate IPv6 socket as non-blocking",
                    err,
                )
            })?;

            socket.set_only_v6(true).map_err(|err| {
                Error::Socket("failed setting UDP associate IPv6 socket as IPv6-only", err)
            })?;

            socket
                .bind(&SockAddr::from(SocketAddr::from((
                    Ipv6Addr::UNSPECIFIED,
                    0,
                ))))
                .map_err(|err| Error::Socket("failed to bind UDP associate IPv6 socket", err))?;

            Some(UdpSocket::from_std(StdUdpSocket::from(socket))?)
        } else {
            None
        };

        let (tx, rx) = oneshot::channel();

        let session = Self(Arc::new(UdpSessionInner {
            conn,
            assoc_id,
            socket_v4,
            socket_v6,
            close: AsyncRwLock::new(Some(tx)),
        }));

        let session_listening = session.clone();
        let listen = async move {
            loop {
                let (pkt, addr) = match session_listening.recv().await {
                    Ok(res) => res,
                    Err(err) => {
                        log::warn!(
                            "[{id:#010x}] [{addr}] [{user}] [packet] [{assoc_id:#06x}] outbound \
                             listening error: {err}",
                            id = session_listening.0.conn.id(),
                            addr = session_listening.0.conn.inner.remote_address(),
                            user = session_listening.0.conn.auth,
                        );
                        continue;
                    }
                };

                tokio::spawn(session_listening.0.conn.clone().relay_packet(
                    pkt,
                    Address::SocketAddress(addr),
                    session_listening.0.assoc_id,
                ));
            }
        };

        tokio::spawn(async move {
            tokio::select! {
                _ = listen => unreachable!(),
                _ = rx => {},
            }
        });

        Ok(session)
    }

    pub async fn send(&self, pkt: Bytes, addr: SocketAddr) -> Result<(), Error> {
        let socket = match addr {
            SocketAddr::V4(_) => &self.0.socket_v4,
            SocketAddr::V6(_) => self
                .0
                .socket_v6
                .as_ref()
                .ok_or_else(|| Error::UdpRelayIpv6Disabled(addr))?,
        };

        socket.send_to(&pkt, addr).await?;
        Ok(())
    }

    async fn recv(&self) -> Result<(Bytes, SocketAddr), IoError> {
        async fn recv(socket: &UdpSocket) -> Result<(Bytes, SocketAddr), IoError> {
            let mut buf = vec![0u8; CONFIG.max_external_packet_size];
            let (n, addr) = socket.recv_from(&mut buf).await?;
            buf.truncate(n);
            Ok((Bytes::from(buf), addr))
        }

        if let Some(socket_v6) = &self.0.socket_v6 {
            tokio::select! {
                res = recv(&self.0.socket_v4) => res,
                res = recv(socket_v6) => res,
            }
        } else {
            recv(&self.0.socket_v4).await
        }
    }

    pub async fn close(&self) {
        let _ = self.0.close.write().await.take().unwrap().send(());
    }
}
