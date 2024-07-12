use std::io::Result;

use which::which;

fn main() -> Result<()> {
    let builder = tonic_build::configure().protoc_arg("--experimental_allow_proto3_optional");

    match which("protoc") {
        Ok(_) => {
            println!("found protoc");
            builder.compile(&["src/message.proto"], &["src/"])?;
        }
        Err(_) => {
            println!("since there is no protoc in the path, we skip the protoc run");
            println!("protoc --descriptor_set_out=message.bin --proto_path=. message.proto");
            builder
                .skip_protoc_run()
                .file_descriptor_set_path("src/message.bin")
                .compile(&["src/message.bin"], &["src/"])?;
        }
    }

    Ok(())
}
