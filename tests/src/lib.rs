use std::net::{TcpListener, TcpStream};

/// free_port returns a free port number for testing.
pub fn free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

pub fn is_port_listening(port: u16) -> bool {
    match TcpStream::connect(("127.0.0.1", port)) {
        Ok(_) => true,
        Err(_) => false,
    }
}
