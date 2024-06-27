pub mod pb {
	tonic::include_proto!("message");
}

// before connectrpc releases rust version, we validate the message by ourselves
// refer: https://github.com/connectrpc
pub mod validate;
