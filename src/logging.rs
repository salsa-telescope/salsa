use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

pub fn setup_logging(journald_logging: bool) {
    if journald_logging {
        let journald_layer = tracing_journald::layer().expect("failed to open journald log");
        tracing_subscriber::registry()
            .with(journald_layer)
            .with(EnvFilter::from_default_env())
            .init();
    } else {
        fmt::init();
    }
}
