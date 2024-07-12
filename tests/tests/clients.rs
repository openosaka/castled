use std::net::SocketAddr;

use bytes::Bytes;
use tests::{free_port, is_port_listening};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tunneld_client::{
    tunnel::{new_http_tunnel, new_tcp_tunnel},
    Client,
};
use tunneld_pkg::shutdown::ShutdownListener;
use tunneld_server::{Config, EntrypointConfig};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn init() {
    let _ = tracing_subscriber::fmt::try_init(); // the tracing subscriber is initialized only once
}

// after client registered a tunnel,
// close the client and server immediately by signal.
#[tokio::test]
async fn client_register_tcp() {
    init();
    let domain = "example.com";
    let server = start_server(vec![domain.to_string()]).await;
    let close_client = server.cancel.clone();
    let remote_port = free_port().unwrap();
    let control_addr = server.control_addr().clone();

    let client_handler = tokio::spawn(async move {
        let client = Client::new(control_addr).unwrap();
        let (handler, entrypoint_rx) = client
            .register_tunnel(
                new_tcp_tunnel(
                    "test".to_string(),
                    SocketAddr::from(([127, 0, 0, 1], 8971)), /* no matter */
                    remote_port,
                ),
                ShutdownListener::from_cancellation(close_client),
            )
            .unwrap();

        if let Err(err) = handler.await {
            return Err(err);
        }
        let entrypoint = entrypoint_rx.await.unwrap();
        assert_eq!(1, entrypoint.len());
        assert_eq!(entrypoint[0], format!("tcp://{}:{}", domain, remote_port));
        Ok(())
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
    let server = start_server(vec![]).await;

    let cancel_client_w = CancellationToken::new();
    let close_client = cancel_client_w.clone();
    let remote_port = free_port().unwrap();

    let control_addr = server.control_addr();
    let client_handler = tokio::spawn(async move {
        let client = Client::new(control_addr).unwrap();
        let (handler, _) = client
            .register_tunnel(
                new_tcp_tunnel(
                    "test".to_string(),
                    SocketAddr::from(([127, 0, 0, 1], 8971)),
                    remote_port,
                ),
                ShutdownListener::from_cancellation(close_client),
            )
            .unwrap();

        if let Err(err) = handler.await {
            return Err(err);
        }
        Ok(())
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
        let client = Client::new(control_addr).unwrap();
        let (handler, _) = client
            .register_tunnel(
                new_tcp_tunnel(
                    "test".to_string(),
                    SocketAddr::from(([127, 0, 0, 1], 8971)), /* no matter */
                    remote_port,
                ),
                ShutdownListener::from_cancellation(close_client),
            )
            .unwrap();

        if let Err(err) = handler.await {
            return Err(err);
        }
        Ok(())
    });

    tokio::spawn(async move {
        sleep(tokio::time::Duration::from_millis(300)).await;
        cancel_client_w.cancel();
    });

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

#[tokio::test]
async fn register_http_tunnel_with_subdomain() {
    let mock_local_server = MockServer::start().await;
    let local_port = {
        let uri = mock_local_server.uri();
        uri.split(":").last().unwrap().parse::<u16>().unwrap()
    };
    let mock_body = "Hello, world!";
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string(mock_body))
        .mount(&mock_local_server)
        .await;

    init();
    let server = start_server(vec![]).await;
    let close_client = server.cancel.clone();
    let control_addr = server.control_addr().clone();

    let client_handler = tokio::spawn(async move {
        let client = Client::new(control_addr).unwrap();
        let (handler, _) = client
            .register_tunnel(
                new_http_tunnel(
                    "test".to_string(),
                    SocketAddr::from(([127, 0, 0, 1], local_port)),
                    Bytes::from(""),
                    Bytes::from("foo"),
                    false,
                    0,
                ),
                ShutdownListener::from_cancellation(close_client),
            )
            .unwrap();

        if let Err(err) = handler.await {
            return Err(err);
        }
        Ok(())
    });

    sleep(tokio::time::Duration::from_millis(100)).await;

    let http_client = reqwest::Client::new();
    let request = http_client
        .request(
            http::method::Method::GET,
            format!("http://localhost:{}/hello", local_port),
        )
        .header("Host", "foo.example.com")
        .build()
        .unwrap();
    let response = http_client.execute(request).await.unwrap();
    assert_eq!(response.status(), 200);

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

async fn start_server(domain: Vec<String>) -> TestServer {
    let control_port = free_port().unwrap();
    let vhttp_port = free_port().unwrap();
    let server = tunneld_server::Server::new(Config {
        vhttp_port,
        control_port,
        entrypoint: EntrypointConfig {
            domain: domain,
            ..Default::default()
        },
    });
    let cancel_w = CancellationToken::new();
    let cancel = cancel_w.clone();
    tokio::spawn(async move {
        server.run(cancel.cancelled()).await;
    });
    sleep(tokio::time::Duration::from_millis(200)).await;

    TestServer {
        control_port,
        vhttp_port,
        cancel: cancel_w,
    }
}
