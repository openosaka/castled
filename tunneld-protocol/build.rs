use std::io::Result;

fn main() -> Result<()> {
    tonic_build::configure()
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile(&["src/message.proto"], &["src/"])?;
    Ok(())
}
