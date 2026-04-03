fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&["proto/livestream.proto"], &["proto"])?;

    println!("cargo:rerun-if-changed=proto/");
    Ok(())
}
