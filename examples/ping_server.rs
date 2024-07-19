use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::main]
async fn main() {
    let in_memory_server = MockServer::start().await;
    let mock = Mock::given(method("GET"))
        .and(path("/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"pong"));
    in_memory_server.register(mock).await;
    println!("ping server started at {}", in_memory_server.uri());
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    }
}
