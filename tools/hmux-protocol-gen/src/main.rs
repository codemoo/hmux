//! Developer-only generator. protoc is not a gateway/Home runtime dependency.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .ok_or("output directory required")?;
    std::fs::create_dir_all(&output)?;
    let mut config = prost_build::Config::new();
    config.out_dir(output).bytes(["."]).skip_debug(["."]);
    // Large control trees must not inflate every terminal frame's enum.
    config
        .boxed(".hmux.v2.Envelope.body.request")
        .boxed(".hmux.v2.Envelope.body.catalog")
        .boxed(".hmux.v2.Envelope.body.usage")
        .boxed(".hmux.v2.Response.result.conversation")
        .boxed(".hmux.v2.Response.result.workspace")
        .boxed(".hmux.v2.Response.result.providers")
        .boxed(".hmux.v2.Response.result.staged");
    config.compile_protos(&["proto/hmux/v2/home.proto"], &["proto"])?;
    Ok(())
}
