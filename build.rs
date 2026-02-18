fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .build_server(true) // We need server for Livestream
        .build_client(true) // We need client for LivestreamCallback
        .compile_protos(
            &["proto/livestream.proto", "proto/livestream_callback.proto"],
            &["proto"],
        )?;

    println!("cargo:rerun-if-changed=proto/");
    Ok(())
}
