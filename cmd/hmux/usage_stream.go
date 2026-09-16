package main

import (
	"context"
	"io"
	"os"

	"github.com/codemoo/hmux/internal/client"
	"github.com/codemoo/hmux/internal/config"
	usagestream "github.com/codemoo/token-terrier/server-go/stream"
)

func runForegroundAppUsageStream(cfg config.ClientConfig, writer io.Writer) error {
	ctx, stopParentWatch, err := usageAppParentContext(context.Background(), os.Getppid)
	if err != nil {
		return err
	}
	defer stopParentWatch()
	return runAppUsageStream(ctx, cfg, writer)
}

func runAppUsageStream(ctx context.Context, cfg config.ClientConfig, writer io.Writer) error {
	supported, err := client.UsageStreamSupported(ctx, cfg)
	if err != nil {
		return err
	}
	if !supported {
		encoder, encodeErr := usagestream.NewEncoder(writer)
		if encodeErr != nil {
			return encodeErr
		}
		return encoder.Encode(usagestream.Frame{
			ProtocolVersion: usagestream.ProtocolVersion,
			Sequence:        1,
			Type:            "status",
			Code:            "home_agent_update_required",
		})
	}
	return client.StreamUsage(ctx, cfg, writer)
}
