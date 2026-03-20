use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerConfig {
    pub port: u16,
    pub bind: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthConfig {
    pub psk: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TerminalConfig {
    pub scrollback_lines: u32,
    pub max_agents: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BeadsConfig {
    pub project_dir: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub terminal: TerminalConfig,
    pub beads: BeadsConfig,
    pub tools: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        let psk = generate_psk();
        let mut tools = HashMap::new();
        tools.insert("claude-code".to_string(), "claude".to_string());
        tools.insert("copilot".to_string(), "gh copilot suggest".to_string());

        Config {
            server: ServerConfig {
                port: 9800,
                bind: "0.0.0.0".to_string(),
            },
            auth: AuthConfig { psk },
            terminal: TerminalConfig {
                scrollback_lines: 1000,
                max_agents: 8,
            },
            beads: BeadsConfig {
                project_dir: ".".to_string(),
            },
            tools,
        }
    }
}

/// Returns the platform-appropriate config file path.
pub fn config_path() -> PathBuf {
    let base = dirs::config_dir().expect("cannot determine config directory");
    base.join("dispatch").join("config.toml")
}

/// Generates a 24-character hex PSK.
fn generate_psk() -> String {
    let bytes: Vec<u8> = (0..12).map(|_| rand::thread_rng().gen()).collect();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Loads config from disk, creating it with defaults if absent.
pub fn load_or_create() -> Config {
    let path = config_path();

    if path.exists() {
        let raw = fs::read_to_string(&path).expect("failed to read config");
        toml::from_str(&raw).expect("invalid config.toml")
    } else {
        let cfg = Config::default();
        let dir = path.parent().unwrap();
        fs::create_dir_all(dir).expect("failed to create config directory");
        let raw = to_toml_with_comments(&cfg);
        fs::write(&path, &raw).expect("failed to write config");
        cfg
    }
}

/// Overwrites the PSK with a freshly generated one and saves.
pub fn regenerate_psk() -> String {
    let path = config_path();
    let mut cfg = load_or_create();
    cfg.auth.psk = generate_psk();
    let raw = to_toml_with_comments(&cfg);
    fs::write(&path, &raw).expect("failed to write config");
    cfg.auth.psk
}

/// TLS certificate and key paths within the config directory.
pub struct TlsIdentity {
    pub acceptor: tokio_rustls::TlsAcceptor,
    /// SHA-256 fingerprint of the DER-encoded certificate (lowercase hex).
    pub fingerprint: String,
}

/// Ensures a self-signed TLS certificate exists in the config directory and
/// returns a TlsAcceptor ready for use. Generates cert + key on first run.
pub fn load_or_create_tls() -> TlsIdentity {
    let base = dirs::config_dir().expect("cannot determine config directory");
    let dir = base.join("dispatch");
    fs::create_dir_all(&dir).expect("failed to create config directory");

    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");

    // Generate self-signed cert if missing.
    if !cert_path.exists() || !key_path.exists() {
        let certified = rcgen::generate_simple_self_signed(vec![
            "dispatch.local".to_string(),
            "localhost".to_string(),
        ])
        .expect("failed to generate TLS certificate");
        fs::write(&cert_path, certified.cert.der()).expect("failed to write cert.der");
        fs::write(&key_path, certified.key_pair.serialize_der())
            .expect("failed to write key.der");
    }

    let cert_der = fs::read(&cert_path).expect("failed to read cert.der");
    let key_der = fs::read(&key_path).expect("failed to read key.der");

    let certs = vec![rustls::pki_types::CertificateDer::from(cert_der.clone())];
    let key = rustls::pki_types::PrivateKeyDer::from(
        rustls::pki_types::PrivatePkcs8KeyDer::from(key_der),
    );

    // Compute SHA-256 fingerprint of the DER certificate.
    let fingerprint = {
        let mut hasher = Sha256::new();
        hasher.update(&cert_der);
        let hash = hasher.finalize();
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            certs,
            rustls::pki_types::PrivateKeyDer::from(key),
        )
        .expect("invalid TLS certificate/key");

    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    TlsIdentity {
        acceptor,
        fingerprint,
    }
}

/// Serialises config to TOML with comments matching the SPEC.
fn to_toml_with_comments(cfg: &Config) -> String {
    // Build tools table manually for consistent ordering.
    let mut tools_lines = String::new();
    // Ensure canonical ordering.
    for key in ["claude-code", "copilot"] {
        if let Some(val) = cfg.tools.get(key) {
            tools_lines.push_str(&format!("{key} = \"{val}\"\n"));
        }
    }
    for (k, v) in &cfg.tools {
        if k != "claude-code" && k != "copilot" {
            tools_lines.push_str(&format!("{k} = \"{v}\"\n"));
        }
    }

    format!(
        "[server]\n\
         port = {port}\n\
         bind = \"{bind}\"\n\
         \n\
         [auth]\n\
         # Auto-generated. Run `dispatch regenerate-psk` to rotate.\n\
         psk = \"{psk}\"\n\
         \n\
         [terminal]\n\
         scrollback_lines = {scrollback}\n\
         # Maximum concurrent agents. 4-26, in multiples of 4 (one page per 4 agents).\n\
         max_agents = {max_agents}\n\
         \n\
         [beads]\n\
         # Working directory for bd commands. Defaults to cwd.\n\
         project_dir = \"{project_dir}\"\n\
         \n\
         [tools]\n\
         {tools}",
        port = cfg.server.port,
        bind = cfg.server.bind,
        psk = cfg.auth.psk,
        scrollback = cfg.terminal.scrollback_lines,
        max_agents = cfg.terminal.max_agents,
        project_dir = cfg.beads.project_dir,
        tools = tools_lines,
    )
}
