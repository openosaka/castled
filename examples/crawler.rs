use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    vec,
};

use async_shutdown::ShutdownManager;
use bytes::Bytes;
use castled::{client::tunnel::new_http_tunnel, util};
use serde::Deserialize;
use tokio::process::Child;
use url::form_urlencoded;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

const RUN_KUBECTL: &str = "KUBECTL_BIN";
const CLUSTER_ENTRYPOINT_PORT: u16 = 8080;
const CASTLED_CONTROL_PORT: u16 = 6610;

fn request_url(path: &str) -> String {
    format!("http://localhost:{}/{}", CLUSTER_ENTRYPOINT_PORT, path)
}

#[derive(Deserialize, PartialEq, Eq, Debug)]
struct Args {
    args: HashMap<String, String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut _child = init().await?;

    let external_url = "https://httpbin.org/get?foo=bar";
    let client = reqwest::Client::new();

    // # Test external URL
    let response = client.get(external_url).send().await?;
    assert_eq!(200, response.status());
    let args = response.json::<Args>().await?;

    let mut proxy_url = "crawler/proxy-request?requestTo=".to_owned();
    proxy_url
        .push_str(&form_urlencoded::byte_serialize(external_url.as_bytes()).collect::<String>());

    // # Test get same response from proxy
    let response2 = client.get(request_url(&proxy_url)).send().await?;
    assert_eq!(200, response2.status());
    let args2 = response2.json::<Args>().await?;

    assert_eq!(args, args2);

    // # Test request to the in-memory server through the proxy to tunnel
    // ## start in-memory server
    let in_memory_server = MockServer::start().await;
    let mock = Mock::given(method("GET"))
        .and(path("/foo"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello world"));
    in_memory_server.register(mock).await;

    // ## request to the in-memory server directly
    let response1 = client
        .get(format!("{}/foo", in_memory_server.uri()))
        .send()
        .await?;
    assert_eq!(200, response1.status());
    let response1_text = response1.text().await?;
    assert_eq!("hello world", response1_text);

    // ## request to the in-memory server through the proxy
    // as you know, the proxy deployed in the cluster can't access the in-memory server directly
    // ### register http tunnel
    let castled_client = castled::client::Client::new(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        CASTLED_CONTROL_PORT,
    ));
    let mock_host = "mock.test";
    let shutdown = ShutdownManager::new();
    let _ = castled_client
        .start_tunnel(
            new_http_tunnel(
                "foo".to_string(),
                in_memory_server.address().to_owned(),
                Bytes::from(mock_host),
                Bytes::from(""),
                false,
                0,
            ),
            shutdown.wait_shutdown_triggered(),
        )
        .await?;
    // ### call the proxy
    let mut proxy_url = "crawler/proxy-request?requestTo=".to_owned();
    proxy_url.push_str(
        &form_urlencoded::byte_serialize(format!("http://castled.tunnel.svc:6611/foo").as_bytes())
            .collect::<String>(),
    );
    let response2 = client
        .get(request_url(&proxy_url))
        .header("Host", mock_host)
        .send()
        .await?;
    assert_eq!(200, response2.status());
    let response2_text = response2.text().await?;
    assert_eq!(response1_text, response2_text);

    shutdown.trigger_shutdown(()).unwrap();
    Ok(())
}

async fn init() -> anyhow::Result<Vec<Child>> {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();

    let kubectl = std::env::var(RUN_KUBECTL)?;

    let child1 = util::port_forward(
        &kubectl,
        "gateway",
        "service/gateway-istio",
        80,
        CLUSTER_ENTRYPOINT_PORT,
    )
    .await?;
    let child2 = util::port_forward(
        &kubectl,
        "tunnel",
        "service/castled",
        CASTLED_CONTROL_PORT,
        CASTLED_CONTROL_PORT,
    )
    .await?;

    Ok(vec![child1, child2])
}
