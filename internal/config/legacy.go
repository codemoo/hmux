package config

// These decode-only records let existing Home inventories load without keeping
// SSH provisioning or remote-client execution in the active application.
type legacyClient struct {
	ID        string   `toml:"id"`
	Role      string   `toml:"role"`
	Hostnames []string `toml:"hostnames"`
}
type legacyIdentityRef struct {
	ID   string `toml:"id"`
	Path string `toml:"path"`
}
type legacyHost struct {
	ID                  string   `toml:"id"`
	SSHAlias            string   `toml:"ssh_alias"`
	Address             string   `toml:"address"`
	User                string   `toml:"user"`
	Port                int      `toml:"port"`
	ProxyJump           string   `toml:"proxy_jump"`
	IdentityRef         string   `toml:"identity_ref"`
	Tags                []string `toml:"tags"`
	ServerAliveInterval int      `toml:"server_alive_interval"`
	ServerAliveCountMax int      `toml:"server_alive_count_max"`
	ConnectTimeout      int      `toml:"connect_timeout"`
}
