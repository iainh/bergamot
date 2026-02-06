use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "nzbg", version, about = "Usenet downloader")]
pub struct Cli {
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[arg(short = 'D', long, help = "Run in foreground (do not daemonize)")]
    pub foreground: bool,

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
        let cli = Cli::try_parse_from(["nzbg"]).expect("parse");
        assert!(cli.config.is_none());
        assert!(!cli.foreground);
        assert_eq!(cli.log_level, "info");
    }

    #[test]
    fn cli_parses_config_path() {
        let cli = Cli::try_parse_from(["nzbg", "--config", "/etc/nzbg.conf"]).expect("parse");
        assert_eq!(cli.config.unwrap(), PathBuf::from("/etc/nzbg.conf"));
    }

    #[test]
    fn cli_parses_short_flags() {
        let cli = Cli::try_parse_from(["nzbg", "-c", "/etc/nzbg.conf", "-D", "-l", "debug"])
            .expect("parse");
        assert_eq!(cli.config.unwrap(), PathBuf::from("/etc/nzbg.conf"));
        assert!(cli.foreground);
        assert_eq!(cli.log_level, "debug");
    }
}
