package wire

import (
	"os"
	"strings"
	"time"
)

// CurrentProducer reads producer metadata from environment with hostname /
// local TZ as fallbacks. Mirrors Swift ProducerInfo.current().
func CurrentProducer() ProducerInfo {
	id := strings.TrimSpace(os.Getenv("TOKEN_USAGE_PRODUCER_ID"))
	if id == "" {
		host, err := os.Hostname()
		if err != nil || host == "" {
			id = "unknown-producer"
		} else {
			id = host
		}
	}
	tz := strings.TrimSpace(os.Getenv("TOKEN_USAGE_TZ"))
	if tz == "" {
		// time.Local.String() returns the IANA name ("Asia/Seoul") on
		// Unix-like systems when TZ is set or when /etc/localtime resolves.
		tz = time.Local.String()
		if tz == "Local" || tz == "" {
			// Fallback to UTC if Go can't introspect the host TZ name.
			tz = "UTC"
		}
	}
	return ProducerInfo{ID: id, TimeZone: tz}
}
