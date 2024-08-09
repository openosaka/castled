#![warn(missing_docs)]

mod common;

use crate::common::free_port;
use crate::common::is_port_listening;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use async_shutdown::ShutdownManager;
use castled::client::tunnel::HttpRemoteConfig;
use castled::client::tunnel::RemoteConfig;
use castled::{
    client::{tunnel::Tunnel, Client},
    server::{Config, EntrypointConfig, Server},
};
use http::HeaderValue;
use tokio::sync::oneshot;
use tokio::time::sleep;
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
    let server = start_server(EntrypointConfig {
        domain: vec![domain.to_string()],
        ..Default::default()
    })
    .await;
    let close_client = server.cancel.clone();
    let remote_port = free_port().unwrap();
    let control_addr = server.control_addr().clone();

    let client_handler = tokio::spawn(async move {
        let client = Client::new(control_addr).await.unwrap();
        let entrypoint = client
            .start_tunnel(
                Tunnel::new(
                    "test",
                    SocketAddr::from(([127, 0, 0, 1], 8971)), /* no matter */
                    RemoteConfig::Tcp(remote_port),
                ),
                close_client,
            )
            .await;
        assert!(entrypoint.is_ok());
        let entrypoint = entrypoint.unwrap();
        assert_eq!(1, entrypoint.len());
        assert_eq!(entrypoint[0], format!("tcp://{}:{}", domain, remote_port));
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

    server.cancel.trigger_shutdown(0).unwrap();

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

#[tokio::test]
async fn client_register_and_close_then_register_again() {
    init();
    let server = start_server(Default::default()).await;

    let shutdown = ShutdownManager::new();
    let close_client = shutdown.clone();
    let remote_port = free_port().unwrap();

    let control_addr = server.control_addr();
    let client_handler = tokio::spawn(async move {
        let client = Client::new(control_addr).await.unwrap();
        let _ = client
            .start_tunnel(
                Tunnel::new(
                    "test",
                    SocketAddr::from(([127, 0, 0, 1], 8971)),
                    RemoteConfig::Tcp(remote_port),
                ),
                close_client.clone(),
            )
            .await;
    });

    tokio::spawn(async move {
        sleep(tokio::time::Duration::from_millis(300)).await;
        shutdown.trigger_shutdown(0);
    });

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());

    // register again with the same port
    let shutdown = ShutdownManager::new();
    let close_client = shutdown.clone();
    let control_addr = server.control_addr().clone();
    let client_handler = tokio::spawn(async move {
        let client = Client::new(control_addr).await.unwrap();
        let _ = client
            .start_tunnel(
                Tunnel::new(
                    "test",
                    SocketAddr::from(([127, 0, 0, 1], 8971)), /* no matter */
                    RemoteConfig::Tcp(remote_port),
                ),
                close_client,
            )
            .await;
    });

    tokio::spawn(async move {
        sleep(tokio::time::Duration::from_millis(300)).await;
        shutdown.trigger_shutdown(0);
    });

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

#[tokio::test]
async fn register_http_tunnel_with_subdomain() {
    let mock_local_server = MockServer::start().await;
    let local_port = mock_local_server.address().port();
    let mock_body = "Hello, world!";
    let status_code = 201;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(
            ResponseTemplate::new(status_code)
                .set_body_string(mock_body)
                .insert_header("FOO", "BAR"),
        )
        .mount(&mock_local_server)
        .await;

    init();
    let server = start_server(Default::default()).await;
    let close_client = server.cancel.clone();
    let control_addr = server.control_addr().clone();

    let (wait_client_register, wait_client_register_rx) = oneshot::channel();
    let client_handler = tokio::spawn(async move {
        let client = Client::new(control_addr).await.unwrap();
        let _ = client
            .start_tunnel(
                Tunnel::new(
                    "test",
                    SocketAddr::from(([127, 0, 0, 1], local_port)),
                    RemoteConfig::Http(HttpRemoteConfig::Subdomain("foo")),
                ),
                close_client,
            )
            .await;
        wait_client_register.send(()).unwrap();
    });

    wait_client_register_rx.await.unwrap();

    let http_client = reqwest::Client::new();
    let request = http_client
        .request(
            http::method::Method::GET,
            format!("http://localhost:{}/hello", server.vhttp_port),
        )
        .header("Host", "foo.example.com")
        .build()
        .unwrap();
    let response = http_client.execute(request).await.unwrap();
    assert_eq!(response.status(), status_code);
    assert_eq!(
        response.headers().get("FOO"),
        Some(&HeaderValue::from_static("BAR")),
    );
    assert_eq!(response.text().await.unwrap(), mock_body);

    server.cancel.trigger_shutdown(0).unwrap();

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

#[tokio::test]
async fn test_assigned_entrypoint() {
    struct TestTunnel {
        tunnel: Tunnel<'static>,
        expected: Vec<&'static str>,
    }

    struct Case {
        entrypoint: EntrypointConfig,
        tunnels: Vec<TestTunnel>,
    }

    let cases = vec![
        Case {
            entrypoint: Default::default(),
            tunnels: vec![
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Tcp(8080),
                    ),
                    expected: vec![],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Udp(8080),
                    ),
                    expected: vec![], // TODO(Sword): assert random port
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::RandomPort),
                    ),
                    expected: vec![],
                },
            ],
        },
        Case {
            entrypoint: EntrypointConfig {
                domain: vec!["example.com".to_string()],
                ..Default::default()
            },
            tunnels: vec![
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Tcp(8080),
                    ),
                    expected: vec!["tcp://example.com:8080"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Udp(8080),
                    ),
                    expected: vec!["udp://example.com:8080"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Port(9999)),
                    ),
                    expected: vec!["http://example.com:9999"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Domain("mydomain.com")),
                    ),
                    expected: vec!["http://mydomain.com"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Subdomain("foo")),
                    ),
                    expected: vec!["http://foo.example.com"],
                },
            ],
        },
        Case {
            entrypoint: EntrypointConfig {
                ip: vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))],
                ..Default::default()
            },
            tunnels: vec![
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Tcp(8080),
                    ),
                    expected: vec!["tcp://127.0.0.1:8080"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Udp(8080),
                    ),
                    expected: vec!["udp://127.0.0.1:8080"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Port(9999)),
                    ),
                    expected: vec!["http://127.0.0.1:9999"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Domain("mydomain.com")),
                    ),
                    expected: vec!["http://mydomain.com"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Subdomain("foo")),
                    ),
                    expected: vec![/* no assigned entrypoint, because server doesn't has a domain */],
                },
            ],
        },
        Case {
            entrypoint: EntrypointConfig {
                domain: vec!["example.com".to_string(), "foo.example.com".to_string()],
                ip: vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))],
                vhttp_behind_proxy_tls: true,
                ..Default::default()
            },
            tunnels: vec![
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Tcp(8080),
                    ),
                    expected: vec![
                        "tcp://127.0.0.1:8080",
                        "tcp://example.com:8080",
                        "tcp://foo.example.com:8080",
                    ],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8971)),
                        RemoteConfig::Udp(8080),
                    ),
                    expected: vec![
                        "udp://127.0.0.1:8080",
                        "udp://example.com:8080",
                        "udp://foo.example.com:8080",
                    ],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Port(9999)),
                    ),
                    expected: vec![
                        "http://127.0.0.1:9999",
                        "http://example.com:9999",
                        "http://foo.example.com:9999",
                    ],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Domain("mydomain.com")),
                    ),
                    expected: vec!["https://mydomain.com"],
                },
                TestTunnel {
                    tunnel: Tunnel::new(
                        "test",
                        SocketAddr::from(([127, 0, 0, 1], 8080)),
                        RemoteConfig::Http(HttpRemoteConfig::Subdomain("bar")),
                    ),
                    expected: vec!["https://bar.example.com", "https://bar.foo.example.com"],
                },
            ],
        },
    ];

    for case in cases {
        let server = start_server(case.entrypoint).await;

        for tunnel in case.tunnels {
            let shutdown = ShutdownManager::new();
            let close_client = shutdown.clone();
            let control_addr = server.control_addr();

            let client_handler = tokio::spawn(async move {
                let client = Client::new(control_addr).await.unwrap();
                let entrypoint = client.start_tunnel(tunnel.tunnel, close_client).await;
                assert!(entrypoint.is_ok());
                let entrypoint = entrypoint.unwrap();
                assert_eq!(entrypoint, tunnel.expected);
            });

            tokio::spawn(async move {
                sleep(tokio::time::Duration::from_millis(100)).await;
                shutdown.trigger_shutdown(0).unwrap();
            });

            let _ = tokio::join!(client_handler);
        }
    }
}

