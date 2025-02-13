use clap::{crate_version, App, Arg};

// error handling
use crate::{encrypted_sled::PasswordMethod, mnemonic::Cmd, TofndResult};
use anyhow::anyhow;

// TODO: move these into constants.rs
const DEFAULT_PATH_ROOT: &str = ".tofnd";
const TOFND_HOME_ENV_VAR: &str = "TOFND_HOME";
const DEFAULT_MNEMONIC_CMD: &str = "existing";
const DEFAULT_PORT: u16 = 50051;
const AVAILABLE_MNEMONIC_CMDS: [&str; 4] = ["existing", "create", "import", "export"];

#[cfg(feature = "malicious")]
mod malicious;
#[cfg(feature = "malicious")]
use malicious::*;

// TODO: move to types.rs
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub safe_keygen: bool,
    pub mnemonic_cmd: Cmd,
    pub tofnd_path: String,
    pub password_method: PasswordMethod,
    #[cfg(feature = "malicious")]
    pub behaviours: Behaviours,
}
impl Default for Config {
    fn default() -> Self {
        Config {
            port: DEFAULT_PORT,
            safe_keygen: true,
            mnemonic_cmd: Cmd::Existing,
            tofnd_path: DEFAULT_PATH_ROOT.to_string(),
            password_method: PasswordMethod::Prompt,
            #[cfg(feature = "malicious")]
            behaviours: Behaviours::default(),
        }
    }
}

pub fn parse_args() -> TofndResult<Config> {
    // need to use let to avoid dropping temporary value
    let port = &DEFAULT_PORT.to_string();

    let app = App::new("tofnd")
        .about("A threshold signature scheme daemon")
        .version(crate_version!())
        .arg(
            Arg::with_name("port")
                .long("port")
                .short("p")
                .required(false)
                .default_value(port),
        )
        .arg(
            // TODO: change to something like `--unsafe-primes`
            Arg::with_name("unsafe")
                .help(
                    "Use unsafe primes for generation of Pailler encryption keys. (default: deactivated) **Security warning:** This option is intented for use only in tests.  Do not use this option to secure real value.",
                )
                .long("unsafe")
                .required(false)
                .takes_value(false)
                .display_order(0),
        )
        .arg(
            Arg::with_name("no-password")
                .help(
                    "Skip providing a password. (default: disabled) **Security warning:** If this option is set then on-disk storage is encrypted with a default (and insecure) password.",
                )
                .long("no-password")
                .required(false)
                .takes_value(false)
                .display_order(0),
        )
        .arg(
            Arg::with_name("mnemonic")
                .long("mnemonic")
                .short("m")
                .required(false)
                .default_value(DEFAULT_MNEMONIC_CMD)
                .possible_values(&AVAILABLE_MNEMONIC_CMDS),
        )
        .arg(
            Arg::with_name("directory")
                .long("directory")
                .short("d")
                .required(false)
                .env(TOFND_HOME_ENV_VAR)
                .default_value(DEFAULT_PATH_ROOT),
        );

    #[cfg(feature = "malicious")]
    let app = app.subcommand(
        SubCommand::with_name("malicious")
            .about("Select malicious behaviour")
            .arg(
                Arg::with_name("behaviour")
                    .required(true)
                    .possible_values(&AVAILABLE_BEHAVIOURS)
                    .help("malicious behaviour"),
            )
            .arg(Arg::with_name("victim").required(true).help("victim")),
    );
    #[cfg(feature = "malicious")]
    let behaviours = get_behaviour_matches(app.clone())?;

    let matches = app.get_matches();

    let port = matches
        .value_of("port")
        .ok_or_else(|| anyhow!("port value"))?
        .parse::<u16>()?;
    let safe_keygen = !matches.is_present("unsafe");
    let mnemonic_cmd = matches
        .value_of("mnemonic")
        .ok_or_else(|| anyhow!("cmd value"))?
        .to_string();
    let mnemonic_cmd = Cmd::from_string(&mnemonic_cmd)?;
    let tofnd_path = matches
        .value_of("directory")
        .ok_or_else(|| anyhow!("directory value"))?
        .to_string();
    let password_method = match matches.is_present("no-password") {
        true => PasswordMethod::NoPassword,
        false => PasswordMethod::Prompt,
    };

    Ok(Config {
        port,
        safe_keygen,
        mnemonic_cmd,
        tofnd_path,
        password_method,
        #[cfg(feature = "malicious")]
        behaviours,
    })
}
