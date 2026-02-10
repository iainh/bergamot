use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "bergamot", version, about = "Usenet downloader")]
pub struct Cli {
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[arg(short = 'D', long, help = "Run in foreground (do not daemonize)")]
    pub foreground: bool,

    #[arg(long, value_name = "FILE")]
    pub pidfile: Option<PathBuf>,

    #[arg(
        short,
        long,
        default_value = "info",
        help = "Log level (debug, info, warning, error)"
    )]
    pub log_level: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_defaults() {
        let cli = Cli::try_parse_from(["bergamot"]).expect("parse");
        assert!(cli.config.is_none());
        assert!(!cli.foreground);
        assert_eq!(cli.log_level, "info");
    }

    #[test]
    fn cli_parses_config_path() {
        let cli =
            Cli::try_parse_from(["bergamot", "--config", "/etc/bergamot.conf"]).expect("parse");
        assert_eq!(cli.config.unwrap(), PathBuf::from("/etc/bergamot.conf"));
    }

    #[test]
    fn cli_parses_short_flags() {
        let cli =
            Cli::try_parse_from(["bergamot", "-c", "/etc/bergamot.conf", "-D", "-l", "debug"])
                .expect("parse");
        assert_eq!(cli.config.unwrap(), PathBuf::from("/etc/bergamot.conf"));
        assert!(cli.foreground);
        assert_eq!(cli.log_level, "debug");
    }

    #[test]
    fn cli_parses_pidfile_option() {
        let cli =
            Cli::try_parse_from(["bergamot", "--pidfile", "/var/run/bergamot.pid"]).expect("parse");
        assert_eq!(cli.pidfile.unwrap(), PathBuf::from("/var/run/bergamot.pid"));
    }

    #[test]
    fn cli_pidfile_defaults_to_none() {
        let cli = Cli::try_parse_from(["bergamot"]).expect("parse");
        assert!(cli.pidfile.is_none());
    }
}
