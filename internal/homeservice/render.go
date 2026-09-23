// Package homeservice manages the native Home connector as an opt-in user service.
package homeservice

import (
	"bytes"
	"encoding/xml"
	"fmt"
	"sort"
	"strings"
)

const Label = "io.github.codemoo.hmux.home"
const Unit = "hmux-home.service"

type Spec struct {
	Binary, Home, Endpoint, TokenFile, ConfigFile, LogFile string
	Environment                                            map[string]string
}

func (s Spec) arguments() []string {
	// env execs the connector: no shell or additional resident process. Clearing
	// the manager environment also prevents accidental inheritance of credentials.
	args := []string{"/usr/bin/env", "-i"}
	for _, key := range environmentKeys(s.Environment) {
		args = append(args, key+"="+s.Environment[key])
	}
	args = append(args, s.Binary, "connect", "--url", s.Endpoint, "--token-file", s.TokenFile, "--log-file", s.LogFile)
	if s.ConfigFile != "" {
		args = append(args, "--config", s.ConfigFile)
	}
	return args
}

func xmlText(value string) string {
	var output bytes.Buffer
	_ = xml.EscapeText(&output, []byte(value))
	return output.String()
}

func environmentKeys(env map[string]string) []string {
	keys := make([]string, 0, len(env))
	for key := range env {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

func LaunchAgent(s Spec) []byte {
	var out strings.Builder
	out.WriteString("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n")
	fmt.Fprintf(&out, "<key>Label</key><string>%s</string>\n<key>ProgramArguments</key><array>\n", Label)
	for _, arg := range s.arguments() {
		fmt.Fprintf(&out, "<string>%s</string>\n", xmlText(arg))
	}
	fmt.Fprintf(&out, "</array>\n<key>WorkingDirectory</key><string>%s</string>\n", xmlText(s.Home))
	out.WriteString(`<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>10</integer>
<key>ExitTimeOut</key><integer>20</integer>
<key>AbandonProcessGroup</key><true/>
<key>ProcessType</key><string>Background</string>
<key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
</dict></plist>
`)
	return []byte(out.String())
}

// systemd performs specifier expansion even inside quotes. ExecStart also
// expands dollars, whereas Environment does not use shell variable expansion.
func unitQuote(value string, command bool) string {
	value = strings.ReplaceAll(value, "%", "%%")
	if command {
		value = strings.ReplaceAll(value, "$", "$$")
	}
	value = strings.ReplaceAll(value, "\\", "\\\\")
	value = strings.ReplaceAll(value, "\"", "\\\"")
	return "\"" + value + "\""
}

func SystemdUnit(s Spec) []byte {
	var out strings.Builder
	out.WriteString("[Unit]\nDescription=HMux Home connector\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart=")
	for i, arg := range s.arguments() {
		if i > 0 {
			out.WriteByte(' ')
		}
		out.WriteString(unitQuote(arg, true))
	}
	// WorkingDirectory is a scalar path, not an ExecStart argument list.
	// Quotes would become literal path characters; only specifiers expand here.
	fmt.Fprintf(&out, "\nWorkingDirectory=%s\n", strings.ReplaceAll(s.Home, "%", "%%"))
	out.WriteString(`Restart=always
RestartSec=10
TimeoutStopSec=20
KillMode=process
UMask=0077
StandardOutput=null
StandardError=null

[Install]
WantedBy=default.target
`)
	return []byte(out.String())
}
