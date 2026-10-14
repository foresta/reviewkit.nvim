use crate::context::Context;

/// stdout is reserved for the JSON envelope, so logs go to stderr or a file.
pub fn init(ctx: &Context) {
    let mut builder = env_logger::Builder::new();
    builder.filter_level(ctx.log_level);
    builder.target(env_logger::Target::Stderr);
    if let Some(path) = &ctx.log_file {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => {
                builder.target(env_logger::Target::Pipe(Box::new(file)));
            }
            Err(err) => eprintln!(
                "reviewkit: cannot open --log-file {}: {err}; logging to stderr",
                path.display()
            ),
        }
    }
    // A second init (tests) is harmless.
    let _ = builder.try_init();
}
