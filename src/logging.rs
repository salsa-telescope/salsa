use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

pub fn setup_logging(journald_logging: bool) {
    // refinery logs "current version: N" at info level, where N is the
    // database migration and not the release. `database::apply_migrations`
    // logs that information itself under a clearer name, so keep only
    // refinery's warnings and errors (e.g. a migration file that no longer
    // matches the applied one).
    let filter = EnvFilter::from_default_env().add_directive(
        "refinery_core=warn"
            .parse()
            .expect("Hardcoded filter directive should parse"),
    );
    if journald_logging {
        let journald_layer = tracing_journald::layer().expect("failed to open journald log");
        tracing_subscriber::registry()
            .with(journald_layer)
            .with(filter)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(fmt::layer())
            .with(filter)
            .init();
    }
}
