fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = "../../proto";
    println!("cargo:rerun-if-changed={proto_dir}/offline_maps.proto");
    let descriptors = protox::compile(["offline_maps.proto"], [proto_dir])?;
    prost_build::Config::new().compile_fds(descriptors)?;
    Ok(())
}