#[tokio::test]
async fn test_client_auto_close_when_server_crash() {
    init();
    let server = start_server(Default::default()).await;
    let control_addr = server.control_addr().clone();

    let client_handler = tokio::spawn(async move {
        sleep(tokio::time::Duration::from_millis(50)).await; // wait for server to start
        let shutdown = ShutdownManager::new();

        let client = Client::new(control_addr).await.unwrap();
        let _ = client
            .start_tunnel(
                Tunnel::new(
                    "test",
                    SocketAddr::from(([127, 0, 0, 1], 8971)),
                    RemoteConfig::Tcp(free_port().unwrap()),
                ),
                shutdown.clone(),
            )
            .await;

        shutdown.wait_shutdown_complete().await;
    });

    sleep(tokio::time::Duration::from_millis(200)).await;
    server.cancel.trigger_shutdown(0).unwrap();

    let client_exit = tokio::join!(client_handler);
    assert!(client_exit.0.is_ok());
}

struct TestServer {
    control_port: u16,
    vhttp_port: u16,
    cancel: ShutdownManager<i8>,
}

impl TestServer {
    fn control_addr(&self) -> SocketAddr {
        let addr_str = format!("127.0.0.1:{}", self.control_port);
        addr_str.parse().expect("Unable to parse socket address")
    }
}

async fn start_server(entrypoint_config: EntrypointConfig) -> TestServer {
    let shutdown = ShutdownManager::new();
    let control_port = free_port().unwrap();
    let vhttp_port = free_port().unwrap();
    let server = Server::new(
        Config {
            vhttp_port,
            control_port,
            entrypoint: entrypoint_config,
        },
        shutdown.clone(),
    );
    tokio::spawn(async move {
        server.run().await;
    });
    sleep(tokio::time::Duration::from_millis(20)).await;

    TestServer {
        control_port,
        vhttp_port,
        cancel: shutdown.clone(),
    }
}
