use dispatch_core::protocol::NATO_DEFAULTS;
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
    /// Deprecated: use [agents].callsigns instead. Kept for backward compatibility.
    #[serde(default)]
    pub max_agents: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentsConfig {
    pub callsigns: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IdentityConfig {
    /// Display name for the user (shown in chat log and used in prompts).
    /// Default: "Dispatch".
    #[serde(default = "default_user_callsign")]
    pub user_callsign: String,
    /// Display name for the console/orchestrator (shown in chat log and used in prompts).
    /// Default: "Console".
    #[serde(default = "default_console_name")]
    pub console_name: String,
}

fn default_user_callsign() -> String {
    "Dispatch".to_string()
}

fn default_console_name() -> String {
    "Console".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// How agents finalize their work: "pr" (push branch + create PR, default)
    /// or "merge" (merge to main + push).
    #[serde(default = "default_merge_strategy")]
    pub merge_strategy: String,
}

fn default_merge_strategy() -> String {
    "pr".to_string()
}

fn default_workflow() -> WorkflowConfig {
    WorkflowConfig {
        merge_strategy: default_merge_strategy(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub agents: Option<AgentsConfig>,
    #[serde(default = "default_identity")]
    pub identity: IdentityConfig,
    #[serde(default = "default_workflow")]
    pub workflow: WorkflowConfig,
    pub tools: HashMap<String, String>,
}

fn default_identity() -> IdentityConfig {
    IdentityConfig {
        user_callsign: default_user_callsign(),
        console_name: default_console_name(),
    }
}

impl Config {
    /// Returns the configured default tool key (e.g. "claude" or "copilot").
    /// Falls back to "claude" if not set.
    pub fn default_tool_key(&self) -> &str {
        self.tools
            .get("ai_agent")
            .map(|s| s.as_str())
            .unwrap_or("claude")
    }

    /// Returns the configured merge strategy: "pr" or "merge".
    pub fn merge_strategy(&self) -> &str {
        &self.workflow.merge_strategy
    }

    /// Effective callsign list. Uses [agents].callsigns if present,
    /// otherwise generates NATO names from terminal.max_agents for
    /// backward compatibility.
    pub fn callsigns(&self) -> Vec<String> {
        if let Some(agents) = &self.agents {
            agents.callsigns.clone()
        } else {
            let n = self.terminal.max_agents.unwrap_or(8) as usize;
            NATO_DEFAULTS[..n.min(NATO_DEFAULTS.len())]
                .iter()
                .map(|s| s.to_string())
                .collect()
        }
    }
}

fn default_callsigns() -> Vec<String> {
    NATO_DEFAULTS.iter().map(|s| s.to_string()).collect()
}

impl Default for Config {
    fn default() -> Self {
        let psk = generate_psk();
        let mut tools = HashMap::new();
        tools.insert("ai_agent".to_string(), "claude".to_string());
        tools.insert("claude".to_string(), "claude".to_string());
        tools.insert("copilot".to_string(), "copilot".to_string());

        Config {
            server: ServerConfig {
                port: 9800,
                bind: "0.0.0.0".to_string(),
            },
            auth: AuthConfig { psk },
            terminal: TerminalConfig {
                scrollback_lines: 1000,
                max_agents: None,
            },
            agents: Some(AgentsConfig {
                callsigns: default_callsigns(),
            }),
            identity: default_identity(),
            workflow: default_workflow(),
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
    // Default tool selection first.
    let default_tool = cfg.default_tool_key();
    tools_lines.push_str(&format!("# Which tool to use by default when dispatching agents: \"claude\" or \"copilot\".\n"));
    tools_lines.push_str(&format!("ai_agent = \"{default_tool}\"\n"));
    // Ensure canonical ordering for tool commands.
    for key in ["claude", "copilot"] {
        if let Some(val) = cfg.tools.get(key) {
            tools_lines.push_str(&format!("{key} = \"{val}\"\n"));
        }
    }
    for (k, v) in &cfg.tools {
        if k != "claude" && k != "copilot" && k != "ai_agent" {
            tools_lines.push_str(&format!("{k} = \"{v}\"\n"));
        }
    }

    // Build callsigns array.
    let callsigns = cfg.callsigns();
    let callsigns_str = callsigns
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");

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
         \n\
         [agents]\n\
         # Agent names, in slot order. The number of entries determines the slot count.\n\
         # Pages are allocated automatically (4 slots per page).\n\
         callsigns = [{callsigns}]\n\
         \n\
         [identity]\n\
         # Display name for the user (shown in chat log and used in prompts).\n\
         user_callsign = \"{user_callsign}\"\n\
         # Display name for the console/orchestrator.\n\
         console_name = \"{console_name}\"\n\
         \n\
         [workflow]\n\
         # How agents finalize work: \"pr\" (push branch + create PR, default) or \"merge\" (merge to main + push).\n\
         merge_strategy = \"{merge_strategy}\"\n\
         \n\
         [tools]\n\
         {tools}",
        port = cfg.server.port,
        bind = cfg.server.bind,
        psk = cfg.auth.psk,
        scrollback = cfg.terminal.scrollback_lines,
        callsigns = callsigns_str,
        user_callsign = cfg.identity.user_callsign,
        console_name = cfg.identity.console_name,
        merge_strategy = cfg.workflow.merge_strategy,
        tools = tools_lines,
    )
}
