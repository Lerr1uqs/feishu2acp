use std::io::Result;

fn main() -> Result<()> {
    if let Ok(protoc) = protoc_bin_vendored::protoc_bin_path() {
        std::env::set_var("PROTOC", protoc);
    }
    prost_build::compile_protos(&["protos/pbbp2.proto"], &["protos/"])?;

    Ok(())
}
