use anyhow::Context;
use tracing_subscriber::EnvFilter;

use aquilo_server::config::Config;

fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse(std::env::args().skip(1))?;
    if args.mode != "serve" {
        anyhow::bail!("unknown mode '{}'; only 'serve' is supported", args.mode);
    }

    let cfg = Config::load(&args.config_path)
        .with_context(|| format!("loading config from {}", args.config_path))?;

    tracing::info!(
        receiver_id = %cfg.receiver_id,
        listen_port = cfg.listen_port,
        "starting aquilo-server (serve)"
    );

    // The broker thread + tokio runtime do the long-running work; build the
    // runtime here rather than via the macro so `main` stays plain.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(aquilo_server::server::run(cfg))
}

fn init_tracing() {
    // Keep our own logs at info; quiet rumqttd's per-packet routing spans, but
    // keep its server logs (device accept/connect) which are useful diagnostics.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,aquilo_server=info,rumqttd::router=warn")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

struct Args {
    mode: String,
    config_path: String,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Args> {
        let mut mode = "serve".to_string();
        let mut config_path =
            std::env::var("AQUILO_CONFIG").unwrap_or_else(|_| "config.toml".into());

        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-c" | "--config" => {
                    config_path = args.next().context("--config requires a path argument")?;
                }
                flag if flag.starts_with('-') => {
                    anyhow::bail!("unknown flag '{flag}'");
                }
                positional => mode = positional.to_string(),
            }
        }

        Ok(Args { mode, config_path })
    }
}
