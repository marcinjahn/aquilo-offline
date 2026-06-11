use anyhow::Context;
use tracing_subscriber::EnvFilter;

use aquilo_server::config::Config;
use aquilo_server::onboard::{self, LearnOptions, OnboardOptions, DEFAULT_UPSTREAM_HOST};

fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse(std::env::args().skip(1))?;

    // The broker/proxy threads + tokio runtime do the long-running work; build the
    // runtime here rather than via the macro so `main` stays plain.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match args.mode.as_str() {
        "serve" => {
            let cfg = Config::load(&args.config_path)
                .with_context(|| format!("loading config from {}", args.config_path))?;
            tracing::info!(
                receiver_id = %cfg.receiver_id,
                listen_port = cfg.listen_port,
                "starting aquilo-server (serve)"
            );
            rt.block_on(aquilo_server::server::run(cfg))
        }
        "learn" => rt.block_on(onboard::run_learn(args.learn_options())),
        "observe" => rt.block_on(onboard::run_observe(args.onboard_options())),
        other => anyhow::bail!("unknown mode '{other}'; expected serve, learn, or observe"),
    }
}

fn init_tracing() {
    // Keep our own logs at info; quiet rumqttd's per-packet routing spans, but
    // keep its server logs (device accept/connect) which are useful diagnostics.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,aquilo_server=info,rumqttd::router=warn")
    });
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Parsed command line. `serve` reads `--config`; `learn`/`observe` write it (and
/// the initial state into `--data-dir`) and accept capture/proxy options.
struct Args {
    mode: String,
    /// Config path: the input for `serve`, the output for `learn`/`observe`.
    config_path: String,
    data_dir: String,
    bind_addr: String,
    listen_port: u16,
    upstream_host: String,
    upstream_port: u16,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Args> {
        let mut a = Args {
            mode: "serve".to_string(),
            config_path: std::env::var("AQUILO_CONFIG").unwrap_or_else(|_| "config.toml".into()),
            data_dir: "data".to_string(),
            bind_addr: "0.0.0.0".to_string(),
            listen_port: 1883,
            upstream_host: std::env::var("UPSTREAM_HOST")
                .unwrap_or_else(|_| DEFAULT_UPSTREAM_HOST.into()),
            upstream_port: 1883,
        };

        let mut it = args;
        while let Some(arg) = it.next() {
            let mut next = |flag: &str| {
                it.next()
                    .with_context(|| format!("{flag} requires an argument"))
            };
            match arg.as_str() {
                "-c" | "--config" => a.config_path = next("--config")?,
                "--data-dir" => a.data_dir = next("--data-dir")?,
                "--bind" => a.bind_addr = next("--bind")?,
                "--listen-port" => a.listen_port = next("--listen-port")?.parse()?,
                "--upstream-host" => a.upstream_host = next("--upstream-host")?,
                "--upstream-port" => a.upstream_port = next("--upstream-port")?.parse()?,
                flag if flag.starts_with('-') => anyhow::bail!("unknown flag '{flag}'"),
                positional => a.mode = positional.to_string(),
            }
        }
        Ok(a)
    }

    fn onboard_options(&self) -> OnboardOptions {
        OnboardOptions {
            bind_addr: self.bind_addr.clone(),
            listen_port: self.listen_port,
            out_config: self.config_path.clone(),
            data_dir: self.data_dir.clone(),
        }
    }

    fn learn_options(&self) -> LearnOptions {
        LearnOptions {
            onboard: self.onboard_options(),
            upstream_host: self.upstream_host.clone(),
            upstream_port: self.upstream_port,
        }
    }
}
