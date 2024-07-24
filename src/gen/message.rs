// @generated
/// ControlCommand is the command sent by the server to the client  
/// which is used to notify the client to do something.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ControlCommand {
    #[prost(oneof="control_command::Payload", tags="1, 2")]
    pub payload: ::core::option::Option<control_command::Payload>,
}
/// Nested message and enum types in `ControlCommand`.
pub mod control_command {
    #[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Payload {
        #[prost(message, tag="1")]
        Init(super::InitPayload),
        #[prost(message, tag="2")]
        Work(super::WorkPayload),
    }
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InitPayload {
    #[prost(string, tag="1")]
    pub tunnel_id: ::prost::alloc::string::String,
    #[prost(string, repeated, tag="2")]
    pub assigned_entrypoint: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}
/// WorkPayload is sent when the server establishes a user connection.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WorkPayload {
    /// connection_id is the unique identifier of the connection which is assigned by the server.
    #[prost(string, tag="1")]
    pub connection_id: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TrafficToServer {
    /// when the user connects to the remote_port, the server assigns a unique_id to this connection.
    #[prost(string, tag="1")]
    pub connection_id: ::prost::alloc::string::String,
    /// status is the status of the traffic, when the traffic streaming starts,
    /// the first message should be the Start status without data,
    /// then the client sends the data with the Sending status with data,
    /// finally, after the local upstream finishes the response, the client sends the Finished status without data.
    #[prost(enumeration="traffic_to_server::Action", tag="2")]
    pub action: i32,
    /// data is the traffic data.
    #[prost(bytes="vec", tag="3")]
    pub data: ::prost::alloc::vec::Vec<u8>,
}
/// Nested message and enum types in `TrafficToServer`.
pub mod traffic_to_server {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum Action {
        /// start to send the traffic.
        /// the server expects only receiving `Start` action once.
        Start = 0,
        /// sending the traffic.
        Sending = 1,
        /// finish sending the traffic.
        /// the server expects only receiving `Finished` action once.
        Finished = 2,
        /// the client sends the Close action to tell the server to close the user connection. 
        /// generally, when something bad happens below:
        /// - can't dial the local upstream.
        Close = 3,
    }
    impl Action {
        /// String value of the enum field names used in the ProtoBuf definition.
        ///
        /// The values are not transformed in any way and thus are considered stable
        /// (if the ProtoBuf definition does not change) and safe for programmatic use.
        pub fn as_str_name(&self) -> &'static str {
            match self {
                Action::Start => "Start",
                Action::Sending => "Sending",
                Action::Finished => "Finished",
                Action::Close => "Close",
            }
        }
        /// Creates an enum from field names used in the ProtoBuf definition.
        pub fn from_str_name(value: &str) -> ::core::option::Option<Self> {
            match value {
                "Start" => Some(Self::Start),
                "Sending" => Some(Self::Sending),
                "Finished" => Some(Self::Finished),
                "Close" => Some(Self::Close),
                _ => None,
            }
        }
    }
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TrafficToClient {
    #[prost(bytes="vec", tag="1")]
    pub data: ::prost::alloc::vec::Vec<u8>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RegisterReq {
    #[prost(message, optional, tag="1")]
    pub tunnel: ::core::option::Option<Tunnel>,
}
/// Each tunnel is a bidirectional connection between the client and the server.
/// Basically, one tunnel corresponds to one http2 connection.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Tunnel {
    /// id is the unique identifier of the tunnel,
    /// it's assigned by the server.
    #[prost(string, tag="1")]
    pub id: ::prost::alloc::string::String,
    /// name is the name of the tunnel.
    #[prost(string, tag="2")]
    pub name: ::prost::alloc::string::String,
    #[prost(oneof="tunnel::Config", tags="3, 4, 5")]
    pub config: ::core::option::Option<tunnel::Config>,
}
/// Nested message and enum types in `Tunnel`.
pub mod tunnel {
    #[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Config {
        #[prost(message, tag="3")]
        Tcp(super::TcpConfig),
        #[prost(message, tag="4")]
        Http(super::HttpConfig),
        #[prost(message, tag="5")]
        Udp(super::UdpConfig),
    }
}
/// HttpConfig is used to tell the server how to create the http listener,
/// and how to route the request.
///
/// these three fields are exclusive.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct HttpConfig {
    /// the server's public domain is <https://castled.dev,>
    /// you may configure a subdomain <https://monitor.castled.dev,>
    /// then the request matches the subdomain will be forwarded to the related
    /// tunnel.
    ///
    /// if domain is not empty, the server will ignore the rest of the fields.
    #[prost(string, tag="1")]
    pub domain: ::prost::alloc::string::String,
    /// the server assigns <https://{subdomain}.{domain}> as the entrypoint for the
    /// tunnel.
    ///
    /// if subdomain is not empty, the server will ignore the rest of the fields.
    #[prost(string, tag="2")]
    pub subdomain: ::prost::alloc::string::String,
    /// if random_subdomain is true, the server will assign a random subdomain.
    #[prost(bool, tag="3")]
    pub random_subdomain: bool,
    /// random_port is the lowest priority option, the server will assign a random port
    /// if the remote_port is empty.
    /// the server will listen on the remote_port to accept the http request.
    #[prost(int32, tag="4")]
    pub remote_port: i32,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TcpConfig {
    /// if remote_port is empty, the server will assign a random port.
    #[prost(int32, tag="1")]
    pub remote_port: i32,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct UdpConfig {
    /// if remote_port is empty, the server will assign a random port.
    #[prost(int32, tag="1")]
    pub remote_port: i32,
}
include!("message.tonic.rs");
// @@protoc_insertion_point(module)