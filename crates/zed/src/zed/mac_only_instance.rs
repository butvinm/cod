use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    thread,
    time::Duration,
};

use sysinfo::System;

use release_channel::ReleaseChannel;

const LOCALHOST: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(10);
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(35);
const SEND_TIMEOUT: Duration = Duration::from_millis(20);
const USER_BLOCK: u16 = 100;
// Upstream Zed's four channels use 43737, 43837, 43937 and 44037 as their port base (plus the
// per-user offset below), so stock Zed, in any channel, never listens here. A Mac can have a
// stock Zed installed next to Cod (#23 AC-R2: a Cod build must never mistake a running stock
// Zed, or the other way around, for another instance of itself); the two must not share a port
// for the same user, whatever channel either happens to report itself as.
const COD_PORT_BASE: u16 = 44737;

fn port_base() -> u16 {
    // Cod ships as a single preview channel today, but the per-channel spacing is kept (not
    // collapsed to one constant) so a later Cod dev/nightly build still gets its own port,
    // the same way upstream Zed separates its channels.
    match *release_channel::RELEASE_CHANNEL {
        ReleaseChannel::Dev => COD_PORT_BASE,
        ReleaseChannel::Preview => COD_PORT_BASE + USER_BLOCK,
        ReleaseChannel::Stable => COD_PORT_BASE + (2 * USER_BLOCK),
        ReleaseChannel::Nightly => COD_PORT_BASE + (3 * USER_BLOCK),
    }
}

fn address() -> SocketAddr {
    // These port numbers are offset by the user ID to avoid conflicts between
    // different users on the same machine. In addition to that the ports for each
    // release channel are spaced out by 100 to avoid conflicts between different
    // users running different release channels on the same machine. This ends up
    // interleaving the ports between different users and different release channels.
    let port = port_base();
    let mut user_port = port;
    let mut sys = System::new_all();
    sys.refresh_all();
    if let Ok(current_pid) = sysinfo::get_current_pid()
        && let Some(uid) = sys
            .process(current_pid)
            .and_then(|process| process.user_id())
    {
        let uid_u32 = get_uid_as_u32(uid);
        // Ensure that the user ID is not too large to avoid overflow when
        // calculating the port number. This seems unlikely but it doesn't
        // hurt to be safe.
        let max_port = 65535;
        let max_uid: u32 = max_port - port as u32;
        let wrapped_uid: u16 = (uid_u32 % max_uid) as u16;
        user_port += wrapped_uid;
    }

    SocketAddr::V4(SocketAddrV4::new(LOCALHOST, user_port))
}

#[cfg(unix)]
fn get_uid_as_u32(uid: &sysinfo::Uid) -> u32 {
    *uid.clone()
}

#[cfg(windows)]
fn get_uid_as_u32(uid: &sysinfo::Uid) -> u32 {
    // Extract the RID which is an integer
    uid.to_string()
        .rsplit('-')
        .next()
        .and_then(|rid| rid.parse::<u32>().ok())
        .unwrap_or(0)
}

// "Cod", not "Zed Editor": a stock Zed's identical handshake text (over a different port,
// see COD_PORT_BASE above) must never be mistaken for Cod's own, or the other way around.
fn instance_handshake() -> &'static str {
    match *release_channel::RELEASE_CHANNEL {
        ReleaseChannel::Dev => "Cod Dev Instance Running",
        ReleaseChannel::Nightly => "Cod Nightly Instance Running",
        ReleaseChannel::Preview => "Cod Preview Instance Running",
        ReleaseChannel::Stable => "Cod Stable Instance Running",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsOnlyInstance {
    Yes,
    No,
}

pub fn ensure_only_instance() -> IsOnlyInstance {
    if check_got_handshake() {
        return IsOnlyInstance::No;
    }

    let listener = match TcpListener::bind(address()) {
        Ok(listener) => listener,

        Err(err) => {
            log::warn!("Error binding to single instance port: {err}");
            if check_got_handshake() {
                return IsOnlyInstance::No;
            }

            // Avoid failing to start when some other application by chance already has
            // a claim on the port. This is sub-par as any other instance that gets launched
            // will be unable to communicate with this instance and will duplicate
            log::warn!("Backup handshake request failed, continuing without handshake");
            return IsOnlyInstance::Yes;
        }
    };

    thread::Builder::new()
        .name("EnsureSingleton".to_string())
        .spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => return,
                };

                _ = stream.set_nodelay(true);
                _ = stream.set_read_timeout(Some(SEND_TIMEOUT));
                _ = stream.write_all(instance_handshake().as_bytes());
            }
        })
        .unwrap();

    IsOnlyInstance::Yes
}

fn check_got_handshake() -> bool {
    match TcpStream::connect_timeout(&address(), CONNECT_TIMEOUT) {
        Ok(mut stream) => {
            let mut buf = vec![0u8; instance_handshake().len()];

            stream.set_read_timeout(Some(RECEIVE_TIMEOUT)).unwrap();
            if let Err(err) = stream.read_exact(&mut buf) {
                log::warn!("Connected to single instance port but failed to read: {err}");
                return false;
            }

            if buf == instance_handshake().as_bytes() {
                log::info!("Got instance handshake");
                return true;
            }

            log::warn!("Got wrong instance handshake value");
            false
        }

        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Upstream Zed's four port bases (Dev, Preview, Stable, Nightly), kept here as plain
    // numbers rather than imported, so this test still catches it if a future edit of
    // port_base() accidentally goes back to COD_PORT_BASE = 43737 (#23 AC-R2).
    const STOCK_ZED_PORT_BASES: [u16; 4] = [43737, 43737 + 100, 43737 + 200, 43737 + 300];

    #[test]
    fn port_base_never_matches_a_stock_zed_channel() {
        let base = port_base();
        assert!(
            !STOCK_ZED_PORT_BASES.contains(&base),
            "Cod's port base {base} collides with a stock Zed channel {STOCK_ZED_PORT_BASES:?}"
        );
    }

    #[test]
    fn instance_handshake_never_matches_a_stock_zed_handshake() {
        let handshake = instance_handshake();
        assert!(
            handshake.starts_with("Cod "),
            "expected a Cod-specific handshake, got {handshake:?}"
        );
        assert!(!handshake.contains("Zed Editor"));
    }
}
