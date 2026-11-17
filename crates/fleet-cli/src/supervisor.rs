//! What `fleet up` tells the service manager that runs it: systemd's
//! `sd_notify` protocol, one datagram to the socket `NOTIFY_SOCKET` names
//! (sd_notify(3)). Without that variable it tells nothing.

pub enum Phase<'a> {
    Waiting(&'a str),
    Ready,
    Stopping,
}

pub fn tell(phase: Phase) {
    let message = match phase {
        Phase::Waiting(holder) => format!("STATUS=waiting for {holder}"),
        Phase::Ready => "READY=1\nSTATUS=ready: reconciling all Tasks".to_string(),
        Phase::Stopping => "STOPPING=1\nSTATUS=stopping".to_string(),
    };
    if let Err(e) = send(&message) {
        eprintln!("could not tell the service manager {message:?}: {e}");
    }
}

#[cfg(unix)]
fn send(message: &str) -> std::io::Result<()> {
    use std::os::unix::net::UnixDatagram;
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let sender = UnixDatagram::unbound()?;
    #[cfg(target_os = "linux")]
    if let Some(name) = socket.as_encoded_bytes().strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        let address = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
        return sender.send_to_addr(message.as_bytes(), &address).map(drop);
    }
    sender.send_to(message.as_bytes(), socket).map(drop)
}

#[cfg(not(unix))]
fn send(_message: &str) -> std::io::Result<()> {
    Ok(())
}
