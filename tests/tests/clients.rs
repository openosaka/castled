use std::net::SocketAddr;

use tests::{free_port, is_port_listening};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tunneld_client::Client;
use bytes::Bytes;
use wiremock::{matchers::{method, path}, Mock, MockServer, ResponseTemplate};

fn init() {
    let _ = tracing_subscriber::fmt::try_init(); // the tracing subscriber is initialized only once
}

// after client registered a tunnel,
// close the client and server immediately by signal.
#[tokio::test]
async fn client_register_tcp() {
    init();
    let server = start_server().await;
    let close_client = server.cancel.clone();
    let remote_port = free_port().unwrap();
    let control_addr = server.control_addr().clone();

    let client_handler = tokio::spawn(async move {
        let mut client = Client::new(&control_addr).unwrap();
        client.add_tcp_tunnel("test".to_string(), 1234 /* no matter */, remote_port);

        let client_exit = client.run(close_client).await;
        assert!(client_exit.is_ok());
    });

    // check the remote port is available
    sleep(tokio::time::Duration::from_millis(100)).await;
    assert!(
        is_port_listening(remote_port),
        "remote port {} is not listening",
        remote_port,
    );
    assert!(
        is_port_listening(server.vhttp_port),
        "vhttp port {} is not listening",
        server.vhttp_port,
    );

    server.cancel.cancel();

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

#[tokio::test]
async fn client_register_and_close_then_register_again() {
    init();
    let server = start_server().await;

    let cancel_client_w = CancellationToken::new();
    let close_client = cancel_client_w.clone();
    let remote_port = free_port().unwrap();

    let control_addr = server.control_addr().clone();
    let client_handler = tokio::spawn(async move {
        let mut client = Client::new(&control_addr).unwrap();
        client.add_tcp_tunnel("test".to_string(), 1234 /* no matter */, remote_port);

        sleep(tokio::time::Duration::from_millis(200)).await; // wait for server to start
        let client_exit = client.run(close_client).await;
        assert!(client_exit.is_ok());
    });

    tokio::spawn(async move {
        sleep(tokio::time::Duration::from_millis(300)).await;
        cancel_client_w.cancel();
    });

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());

    // register again with the same port
    let cancel_client_w = CancellationToken::new();
    let close_client = cancel_client_w.clone();
    let control_addr = server.control_addr().clone();
    let client_handler = tokio::spawn(async move {
        let mut client = Client::new(&control_addr).unwrap();
        client.add_tcp_tunnel("test".to_string(), 1234, remote_port);

        sleep(tokio::time::Duration::from_millis(200)).await; // wait for server to start
        let client_exit = client.run(close_client).await;
        assert!(client_exit.is_ok());
    });

    tokio::spawn(async move {
        sleep(tokio::time::Duration::from_millis(300)).await;
        cancel_client_w.cancel();
    });

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

#[tokio::test]
async fn register_http_tunnel() {
    let mock_local_server = MockServer::start().await;
    let local_port = {
        let uri = mock_local_server.uri();
        uri.split(":").last().unwrap().parse::<u16>().unwrap()
    };
    let mock_body = "Hello, world!";
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string(mock_body))
        .mount(&mock_local_server);
    
    init();
    let server = start_server().await;
    let close_client = server.cancel.clone();
    let remote_port = free_port().unwrap();
    let control_addr = server.control_addr().clone();
    
    let client_handler = tokio::spawn(async move {
        let mut client = Client::new(&control_addr).unwrap();
        client.add_http_tunnel("test".to_string(), local_port, remote_port, Bytes::from(""), Bytes::from(""));

        let client_exit = client.run(close_client).await;
        assert!(client_exit.is_ok());
    });

    sleep(tokio::time::Duration::from_millis(100)).await;

    server.cancel.cancel();

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

struct TestServer {
    control_port: u16,
    vhttp_port: u16,
    cancel: CancellationToken,
}

impl TestServer {
    fn control_addr(&self) -> SocketAddr {
        let addr_str = format!("127.0.0.1:{}", self.control_port);
        addr_str.parse().expect("Unable to parse socket address")
    }
}

async fn start_server() -> TestServer {
    let control_port = free_port().unwrap();
    let vhttp_port = free_port().unwrap();
    let mut server = tunneld_server::Server::new(tunneld_server::Config {
        control_port,
        vhttp_port,
        domain: "".to_string(),
    });
    let cancel_w = CancellationToken::new();
    let cancel = cancel_w.clone();
    tokio::spawn(async move {
        server.run(cancel).await;
    });
    sleep(tokio::time::Duration::from_millis(200)).await;

    TestServer {
        control_port,
        vhttp_port,
        cancel: cancel_w,
    }
}
