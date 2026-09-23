package webgateway

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"time"

	"github.com/codemoo/hmux/internal/filestage"
	"github.com/codemoo/hmux/internal/home"
)

var (
	homeFileStageRoot   = filestage.DefaultRoot
	homeFileStageVerify = home.VerifyFileStageSession
	homeFileStageSweep  = filestage.SweepExpired
)

const webFileStageTTL = 3 * time.Hour

func runHomeUpload(ctx context.Context, p *peer, header filestage.Header, input <-chan Message) {
	fail := func() {
		_ = p.send(ctx, Message{Type: "upload-error", ID: header.RequestID, Error: "Home upload unavailable"})
	}
	root, err := homeFileStageRoot()
	if err != nil {
		fail()
		return
	}
	reader, writer := io.Pipe()
	var response bytes.Buffer
	receiveDone := make(chan error, 1)
	go func() {
		err := filestage.ReceiveWithTTL(ctx, root, reader, &response, homeFileStageVerify, func() time.Time { return time.Now().UTC() }, webFileStageTTL)
		_ = reader.CloseWithError(err)
		receiveDone <- err
	}()
	receiverFinished := false
	waitReceiver := func() error {
		if !receiverFinished {
			err = <-receiveDone
			receiverFinished = true
		}
		return err
	}
	defer func() {
		if !receiverFinished {
			_ = writer.CloseWithError(context.Canceled)
			_ = reader.CloseWithError(context.Canceled)
			_ = waitReceiver()
		}
	}()
	if err := filestage.WriteHeader(ctx, writer, header); err != nil {
		_ = writer.CloseWithError(err)
		fail()
		return
	}
	if p.send(ctx, Message{Type: "upload-ready", ID: header.RequestID}) != nil {
		_ = writer.CloseWithError(errors.New("gateway disconnected"))
		return
	}
	var received int64
	for {
		select {
		case <-ctx.Done():
			_ = writer.CloseWithError(ctx.Err())
			return
		case err := <-receiveDone:
			receiverFinished = true
			_ = writer.CloseWithError(err)
			fail()
			return
		case message := <-input:
			switch message.Type {
			case "upload-data":
				if int64(len(message.Data)) > header.TotalBytes-received {
					_ = writer.CloseWithError(errors.New("upload exceeds declared size"))
					fail()
					return
				}
				n, writeErr := writer.Write(message.Data)
				if writeErr != nil || n != len(message.Data) {
					_ = writer.CloseWithError(writeErr)
					fail()
					return
				}
				received += int64(n)
				if p.send(ctx, Message{Type: "upload-ack", ID: header.RequestID, Received: received}) != nil {
					_ = writer.CloseWithError(errors.New("gateway disconnected"))
					return
				}
			case "upload-finish":
				if received != header.TotalBytes {
					_ = writer.CloseWithError(errors.New("upload is incomplete"))
					fail()
					return
				}
				if err := writer.Close(); err != nil {
					fail()
					return
				}
				if err := waitReceiver(); err != nil || response.Len() < 1 || response.Len() > filestage.MaximumResponseBytes {
					fail()
					return
				}
				_ = p.send(ctx, Message{Type: "upload-complete", ID: header.RequestID, Payload: append(json.RawMessage(nil), response.Bytes()...)})
				return
			}
		}
	}
}

func runHomeFileStageSweeper(ctx context.Context, root string, interval time.Duration, now func() time.Time) {
	if interval <= 0 || now == nil {
		return
	}
	sweep := func() {
		sweepCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
		_ = homeFileStageSweep(sweepCtx, root, now().UTC())
		cancel()
	}
	sweep()
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			sweep()
		}
	}
}
